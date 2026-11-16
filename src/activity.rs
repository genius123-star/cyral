//! Cyral 活动：前台窗口/输入/锁屏采样 → 状态机 → 观察事件持久化。
//!
//! 设计：轮询采样而非 WinEventHook。每 5 秒读取一次前台窗口（应用名/标题）、
//! 距离上次用户输入的空闲时长、锁定状态，用可测试的纯函数状态机维护内存态。
//! 数据库只保存 Cyral 实际完成的观察，不保存用户窗口停留轨迹。

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager};

use crate::storage::{
    delete_expired_cyral_activity_events, load_eligible_activity_themes,
    mark_activity_theme_mentioned, past_cyral_events_for_title, record_activity_theme,
    record_cyral_activity_event, ActivityTheme,
};
use crate::Database;
use crate::Message;

/// 前台窗口的一次采样结果。
#[derive(Clone, Debug, PartialEq)]
pub struct ActivitySample {
    /// 前台进程的可执行文件名（不含路径），如 "WINWORD.EXE"。
    pub app_name: String,
    /// 前台窗口标题（可选；获取失败时为 None）。
    pub title: Option<String>,
    /// 距离上次用户输入的空闲秒数。
    pub idle_secs: u64,
    /// 是否处于锁屏/无前台状态。
    pub locked: bool,
}

/// 一次观察的结构化结果：应用/文件标识/窗口标题 + 摘要，供对话模型如实转述。
/// 只保存结构化摘要，不含任何截图字节。
#[derive(Clone, Debug, PartialEq)]
pub struct Observation {
    pub app_name: String,
    pub file_key: Option<String>,
    pub window_title: Option<String>,
    pub summary: String,
    pub source: String,
    pub captured_at: i64,
}

/// 状态机对一次采样做出的决策。
#[derive(Clone, Debug, PartialEq)]
pub enum ActivityAction {
    /// 继续累计当前活动行。
    Accumulate,
    /// 清空当前内存态并暂停观察。空闲/锁定/休眠时触发。
    EndAndPause,
    /// 更新当前内存态（前台窗口切换）。
    Switch {
        app_name: String,
        title: Option<String>,
    },
    /// 同应用前台窗口标题变化，仅更新内存态标题。
    UpdateTitle(Option<String>),
    /// 忽略本轮（前台的正是 Cyral 自身等）。
    Skip,
}

/// 当前活动的内存态。
#[derive(Clone, Debug)]
pub struct CurrentActivity {
    pub app_name: String,
    pub title: Option<String>,
}

/// 已知会把文件名放进窗口标题的应用后缀（小写匹配，用于抽取文件标识）。
const FILE_TITLE_APP_SUFFIXES: &[&str] = &[
    " - word",
    " - powerpoint",
    " - excel",
    " - notepad",
    " - 记事本",
    " - wordpad",
    " - 写字板",
    " - visual studio code",
    " - wps 文字",
    " - wps 演示",
    " - wps 表格",
];

/// 从窗口标题抽取稳定的文件标识（如 "报告.pptx - PowerPoint" → "报告.pptx"）。
/// 仅对已知把文件名放进标题的桌面应用生效；浏览器页面标题等非文件场景返回 None，
/// 避免把任意窗口标题当成文件。
pub fn extract_file_key(title: Option<&str>) -> Option<String> {
    let title = title?.trim();
    if title.is_empty() {
        return None;
    }
    let lower = title.to_lowercase();
    let mut candidate = title;
    let mut stripped_suffix = false;
    for suffix in FILE_TITLE_APP_SUFFIXES {
        if lower.ends_with(suffix) {
            candidate = title[..title.len() - suffix.len()].trim();
            stripped_suffix = true;
            break;
        }
    }
    if candidate.is_empty() {
        return None;
    }
    // 必须有文件名特征：剥离了已知应用后缀，或标题本身带扩展名。
    // 否则（如浏览器页面标题）不当作文件，避免"旧文件重现"误触发。
    if !stripped_suffix && !looks_like_filename(candidate) {
        return None;
    }
    // 扩展名大小写归一（"报告.PPTX" → "报告.pptx"），文件名主体保持不变。
    Some(normalize_extension(candidate))
}

fn looks_like_filename(name: &str) -> bool {
    match name.rsplit_once('.') {
        Some((stem, ext)) => !stem.is_empty() && !ext.trim().is_empty(),
        None => false,
    }
}

fn normalize_extension(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.trim().is_empty() => {
            format!("{stem}.{}", ext.trim().to_lowercase())
        }
        _ => name.to_string(),
    }
}

/// 主题身份只来自稳定文件名，或标题中明确存在“第 N 集”的剧集名；普通网页不归类。
fn topic_identity(app_name: &str, title: Option<&str>) -> Option<(String, String, String)> {
    if let Some(file_key) = extract_file_key(title) {
        let app = app_name.trim().to_ascii_uppercase();
        return Some((
            format!("file:{app}:{file_key}"),
            "file".to_string(),
            file_key,
        ));
    }
    let title = title?.trim();
    let (prefix, suffix) = title.split_once('第')?;
    let episode = suffix.trim_start();
    let digits = episode.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if digits == 0 || !episode[digits..].starts_with('集') {
        return None;
    }
    let series = prefix
        .trim()
        .trim_matches(|ch: char| matches!(ch, '《' | '》' | '-' | '—' | '–' | '|'))
        .trim();
    (!series.is_empty()).then(|| {
        (
            format!("media:{series}"),
            "media".to_string(),
            series.to_string(),
        )
    })
}

/// 把历史观察压缩成一句可转述的摘要（时间/次数全部来自真实记录）。
fn summarize_activity_history(events: &[crate::storage::CyralActivityEvent]) -> String {
    let title = events
        .first()
        .and_then(|event| event.window_title.as_deref())
        .unwrap_or("该文件");
    let mut records: Vec<String> = events
        .iter()
        .take(3)
        .map(|event| {
            let observed = chrono::DateTime::from_timestamp(event.observed_at, 0)
                .map(|time| time.format("%m-%d %H:%M").to_string())
                .unwrap_or_else(|| event.observed_at.to_string());
            format!("{observed} 观察到：{}", event.summary)
        })
        .collect();
    records.dedup();
    format!(
        "{title}：过去记录到 {} 次观察，最近：{}",
        events.len(),
        records.join("；")
    )
}

/// 空闲超过该秒数视为暂停记录。
pub const IDLE_PAUSE_SECS: u64 = 300;
/// 两次采样间隔超过该秒数视为休眠/挂起，当前行按上次采样截断。
pub const GAP_PAUSE_SECS: u64 = 10;
/// 同一窗口长期驻留达到该秒数时触发一次截图检查。
pub const STAY_CAPTURE_SECS: u64 = 600;
/// 频繁操作累计达到该秒数时触发一次截图检查。
pub const BUSY_CAPTURE_SECS: u64 = 300;
/// 两次截图检查之间的最小间隔，避免高频切换窗口造成截图风暴。
pub const CAPTURE_COOLDOWN_SECS: u64 = 300;
/// 距上次主动开口至少经过该秒数才再次开口。
pub const SPEAK_COOLDOWN_SECS: u64 = 10;
/// 长期驻留类型的冷却倍率（SPEAK_COOLDOWN_SECS × 该倍数）。
pub const STAY_SPEAK_COOLDOWN_MULTIPLIER: u32 = 2;
/// 陪伴开启后每 15 分钟的一次低频主动对话，让用户知道观察仍在运行。
pub const COMPANION_CONFIRM_INTERVAL_SECS: i64 = 900;

/// 截图触发类型，用于主动对话决策器区分处理优先级。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AnalyseKind {
    FirstCapture,
    StayedLong,
    BusyPeriod,
    /// 重新打开了一份过去记录过使用历史的文件（文件标识 + 历史活动会话）。
    ReappearedFile,
}

/// 主动对话决策器的输入上下文。
pub struct DecideContext<'a> {
    pub kind: AnalyseKind,
    pub last_summary: Option<&'a str>,
    pub speaker_seen_secs: Option<u64>,
    pub current_summary: &'a str,
    pub now_unix_secs: i64,
    pub proactive_allowed: bool,
}

/// 主动对话决策结果。
#[derive(Clone, Debug, PartialEq)]
pub enum SpeakDecision {
    DoNotSpeak,
    SpeakAndGood(Speak),
}

/// 主动开口状态。
#[derive(Clone, Debug, PartialEq)]
pub struct Speak {
    pub speaker_last_at: i64,
    pub prev_summary: String,
}

/// 主动对话的跨线程共享状态。
static COMPANION_SPEAKER: Mutex<Option<SpeakerState>> = Mutex::new(None);

struct SpeakerState {
    speaker_last_at: i64,
    prev_summary: String,
}

/// 读取当前主动对话状态。
fn read_speaker_state() -> (Option<i64>, Option<String>) {
    COMPANION_SPEAKER
        .lock()
        .ok()
        .map(|guard| {
            guard
                .as_ref()
                .map(|s| (Some(s.speaker_last_at), Some(s.prev_summary.clone())))
                .unwrap_or((None, None))
        })
        .unwrap_or((None, None))
}

/// 写入主动对话状态（更新上次开口时间与摘要）。
fn write_speaker_state(speaker_last_at: i64, prev_summary: String) {
    if let Ok(mut guard) = COMPANION_SPEAKER.lock() {
        *guard = Some(SpeakerState {
            speaker_last_at,
            prev_summary,
        });
    }
}

/// 判断字符是否为不可见语言标记（中文标点/空格等）。
fn is_visible_char(c: char) -> bool {
    !c.is_whitespace() && !c.is_ascii_punctuation() && !c.is_control() && !is_cjk_punctuation(c)
}

fn is_cjk_punctuation(c: char) -> bool {
    matches!(c, '\u{3000}'..='\u{303F}' | '\u{FF00}'..='\u{FFEF}')
}

/// 摘要指纹：去空白和所有标点（含中文），保留汉字/字母/数字。
fn summary_fingerprint(s: &str) -> String {
    s.chars().filter(|c| is_visible_char(*c)).collect()
}

/// 决定是否主动开口。纯函数，可测试。
pub fn decide_speak(context: &DecideContext) -> SpeakDecision {
    if !context.proactive_allowed {
        return SpeakDecision::DoNotSpeak;
    }
    let summary = context.current_summary.trim();
    if summary.is_empty() {
        return SpeakDecision::DoNotSpeak;
    }
    let fp = summary_fingerprint(summary);
    // 与上一句重复 → 不说话。
    if let Some(prev) = context.last_summary {
        if summary_fingerprint(prev) == fp {
            return SpeakDecision::DoNotSpeak;
        }
    }
    // 冷却期内不开口。
    if let Some(elapsed) = context.speaker_seen_secs {
        let cooldown = match context.kind {
            AnalyseKind::StayedLong => SPEAK_COOLDOWN_SECS * STAY_SPEAK_COOLDOWN_MULTIPLIER as u64,
            _ => SPEAK_COOLDOWN_SECS,
        };
        if elapsed < cooldown {
            return SpeakDecision::DoNotSpeak;
        }
    }
    SpeakDecision::SpeakAndGood(Speak {
        speaker_last_at: context.now_unix_secs,
        prev_summary: summary.to_string(),
    })
}

/// 截图触发状态机的输入与决策。
#[derive(Clone, Debug, PartialEq)]
pub struct CaptureContext {
    pub first_tick: bool,
    pub stayed_secs: u64,
    pub busy_secs: u64,
    pub idle_secs: u64,
    pub since_last_capture: u64,
}

/// 是否应进行一次截图检查。
pub fn should_capture(context: &CaptureContext) -> bool {
    if context.first_tick
        || context.stayed_secs >= STAY_CAPTURE_SECS
        || (context.busy_secs >= BUSY_CAPTURE_SECS && context.idle_secs < 60)
    {
        context.since_last_capture >= CAPTURE_COOLDOWN_SECS
    } else {
        false
    }
}

/// 状态机：根据前一次采样与本次采样决定动作。纯函数，便于单测。
///
/// 规则：
/// - 前台的正是 Cyral 自身（app_name 以 cyral 开头）→ Skip；
/// - 锁屏、或两次采样间隔过大（休眠）、或空闲超阈值 → EndAndPause；
/// - 前台应用变化 → Switch；同应用标题变化 → UpdateTitle；否则 Accumulate。
pub fn update_activity(
    previous: Option<&CurrentActivity>,
    sample: &ActivitySample,
    gap_secs: u64,
) -> ActivityAction {
    if sample.app_name.to_ascii_lowercase().starts_with("cyral") {
        return ActivityAction::Skip;
    }
    if sample.locked || gap_secs > GAP_PAUSE_SECS || sample.idle_secs >= IDLE_PAUSE_SECS {
        return ActivityAction::EndAndPause;
    }
    match previous {
        Some(current) if current.app_name == sample.app_name => {
            if current.title != sample.title {
                ActivityAction::UpdateTitle(sample.title.clone())
            } else {
                ActivityAction::Accumulate
            }
        }
        _ => ActivityAction::Switch {
            app_name: sample.app_name.clone(),
            title: sample.title.clone(),
        },
    }
}

/// 当前系统时间戳（秒）。
pub fn now_unix_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

/// 活动记录已变化的通知：面板在窗口切换/收尾时刷新。
pub fn notify_activity_changed(app: &AppHandle) {
    let _ = app.emit_to("panel", "activity-changed", ());
}

/// 在后台持续采样并写库的跟踪任务。
///
/// 使用独立线程轮询（每 interval_secs 秒），绕开 tokio 的 Send 约束；
/// 线程内无 await，MutexGuard 不会跨等待点。门控：仅当 `CompanionSession`
/// 为 true 时落库；false 时若存在当前行则收尾。
pub fn run_activity_tracker(app: AppHandle, interval_secs: u64) {
    std::thread::spawn(move || {
        let mut current: Option<CurrentActivity> = None;
        let mut last_sample = Instant::now();
        // 截图触发追踪状态：停留/频繁操作/冷却计时。
        let mut stayed_secs: u64 = 0;
        let mut busy_secs: u64 = 0;
        let mut since_last_capture: u64 = u64::MAX;
        let mut first_tick = true;
        let mut was_companion = false;
        // 3 天保留清理的节流：约每小时执行一次，避免频繁 DELETE。
        let mut last_cleanup_at: i64 = 0;

        loop {
            std::thread::sleep(Duration::from_secs(interval_secs));
            let now = Instant::now();
            let gap_secs = now.duration_since(last_sample).as_secs();
            last_sample = now;
            let Some(sample) = activity_sampling::sample_foreground() else {
                continue;
            };
            let companion_state = app
                .try_state::<crate::CompanionSession>()
                .map(|session| {
                    session
                        .0
                        .lock()
                        .map(|state| (state.active, state.last_observation.is_some()))
                        // 锁失败时按"已有观察"处理，避免无谓的密集重试。
                        .unwrap_or((false, true))
                })
                .unwrap_or((false, true));
            let companion_active = companion_state.0;
            // 第一条观察尚未落地时，每个采样拍都重试截图分析（仅视觉模型模式）。
            let observation_ready = companion_state.1;
            // 陪伴从关闭→开启：视为一次"首次"分析，立即触发一次截图检查。
            let fresh_start = companion_active && !was_companion;
            was_companion = companion_active;
            // 首次 20 秒反馈由开启入口的独立任务处理。定期主动搭话从 CompanionState
            // 原子领取到期任务；状态机决定随机下次时间和失败重试，tracker 不再保存局部计时。
            let proactive_generation = if companion_active && !sample.locked {
                app.try_state::<crate::CompanionSession>()
                    .and_then(|session| {
                        session.0.lock().ok().and_then(|mut state| {
                            crate::claim_due_proactive(&mut state, now_unix_secs())
                        })
                    })
            } else {
                None
            };
            if let Some(generation) = proactive_generation {
                let _ = app.emit_to("panel", "mode-changed", ());
                spawn_confirm_speak(&app, &sample, generation);
            }

            let action = update_activity(current.as_ref(), &sample, gap_secs);
            let changed = matches!(
                action,
                ActivityAction::Switch { .. } | ActivityAction::EndAndPause
            );
            // 前台是否正是 Cyral 自己（用户在对话/面板中）：观察保留最近的非 Cyral 应用。
            let foreground_is_cyral = sample.app_name.to_ascii_lowercase().starts_with("cyral");
            if let Some(session) = app.try_state::<crate::CompanionSession>() {
                if let Ok(mut state) = session.0.lock() {
                    state.user_talking_to_cyral = foreground_is_cyral;
                }
            }

            // 更新触发计时：切换/暂停重置停留与频繁操作；否则累计。
            match &action {
                ActivityAction::Switch { .. } => {
                    stayed_secs = 0;
                    busy_secs = 0;
                }
                ActivityAction::EndAndPause => {
                    stayed_secs = 0;
                    busy_secs = 0;
                }
                ActivityAction::Accumulate | ActivityAction::UpdateTitle(_) => {
                    if !sample.locked && sample.idle_secs < 60 {
                        stayed_secs = stayed_secs.saturating_add(gap_secs);
                        busy_secs = busy_secs.saturating_add(gap_secs);
                    } else {
                        // 空闲/锁屏时暂停累计。
                        stayed_secs = 0;
                        busy_secs = 0;
                    }
                }
                ActivityAction::Skip => {}
            }
            // 冷却计时累计（无论如何都增长，直到再次截图检查）。
            since_last_capture = since_last_capture.saturating_add(gap_secs);

            // 首次/陪伴刚开启时触发一次截图检查。
            let first_capture = first_tick || fresh_start;
            first_tick = false;

            let Some(database) = app.try_state::<Database>() else {
                continue;
            };
            let Ok(connection) = database.0.try_lock() else {
                // ponytail: 抢不到锁就跳过本轮，避免阻塞 send_message 等主路径。
                if first_capture || stayed_secs >= STAY_CAPTURE_SECS {
                    since_last_capture = 0;
                }
                continue;
            };
            // 视觉模型是否启用：未启用时观察只走窗口元数据，不触发截图。
            let vision_enabled = crate::storage::load_vision_provider(&connection)
                .map(|provider| provider.enabled && !provider.base_url.is_empty())
                .unwrap_or(false);
            // Cyral 观察记录保留 30 天（时间闸门，约每小时）。
            let now_unix = now_unix_secs();
            if now_unix - last_cleanup_at >= 3600 {
                let _ = delete_expired_cyral_activity_events(&connection, now_unix);
                last_cleanup_at = now_unix;
            }

            // "旧文件重现"的历史召回：只查询 Cyral 过去实际完成的观察。
            let mut file_history: Option<String> = None;

            match action {
                ActivityAction::Accumulate => {
                    // 主题时长来自真实前台采样：陪伴开启、非锁屏且用户仍在输入。
                    if companion_active && !sample.locked && sample.idle_secs < 60 {
                        if let Some((identity_key, kind, display_name)) =
                            topic_identity(&sample.app_name, sample.title.as_deref())
                        {
                            let _ = record_activity_theme(
                                &connection,
                                &identity_key,
                                &kind,
                                &display_name,
                                now_unix,
                                gap_secs as i64,
                            );
                        }
                    }
                }
                ActivityAction::UpdateTitle(title) => {
                    if companion_active {
                        if let Some(current) = current.as_mut() {
                            current.title = title.clone();
                        }
                    }
                }
                ActivityAction::Switch { app_name, title } => {
                    if companion_active {
                        let app_for_history = app_name.clone();
                        let title_for_history = title.clone();
                        current = Some(CurrentActivity { app_name, title });
                        if let Some(title_text) = title_for_history.as_deref() {
                            if let Ok(past) = past_cyral_events_for_title(
                                &connection,
                                &app_for_history,
                                title_text,
                                3,
                            ) {
                                if !past.is_empty() {
                                    file_history = Some(summarize_activity_history(&past));
                                }
                            }
                        }
                        // 视觉模式下，切换窗口时先用元数据保鲜最近观察，视觉分析成功后再覆盖。
                        if vision_enabled {
                            write_window_observation(&app, &sample);
                        }
                    } else {
                        current = None;
                    }
                }
                ActivityAction::EndAndPause => {
                    current = None;
                }
                ActivityAction::Skip => {}
            }
            drop(connection);
            // 无视觉模型：每个采样拍直接以窗口元数据更新最近观察，不依赖截图。
            // 前台是 Cyral 自己（Skip）时不覆盖，保留最近的非 Cyral 应用。
            if should_write_metadata_observation(
                companion_active,
                vision_enabled,
                sample.locked,
                foreground_is_cyral,
            ) {
                let observation = write_window_observation(&app, &sample);
                if changed || fresh_start {
                    if let Some(observation) = observation.as_ref() {
                        persist_observation(&app, observation);
                    }
                }
            }

            // 截图检查：仅陪伴模式且视觉模型启用时触发；Skip（前台是 Cyral 自己）不阻断本轮，
            // 这样用户在聊天框问"能否看到屏幕"时也能产生观察。
            let reappeared_file = companion_active && file_history.is_some();
            let should_capture = should_capture_check(
                companion_active,
                observation_ready,
                vision_enabled,
                reappeared_file,
                &CaptureContext {
                    first_tick: first_capture,
                    stayed_secs,
                    busy_secs,
                    idle_secs: sample.idle_secs,
                    since_last_capture,
                },
            );
            let capture_kind = if reappeared_file {
                AnalyseKind::ReappearedFile
            } else if first_capture {
                AnalyseKind::FirstCapture
            } else if stayed_secs >= STAY_CAPTURE_SECS {
                AnalyseKind::StayedLong
            } else {
                AnalyseKind::BusyPeriod
            };

            // 需要截图分析时，在单独线程/任务中执行，避免阻塞本采样循环。
            if should_capture {
                since_last_capture = 0;
                spawn_screenshot_analysis(
                    &app,
                    &sample,
                    current.as_ref(),
                    first_capture,
                    capture_kind,
                    file_history,
                );
            }
        }
    });
}

/// 截图检查决策（纯函数）：仅陪伴开启且视觉模型启用时截图；
/// 第一条视觉观察尚未落地时每个采样拍都重试，已有观察后按触发条件 + 冷却决定。
/// 无视觉模型时观察由窗口元数据承担，本函数恒返回 false。
pub fn should_capture_check(
    companion_active: bool,
    observation_ready: bool,
    vision_enabled: bool,
    reappeared_file: bool,
    context: &CaptureContext,
) -> bool {
    if !companion_active || !vision_enabled {
        return false;
    }
    if !observation_ready {
        return true; // 第一条观察落地前，每拍重试。
    }
    context.since_last_capture >= CAPTURE_COOLDOWN_SECS
        && (reappeared_file || should_capture(context))
}

/// 以当前前台窗口元数据更新最近观察（短锁），并返回已写入的观察。
fn write_window_observation(app: &AppHandle, sample: &ActivitySample) -> Option<Observation> {
    let observation = observation_from_window(&sample.app_name, sample.title.as_deref());
    if let Some(session) = app.try_state::<crate::CompanionSession>() {
        if let Ok(mut state) = session.0.lock() {
            if state.active {
                state.last_observation = Some(observation.clone());
                return Some(observation);
            }
        }
    }
    None
}

/// 保存一次 Cyral 已完成的观察。截图字节从不进入数据库。
pub(crate) fn persist_observation(app: &AppHandle, observation: &Observation) {
    let Some(database) = app.try_state::<Database>() else {
        return;
    };
    let Ok(connection) = database.0.lock() else {
        return;
    };
    if let Err(error) = record_cyral_activity_event(
        &connection,
        "screen_observation",
        &observation.source,
        &observation.app_name,
        observation.window_title.as_deref(),
        &observation.summary,
        observation.captured_at,
    ) {
        eprintln!("Cyral 观察记录写入失败：{error}");
        return;
    }
    if let Some((identity_key, kind, display_name)) =
        topic_identity(&observation.app_name, observation.window_title.as_deref())
    {
        if let Err(error) = record_activity_theme(
            &connection,
            &identity_key,
            &kind,
            &display_name,
            observation.captured_at,
            0,
        ) {
            eprintln!("Cyral 主题观察聚合失败：{error}");
        }
    }
    drop(connection);
    notify_activity_changed(app);
}

/// 是否用窗口元数据覆盖最近观察：仅陪伴开启、无视觉模型、非锁屏、
/// 且前台不是 Cyral 自己（保留最近的非 Cyral 应用）。
fn should_write_metadata_observation(
    companion_active: bool,
    vision_enabled: bool,
    locked: bool,
    foreground_is_cyral: bool,
) -> bool {
    companion_active && !vision_enabled && !locked && !foreground_is_cyral
}

/// 陪伴模式刚开启时的强制主动对话：观察当前前台（前台是 Cyral 时保留最近的非
/// Cyral 观察），并主动询问用户今天的任务与待办。回答走正常对话链路，待办按
/// 既有确认流程进入待办模块。
#[allow(dead_code)]
const COMPANION_GREETING_PROMPT: &str = "你是桌面宠物希莉尔。陪伴模式刚刚开启，下面是当前观察到的前台窗口信息。\
    请用一句简短自然、像贴心朋友的话，先简单回应当前场景（如果有信息），然后主动询问用户：今天有什么任务或待办需要记下来吗？\
    只输出 <cyral_final>正文</cyral_final>，标签外不得有任何文字，正文不超过 40 个中文字。";

#[allow(dead_code)]
fn spawn_companion_greeting(app: &AppHandle, sample: &ActivitySample) {
    let app = app.clone();
    let sample = sample.clone();
    tauri::async_runtime::spawn(async move {
        eprintln!(
            "[companion] 启动问候触发：app={}，title={:?}",
            sample.app_name, sample.title
        );
        let generation = app
            .try_state::<crate::CompanionSession>()
            .map(|session| crate::companion_stop_generation(&session))
            .unwrap_or(0);
        let summary = observe_for_speak(&app, &sample).await;
        // 检查点：观察期间被双击取消 → 不再开口。
        let chain_alive = app
            .try_state::<crate::CompanionSession>()
            .map(|session| crate::companion_chain_alive(&session, generation))
            .unwrap_or(false);
        if !chain_alive {
            eprintln!("[companion] 启动问候取消：陪伴已停止");
            return;
        }
        let (provider, api_key) = {
            let Some(database) = app.try_state::<Database>() else {
                eprintln!("[companion] 启动问候中止：数据库状态不可用");
                return;
            };
            if crate::cloud_budget_blocked(&database) {
                eprintln!("[companion] 启动问候跳过：本月 API 预算已用完");
                return;
            }
            let Ok(connection) = database.0.lock() else {
                eprintln!("[companion] 启动问候中止：数据库锁竞争");
                return;
            };
            let provider = crate::load_provider_settings(&connection).ok();
            let api_key = keyring::Entry::new("app.cyral.desktop", "active-provider-api-key")
                .ok()
                .and_then(|entry| entry.get_password().ok())
                .unwrap_or_default();
            (provider, api_key)
        };
        let Some(provider) = provider else {
            eprintln!("[companion] 启动问候中止：未配置模型供应商");
            return;
        };
        let text = crate::chat_plain(
            &provider,
            &api_key,
            &[
                Message {
                    role: "system".to_string(),
                    content: COMPANION_GREETING_PROMPT.to_string(),
                    created_at: 0,
                    source: "chat".to_string(),
                },
                Message {
                    role: "user".to_string(),
                    content: summary.clone(),
                    created_at: 0,
                    source: "chat".to_string(),
                },
            ],
        )
        .await;
        let text = match text {
            Ok(text) => text,
            Err(error) => {
                eprintln!("[companion] 启动问候失败：{error}");
                return;
            }
        };
        // 模型调用结束后、emit 前最终检查：请求进行中双击也不发气泡。
        let chain_alive = app
            .try_state::<crate::CompanionSession>()
            .map(|session| crate::companion_chain_alive(&session, generation))
            .unwrap_or(false);
        if !chain_alive {
            eprintln!("[companion] 启动问候取消：模型返回后陪伴已停止，不发出");
            return;
        }
        eprintln!("[companion] 启动问候发出：{text}");
        crate::ensure_pet_visible(&app);
        let _ = app.emit_to("pet", "assistant-message", &text);
        // 写入主动对话冷却，避免截图决策器紧接着再开口。
        write_speaker_state(now_unix_secs(), summary);
    });
}

/// 主动对话共用的观察摘要：前台非 Cyral 时观察前台并写入 `last_observation`；
/// 前台是 Cyral 自己时保留最近的非 Cyral 观察。
async fn observe_for_speak(app: &AppHandle, sample: &ActivitySample) -> String {
    let foreground_is_cyral = sample.app_name.to_ascii_lowercase().starts_with("cyral");
    if foreground_is_cyral {
        let last = app
            .try_state::<crate::CompanionSession>()
            .and_then(|session| {
                session
                    .0
                    .lock()
                    .ok()
                    .and_then(|state| state.last_observation.clone())
            });
        return match last {
            Some(observation) => observation.summary,
            None => "（尚未观察到其他应用，用户正在与 Cyral 对话）".to_string(),
        };
    }
    let Some(database) = app.try_state::<Database>() else {
        return format!("应用：{}", sample.app_name);
    };
    let observation =
        crate::activity::observe_foreground(&sample.app_name, sample.title.as_deref(), &database)
            .await;
    if let Some(session) = app.try_state::<crate::CompanionSession>() {
        if let Ok(mut state) = session.0.lock() {
            if state.active {
                state.last_observation = Some(observation.clone());
            }
        }
    }
    persist_observation(app, &observation);
    observation.summary
}

/// 陪伴开启约 3 分钟后的"确认在工作"一次性搭话提示。
const COMPANION_CONFIRM_PROMPT: &str = "你是桌面宠物希莉尔。下面是当前观察到的前台窗口信息。\
    用一句自然、有温度的话开启聊天，可以结合当前场景留一个轻松的问题；不要机械地说“我注意到你在用”。\
    只能引用给出的事实，不得猜测文件内容、工作目的、情绪或隐私。\
    只输出 <cyral_final>正文</cyral_final>，标签外不得有任何文字，正文不超过 35 个中文字。";

const COMPANION_THEME_PROMPT: &str = "你是桌面宠物希莉尔。下面是一张由本地真实活动记录生成的跨天主题事实卡。\
    用一句像熟悉近况的朋友那样自然的开场：可以提及这件事陪伴了用户几天，再留一个开放、容易回答的问题。\
    不要使用“我注意到你在用”这类机械句式；不得猜测文件内容、工作目标、剧集剧情、用户情绪或任何未给出的信息。\
    只输出 <cyral_final>正文</cyral_final>，标签外不得有任何文字，正文不超过 45 个中文字。";

fn theme_fact(theme: &ActivityTheme) -> String {
    format!(
        "主题类型：{}；主题名称：{}；已在 {} 个不同自然日出现；累计可观测活跃时长：{} 分钟；最近一次出现：{}。",
        if theme.kind == "media" { "剧集/媒体" } else { "文件" },
        theme.display_name,
        theme.active_days,
        theme.total_active_secs / 60,
        theme.last_seen_at,
    )
}

fn proactive_interaction_enabled(app: &AppHandle) -> bool {
    app.try_state::<Database>()
        .and_then(|database| {
            let connection = database.0.lock().ok()?;
            crate::load_privacy_config(&connection).ok()
        })
        .map(|config| config.proactive_interaction)
        .unwrap_or(false)
}
/// 一次性确认主动对话：观察前台 → 生成一句"我在关注你的屏幕"的搭话 → 气泡展示。
/// 仅在陪伴仍开启且未被取消时发出。
fn spawn_confirm_speak(app: &AppHandle, sample: &ActivitySample, generation: u64) {
    let app = app.clone();
    let sample = sample.clone();
    tauri::async_runtime::spawn(async move {
        if !proactive_interaction_enabled(&app) {
            finish_proactive_task(
                &app,
                generation,
                crate::ProactiveOutcome::Skipped("用户已关闭主动关心"),
            );
            return;
        }
        eprintln!("[companion] 定期主动搭话触发：app={}", sample.app_name);
        let summary = observe_for_speak(&app, &sample).await;
        let alive = app
            .try_state::<crate::CompanionSession>()
            .map(|session| crate::companion_chain_alive(&session, generation))
            .unwrap_or(false);
        if !alive {
            eprintln!("[companion] 3 分钟确认搭话取消：陪伴已停止");
            return;
        }
        let (provider, api_key, theme) = {
            let Some(database) = app.try_state::<Database>() else {
                finish_proactive_task(
                    &app,
                    generation,
                    crate::ProactiveOutcome::Retryable("数据库状态不可用"),
                );
                return;
            };
            if crate::cloud_budget_blocked(&database) {
                finish_proactive_task(
                    &app,
                    generation,
                    crate::ProactiveOutcome::Skipped("本月 API 预算已用完"),
                );
                return;
            }
            let Ok(connection) = database.0.lock() else {
                finish_proactive_task(
                    &app,
                    generation,
                    crate::ProactiveOutcome::Retryable("数据库繁忙"),
                );
                return;
            };
            let provider = crate::load_provider_settings(&connection).ok();
            let api_key = keyring::Entry::new("app.cyral.desktop", "active-provider-api-key")
                .ok()
                .and_then(|entry| entry.get_password().ok())
                .unwrap_or_default();
            let theme = load_eligible_activity_themes(&connection, now_unix_secs())
                .ok()
                .and_then(|themes| themes.into_iter().next());
            (provider, api_key, theme)
        };
        let Some(provider) = provider else {
            finish_proactive_task(
                &app,
                generation,
                crate::ProactiveOutcome::Skipped("未配置对话模型"),
            );
            return;
        };
        let text = crate::chat_plain(
            &provider,
            &api_key,
            &[
                Message {
                    role: "system".to_string(),
                    content: theme
                        .as_ref()
                        .map(|_| COMPANION_THEME_PROMPT)
                        .unwrap_or(COMPANION_CONFIRM_PROMPT)
                        .to_string(),
                    created_at: 0,
                    source: "chat".to_string(),
                },
                Message {
                    role: "user".to_string(),
                    content: theme
                        .as_ref()
                        .map(theme_fact)
                        .unwrap_or_else(|| summary.clone()),
                    created_at: 0,
                    source: "chat".to_string(),
                },
            ],
        )
        .await;
        let text = match text {
            Ok(text) => text,
            Err(error) => {
                eprintln!("[companion] 定期主动搭话失败：{error}");
                finish_proactive_task(
                    &app,
                    generation,
                    crate::ProactiveOutcome::Retryable("模型请求或最终正文校验失败"),
                );
                return;
            }
        };
        let alive = app
            .try_state::<crate::CompanionSession>()
            .map(|session| crate::companion_chain_alive(&session, generation))
            .unwrap_or(false);
        if !alive {
            eprintln!("[companion] 3 分钟确认搭话取消：模型返回后陪伴已停止");
            return;
        }
        // 先持久化，再显示：对话记录中的正文必须与用户气泡看到的内容完全一致。
        let now = now_unix_secs();
        let persist_result = app
            .try_state::<Database>()
            .ok_or_else(|| "数据库状态不可用".to_string())
            .and_then(|database| {
                let connection = database.0.lock().map_err(|error| error.to_string())?;
                crate::save_message_with_source(&connection, "assistant", &text, "proactive")
                    .map_err(|error| error.to_string())?;
                if let Some(theme) = theme.as_ref() {
                    mark_activity_theme_mentioned(&connection, &theme.identity_key, now)
                        .map_err(|error| error.to_string())?;
                }
                Ok::<(), String>(())
            });
        if let Err(error) = persist_result {
            eprintln!("[companion] 主动消息未持久化：{error}");
            finish_proactive_task(
                &app,
                generation,
                crate::ProactiveOutcome::Retryable("主动消息保存失败"),
            );
            return;
        }
        eprintln!("[companion] 定期主动搭话发出：{text}");
        crate::ensure_pet_visible(&app);
        let _ = app.emit_to("pet", "assistant-message", &text);
        write_speaker_state(now, summary);
        finish_proactive_task(&app, generation, crate::ProactiveOutcome::Success);
    });
}

fn finish_proactive_task(app: &AppHandle, generation: u64, outcome: crate::ProactiveOutcome) {
    if let Some(session) = app.try_state::<crate::CompanionSession>() {
        if let Ok(mut state) = session.0.lock() {
            crate::finish_proactive(&mut state, generation, outcome);
        }
    }
    let _ = app.emit_to("panel", "mode-changed", ());
}

/// 在独立任务中执行截图 + 视觉分析 + 主动对话。截图在内存完成，立即用于分析后丢弃。
const COMPANION_SPEAK_PROMPT: &str =
    "你是桌面宠物希莉尔。下面是用户当前屏幕的场景摘要，请用一句简短自然的问候贴合场景向用户搭话，\
    像贴心的朋友，不要用'我注意到'这类提醒句式。只输出 <cyral_final>正文</cyral_final>，标签外不得有任何文字，正文不超过 20 个中文字。";

/// "旧文件重现"的主动搭话提示：只能引用给定的历史观察记录，不得编造。
const COMPANION_RECALL_PROMPT: &str = "你是桌面宠物希莉尔。用户重新打开了一份过去记录过使用历史的文件。\
    下面是该文件的历史观察记录和当前屏幕场景摘要，请用一句简短自然的问候贴合场景向用户搭话，像贴心的朋友。\
    可以自然地提起过去观察到的时间、次数或摘要，但只能引用下面给出的历史信息，不得编造。\
    只输出 <cyral_final>正文</cyral_final>，标签外不得有任何文字，正文不超过 30 个中文字。";

fn spawn_screenshot_analysis(
    app: &AppHandle,
    sample: &ActivitySample,
    _current: Option<&CurrentActivity>,
    _first_tick: bool,
    capture_kind: AnalyseKind,
    history: Option<String>,
) {
    let app = app.clone();
    let sample = sample.clone();
    tauri::async_runtime::spawn(async move {
        eprintln!(
            "[companion] 观察分析触发：kind={capture_kind:?}，app={}",
            sample.app_name
        );
        let companion_active = app
            .try_state::<crate::CompanionSession>()
            .map(|s| s.0.lock().map(|s| s.active).unwrap_or(false))
            .unwrap_or(false);
        if !companion_active {
            eprintln!("[companion] 观察分析中止：陪伴已停止，丢弃未完成分析");
            return; // 停止陪伴后丢弃未完成分析。
        }
        // 快照停止代次：之后每次 await 后检查，双击取消时静默中止全链。
        let generation = app
            .try_state::<crate::CompanionSession>()
            .map(|session| crate::companion_stop_generation(&session))
            .unwrap_or(0);
        // 统一观察路径：视觉模型可用时尝试截图增强，否则/失败回退窗口元数据。
        let Some(database) = app.try_state::<Database>() else {
            return;
        };
        let observation = crate::activity::observe_foreground(
            &sample.app_name,
            sample.title.as_deref(),
            &database,
        )
        .await;
        // 检查点：观察（可能含视觉模型请求）期间被双击取消 → 丢弃，不写观察。
        let chain_alive = app
            .try_state::<crate::CompanionSession>()
            .map(|session| crate::companion_chain_alive(&session, generation))
            .unwrap_or(false);
        if !chain_alive {
            eprintln!("[companion] 观察分析取消：陪伴已停止，丢弃观察结果");
            return;
        }
        let summary_text = observation.summary.clone();
        let summary_source = observation.source.clone();
        eprintln!(
            "[companion] 观察完成：source={summary_source}，摘要长度={}",
            summary_text.len()
        );
        // 观察结果回流：观察完成即写（无论是否开口），供对话模型基于事实回答。
        // 短锁、不跨 await；陪伴已停止时不再写入。
        if let Some(session) = app.try_state::<crate::CompanionSession>() {
            if let Ok(mut state) = session.0.lock() {
                if state.active {
                    state.last_observation = Some(observation.clone());
                    eprintln!(
                        "[companion] last_observation 已写入：app={}，source={summary_source}",
                        sample.app_name
                    );
                }
            }
        }

        persist_observation(&app, &observation);

        // 主动对话决策：根据分析类型、冷却时间、重复判断是否开口。
        let (speaker_last_at, last_summary) = read_speaker_state();
        let elapsed = speaker_last_at.map(|last| now_unix_secs().saturating_sub(last) as u64);
        // "旧文件重现"把历史使用摘要并入当前摘要，作为主动搭话的输入。
        let speak_input = if matches!(capture_kind, AnalyseKind::ReappearedFile) {
            match &history {
                Some(history_text) => format!("{history_text}\n{summary_text}"),
                None => summary_text.to_string(),
            }
        } else {
            summary_text.to_string()
        };
        let decision = decide_speak(&DecideContext {
            kind: capture_kind,
            last_summary: last_summary.as_deref(),
            speaker_seen_secs: elapsed,
            current_summary: &speak_input,
            now_unix_secs: now_unix_secs(),
            proactive_allowed: proactive_interaction_enabled(&app),
        });
        match decision {
            SpeakDecision::SpeakAndGood(speak) => {
                // emit 前再查一次链存活，停止/取消后不开口。
                let chain_alive = app
                    .try_state::<crate::CompanionSession>()
                    .map(|session| crate::companion_chain_alive(&session, generation))
                    .unwrap_or(false);
                if !chain_alive {
                    eprintln!("[companion] 主动对话取消：陪伴已停止");
                    return;
                }
                // 取对话模型 provider + key，组织一句话。
                let (speak_provider, speak_key) = {
                    let Some(database) = app.try_state::<Database>() else {
                        return;
                    };
                    if crate::cloud_budget_blocked(&database) {
                        eprintln!("[companion] 主动搭话跳过：本月 API 预算已用完");
                        return;
                    }
                    let Ok(connection) = database.0.lock() else {
                        return;
                    };
                    let provider = crate::load_provider_settings(&connection).ok();
                    let api_key =
                        keyring::Entry::new("app.cyral.desktop", "active-provider-api-key")
                            .ok()
                            .and_then(|entry| entry.get_password().ok())
                            .unwrap_or_default();
                    (provider, api_key)
                };
                let Some(speak_provider) = speak_provider else {
                    return;
                };
                let prompt = if matches!(capture_kind, AnalyseKind::ReappearedFile) {
                    COMPANION_RECALL_PROMPT
                } else {
                    COMPANION_SPEAK_PROMPT
                };
                let Ok(text) = crate::chat_plain(
                    &speak_provider,
                    &speak_key,
                    &[
                        Message {
                            role: "system".to_string(),
                            content: prompt.to_string(),
                            created_at: 0,
                            source: "chat".to_string(),
                        },
                        Message {
                            role: "user".to_string(),
                            content: speak_input.clone(),
                            created_at: 0,
                            source: "chat".to_string(),
                        },
                    ],
                )
                .await
                else {
                    return; // 静默降级。
                };
                // 模型调用结束后、emit 前最终检查：请求进行中双击也不发气泡。
                let chain_alive = app
                    .try_state::<crate::CompanionSession>()
                    .map(|session| crate::companion_chain_alive(&session, generation))
                    .unwrap_or(false);
                if !chain_alive {
                    eprintln!("[companion] 主动对话取消：模型返回后陪伴已停止，不发出");
                    return;
                }
                // 这条事件触发的主动消息也必须先成功落库，不能绕过对话记录。
                let Some(database) = app.try_state::<Database>() else {
                    return;
                };
                let Ok(connection) = database.0.lock() else {
                    return;
                };
                if let Err(error) =
                    crate::save_message_with_source(&connection, "assistant", &text, "proactive")
                {
                    eprintln!("[companion] 场景主动消息未持久化：{error}");
                    return;
                }
                drop(connection);
                crate::ensure_pet_visible(&app);
                let _ = app.emit_to("pet", "assistant-message", &text);
                write_speaker_state(speak.speaker_last_at, speak.prev_summary);
            }
            SpeakDecision::DoNotSpeak => {}
        }
        // 截图字节在此函数作用域结束即释放（不落盘）。
    });
}

// —— 采样实现 ——

#[cfg(windows)]
pub mod activity_sampling {
    use super::ActivitySample;

    use windows::Win32::Foundation::{CloseHandle, HWND};
    use windows::Win32::System::ProcessStatus::K32GetProcessImageFileNameW;
    use windows::Win32::System::SystemInformation::GetTickCount64;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
    };

    fn idle_secs() -> u64 {
        let mut info = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        let ok = unsafe { GetLastInputInfo(&mut info) };
        if !ok.as_bool() {
            return 0;
        }
        let now_tick = unsafe { GetTickCount64() };
        // ponytail: tick 计数为 32 位语义，用 saturating_sub 规避回绕。
        now_tick.saturating_sub(info.dwTime as u64) / 1000
    }

    fn process_name_for(hwnd: HWND) -> Option<String> {
        let mut pid: u32 = 0;
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
        }
        if pid == 0 {
            return None;
        }
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) };
        let handle = match handle {
            Ok(handle) => handle,
            Err(_) => return None,
        };
        let mut buffer = [0u16; 4096];
        let len = unsafe { K32GetProcessImageFileNameW(handle, &mut buffer) };
        unsafe {
            let _ = CloseHandle(handle);
        }
        if len == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&buffer[..len as usize]);
        let file = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_string();
        let cleaned = clean_name(&file);
        (!cleaned.is_empty()).then_some(cleaned)
    }

    fn window_title_for(hwnd: HWND) -> Option<String> {
        let mut buffer = [0u16; 512];
        let len = unsafe { GetWindowTextW(hwnd, &mut buffer) };
        if len == 0 {
            return None;
        }
        let text = String::from_utf16_lossy(&buffer[..len as usize]);
        let cleaned = clean_name(&text);
        (!cleaned.is_empty()).then_some(cleaned)
    }

    /// 过滤控制字符并截断到合理长度。
    fn clean_name(value: &str) -> String {
        value
            .chars()
            .filter(|character| !character.is_control())
            .take(120)
            .collect::<String>()
            .trim()
            .to_string()
    }

    pub fn sample_foreground() -> Option<ActivitySample> {
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.is_invalid() {
            return Some(ActivitySample {
                app_name: String::new(),
                title: None,
                idle_secs: idle_secs(),
                locked: true,
            });
        }
        let app_name = process_name_for(hwnd)?;
        if app_name.eq_ignore_ascii_case("lockapp.exe") {
            return Some(ActivitySample {
                app_name,
                title: None,
                idle_secs: idle_secs(),
                locked: true,
            });
        }
        let title = window_title_for(hwnd);
        Some(ActivitySample {
            app_name,
            title,
            idle_secs: idle_secs(),
            locked: false,
        })
    }
}

#[cfg(not(windows))]
pub mod activity_sampling {
    use super::ActivitySample;
    pub fn sample_foreground() -> Option<ActivitySample> {
        None
    }
}

// —— 单帧桌面截图（内存，不入盘） ——

/// 捕获当前主屏幕的单帧，返回 PNG 编码的内存字节。
/// 原始像素只存在于内存，处理完毕后由调用方丢弃；失败时返回 None。
#[cfg(windows)]
pub fn capture_screen_png() -> Option<Vec<u8>> {
    use windows::Win32::{
        Graphics::Gdi::{
            BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
            GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
            DIB_RGB_COLORS, HGDIOBJ, RGBQUAD, SRCCOPY,
        },
        UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN},
    };

    let width = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let height = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    if width <= 0 || height <= 0 {
        eprintln!("[companion] 截图失败：屏幕尺寸无效（{width}x{height}）");
        return None;
    }
    let width = width as u32;
    let height = height as u32;

    let screen_dc = unsafe { GetDC(None) };
    if screen_dc.is_invalid() {
        eprintln!("[companion] 截图失败：无法获取屏幕 DC");
        return None;
    }
    let mem_dc = unsafe { CreateCompatibleDC(Some(screen_dc)) };
    if mem_dc.is_invalid() {
        let _ = unsafe { ReleaseDC(None, screen_dc) };
        eprintln!("[companion] 截图失败：无法创建兼容 DC");
        return None;
    }
    let bitmap = unsafe { CreateCompatibleBitmap(screen_dc, width as i32, height as i32) };
    if bitmap.is_invalid() {
        let _ = unsafe { DeleteDC(mem_dc) };
        let _ = unsafe { ReleaseDC(None, screen_dc) };
        eprintln!("[companion] 截图失败：无法创建兼容位图（{width}x{height}）");
        return None;
    }
    let old = unsafe { SelectObject(mem_dc, HGDIOBJ(bitmap.0)) };
    let blt = unsafe {
        BitBlt(
            mem_dc,
            0,
            0,
            width as i32,
            height as i32,
            Some(screen_dc),
            0,
            0,
            SRCCOPY,
        )
    };
    let _ = unsafe { SelectObject(mem_dc, old) };
    let _ = unsafe { DeleteDC(mem_dc) };
    let _ = unsafe { ReleaseDC(None, screen_dc) };
    if blt.is_err() {
        let _ = unsafe { DeleteObject(HGDIOBJ(bitmap.0)) };
        eprintln!("[companion] 截图失败：BitBlt 失败");
        return None;
    }

    // 用 GetDIBits 读回 BGRA 像素。
    let mut bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: 0,
            ..Default::default()
        },
        bmiColors: [RGBQUAD::default(); 1],
    };
    let mut buffer = vec![0u8; (width as usize) * (height as usize) * 4];
    let lines = unsafe {
        GetDIBits(
            screen_dc,
            bitmap,
            0,
            height as u32,
            Some(buffer.as_mut_ptr() as *mut core::ffi::c_void),
            &mut bmi,
            DIB_RGB_COLORS,
        )
    };
    let _ = unsafe { DeleteObject(HGDIOBJ(bitmap.0)) };
    if lines == 0 {
        eprintln!("[companion] 截图失败：GetDIBits 返回 0 行");
        return None;
    }

    // ponytail: 用 image crate 把 BGRA 编码为 PNG，内存中完成，不落盘。
    let mut rgba = vec![0u8; buffer.len()];
    for (index, chunk) in buffer.chunks_exact(4).enumerate() {
        rgba[index * 4] = chunk[2];
        rgba[index * 4 + 1] = chunk[1];
        rgba[index * 4 + 2] = chunk[0];
        rgba[index * 4 + 3] = 0xFF;
    }
    let mut output = std::io::Cursor::new(Vec::new());
    let image = image::RgbaImage::from_raw(width, height, rgba)?;
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut output, image::ImageFormat::Png)
        .ok()?;
    Some(output.into_inner())
}

#[cfg(not(windows))]
pub fn capture_screen_png() -> Option<Vec<u8>> {
    None
}

// —— 视觉分析（截图理解） ——

/// 视觉分析的输入：一张截图 PNG + 当前活动的窗口上下文。
pub struct VisualInput {
    pub screenshot_png: Vec<u8>,
    pub app_name: String,
    pub window_title: Option<String>,
}

/// 视觉分析结果：要么得出视觉摘要，要么降级为仅窗口标题/进程名的文本摘要。
#[derive(Debug)]
pub enum VisualSummary {
    /// 视觉模型成功给出场景摘要。
    Visual(String),
    /// 未配置视觉模型或调用失败，降级为纯文本上下文。
    Text(String),
}

/// 视觉分析的 prompt，要求模型只描述屏幕上的场景，不编造。
const VISION_PROMPT: &str = "这是一张用户电脑屏幕的截图。请用简洁的中文描述用户正在做什么：\
    识别主要应用、正在进行的任务类型（如写文档、浏览网页、看视频、代码编辑）、\
    屏幕上的关键可见内容。只描述你确实看到的内容，不要猜测或编造。输出不超过 3 句话。";

/// 根据视觉模型配置分析截图；未配置或失败时自动降级为纯文本摘要。
pub async fn analyze_screenshot(
    provider: Option<&crate::storage::VisionProvider>,
    api_key: &str,
    input: &VisualInput,
) -> VisualSummary {
    let Some(provider) = provider.filter(|p| p.enabled && !p.base_url.is_empty()) else {
        return VisualSummary::Text(text_only_summary(input));
    };
    if api_key.is_empty() {
        return VisualSummary::Text(text_only_summary(input));
    }
    match send_vision_request(provider, api_key, input).await {
        Ok(summary) if !summary.trim().is_empty() => VisualSummary::Visual(summary),
        _ => VisualSummary::Text(text_only_summary(input)),
    }
}

fn text_only_summary(input: &VisualInput) -> String {
    match &input.window_title {
        Some(title) => format!("应用：{}，窗口标题：{}", input.app_name, title),
        None => format!("应用：{}", input.app_name),
    }
}

/// 从窗口元数据构造观察（无截图、无视觉模型）：前台进程名 + 窗口标题。
/// 这是观察的默认来源；截图/视觉只作为增强层覆盖。
pub(crate) fn observation_from_window(app_name: &str, window_title: Option<&str>) -> Observation {
    let summary = match &window_title {
        Some(title) => format!("应用：{app_name}，窗口标题：{title}"),
        None => format!("应用：{app_name}"),
    };
    Observation {
        app_name: app_name.to_string(),
        file_key: extract_file_key(window_title),
        window_title: window_title.map(ToOwned::to_owned),
        summary,
        source: "window".to_string(),
        captured_at: now_unix_secs(),
    }
}

/// 视觉增强观察：仅当视觉模型已配置且 Key 可用时才尝试截图分析；
/// 截图失败或视觉分析失败都返回 None，由调用方回退到窗口元数据观察。
pub(crate) async fn visual_observation_if_available(
    app_name: &str,
    window_title: Option<&str>,
    database: &crate::Database,
) -> Option<Observation> {
    // 预算硬限：视觉请求也计入月度费用，100% 时跳过截图分析（回退窗口元数据）。
    if crate::cloud_budget_blocked(database) {
        eprintln!("[companion] 视觉分析跳过：本月 API 预算已用完");
        return None;
    }
    let (provider, api_key) = {
        let Ok(connection) = database.0.lock() else {
            return None;
        };
        let provider = crate::storage::load_vision_provider(&connection).ok();
        let api_key = keyring::Entry::new("app.cyral.desktop", "vision-provider-api-key")
            .ok()
            .and_then(|entry| entry.get_password().ok())
            .unwrap_or_default();
        (provider, api_key)
    };
    let provider = provider.filter(|p| p.enabled && !p.base_url.is_empty())?;
    if api_key.is_empty() {
        return None;
    }
    let png = capture_screen_png()?;
    let input = VisualInput {
        screenshot_png: png,
        app_name: app_name.to_string(),
        window_title: window_title.map(ToOwned::to_owned),
    };
    match analyze_screenshot(Some(&provider), &api_key, &input).await {
        VisualSummary::Visual(text) => Some(Observation {
            app_name: app_name.to_string(),
            file_key: extract_file_key(window_title),
            window_title: window_title.map(ToOwned::to_owned),
            summary: text,
            source: "visual".to_string(),
            captured_at: now_unix_secs(),
        }),
        // 视觉降级（Text）不代表成功：回退窗口元数据。
        VisualSummary::Text(_) => None,
    }
}

/// 观察当前前台窗口：视觉模型可用时尝试截图增强，否则或失败时
/// 返回窗口元数据观察。此函数不再因截图失败而失败。
pub(crate) async fn observe_foreground(
    app_name: &str,
    window_title: Option<&str>,
    database: &crate::Database,
) -> Observation {
    visual_observation_if_available(app_name, window_title, database)
        .await
        .unwrap_or_else(|| observation_from_window(app_name, window_title))
}

/// 单次视觉模型请求：上传截图并返回场景摘要。兼容 chat_completions / responses。
async fn send_vision_request(
    provider: &crate::storage::VisionProvider,
    api_key: &str,
    input: &VisualInput,
) -> Result<String, String> {
    // 用共享客户端发送请求，兼容代理环境。
    let client = crate::shared_http_client(30);
    let base_url = provider.base_url.trim_end_matches('/');
    let b64 = base64_standard(&input.screenshot_png);

    let body = match provider.protocol.as_str() {
        "responses" => serde_json::json!({
            "model": provider.model,
            "input": [{
                "role": "user",
                "content": [
                    {"type": "input_image", "image_url": format!("data:image/png;base64,{b64}")},
                    {"type": "input_text", "text": VISION_PROMPT}
                ]
            }]
        }),
        _ => serde_json::json!({
            "model": provider.model,
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": VISION_PROMPT},
                    {"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{b64}")}}
                ]
            }]
        }),
    };
    let url = if base_url.ends_with("/chat/completions") || base_url.ends_with("/responses") {
        base_url.to_string()
    } else if provider.protocol == "responses" {
        format!("{base_url}/responses")
    } else {
        format!("{base_url}/chat/completions")
    };

    let response = client
        .post(&url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    let json: serde_json::Value = response.json().await.map_err(|error| error.to_string())?;
    if provider.protocol == "responses" {
        json.get("output_text")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned)
            .ok_or_else(|| "视觉模型未返回文本".to_string())
    } else {
        json["choices"][0]["message"]["content"]
            .as_str()
            .map(ToOwned::to_owned)
            .ok_or_else(|| "视觉模型未返回文本".to_string())
    }
}

/// 标准 base64 编码（无依赖，用 image 字节直接编码）。
fn base64_standard(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod vision_tests {
    use super::*;

    #[test]
    fn base64_encodes_correctly() {
        assert_eq!(base64_standard(b""), "");
        assert_eq!(base64_standard(b"f"), "Zg==");
        assert_eq!(base64_standard(b"fo"), "Zm8=");
        assert_eq!(base64_standard(b"foo"), "Zm9v");
    }

    #[test]
    fn text_summary_contains_application_and_title() {
        let input = VisualInput {
            screenshot_png: vec![0],
            app_name: "CHROME.EXE".to_string(),
            window_title: Some("知乎 - 今日热榜".to_string()),
        };
        let s = text_only_summary(&input);
        assert!(s.contains("CHROME.EXE"));
        assert!(s.contains("知乎 - 今日热榜"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(app_name: &str, locked: bool, idle_secs: u64) -> ActivitySample {
        ActivitySample {
            app_name: app_name.to_string(),
            title: None,
            idle_secs,
            locked,
        }
    }

    fn sample_with_title(app_name: &str, title: &str, idle_secs: u64) -> ActivitySample {
        ActivitySample {
            app_name: app_name.to_string(),
            title: Some(title.to_string()),
            idle_secs,
            locked: false,
        }
    }

    fn current(_id: i64, app_name: &str, title: Option<&str>) -> CurrentActivity {
        CurrentActivity {
            app_name: app_name.to_string(),
            title: title.map(|t| t.to_string()),
        }
    }

    // —— 前台切换 ——

    #[test]
    fn switches_from_previous_to_new_application() {
        let prev = current(1, "WINWORD.EXE", None);
        let sample = sample("CHROME.EXE", false, 0);
        let action = update_activity(Some(&prev), &sample, 5);
        assert_eq!(
            action,
            ActivityAction::Switch {
                app_name: "CHROME.EXE".to_string(),
                title: None,
            }
        );
    }

    #[test]
    fn accumulates_the_same_application() {
        let prev = current(1, "WINWORD.EXE", Some("论文.docx"));
        let sample = sample_with_title("WINWORD.EXE", "论文.docx", 10);
        let action = update_activity(Some(&prev), &sample, 5);
        assert_eq!(action, ActivityAction::Accumulate);
    }

    #[test]
    fn updates_title_for_the_same_application() {
        let prev = current(1, "WINWORD.EXE", Some("论文.docx"));
        let sample = sample_with_title("WINWORD.EXE", "报告.docx", 10);
        let action = update_activity(Some(&prev), &sample, 5);
        assert_eq!(
            action,
            ActivityAction::UpdateTitle(Some("报告.docx".to_string()))
        );
    }

    // —— 暂停/结束 ——

    #[test]
    fn end_and_pause_on_lock() {
        let prev = current(1, "WINWORD.EXE", None);
        let sample = sample("WINWORD.EXE", true, 0);
        let action = update_activity(Some(&prev), &sample, 5);
        assert_eq!(action, ActivityAction::EndAndPause);
    }

    #[test]
    fn end_and_pause_on_idle_timeout() {
        let prev = current(1, "WINWORD.EXE", None);
        let sample = sample("WINWORD.EXE", false, IDLE_PAUSE_SECS);
        let action = update_activity(Some(&prev), &sample, 5);
        assert_eq!(action, ActivityAction::EndAndPause);
    }

    #[test]
    fn end_and_pause_on_long_gap() {
        let prev = current(1, "WINWORD.EXE", None);
        let sample = sample("WINWORD.EXE", false, 0);
        let action = update_activity(Some(&prev), &sample, GAP_PAUSE_SECS + 1);
        assert_eq!(action, ActivityAction::EndAndPause);
    }

    // —— 跳过自身 ——

    #[test]
    fn skips_cyral_window() {
        let prev = current(1, "WINWORD.EXE", None);
        let sample = sample("cyral.exe", false, 0);
        let action = update_activity(Some(&prev), &sample, 5);
        assert_eq!(action, ActivityAction::Skip);
    }

    #[test]
    fn skips_cyral_case_insensitive() {
        let sample = sample("Cyral.exe", false, 0);
        let action = update_activity(None, &sample, 5);
        assert_eq!(action, ActivityAction::Skip);
    }

    // —— 无前驱 ——

    #[test]
    fn starts_without_previous() {
        let sample = sample("WINWORD.EXE", false, 0);
        let action = update_activity(None, &sample, 5);
        assert_eq!(
            action,
            ActivityAction::Switch {
                app_name: "WINWORD.EXE".to_string(),
                title: None,
            }
        );
    }

    #[test]
    fn no_previous_with_lock_yields_pause() {
        let sample = sample("", true, 0);
        let action = update_activity(None, &sample, 5);
        assert_eq!(action, ActivityAction::EndAndPause);
    }

    // —— 边界值 ——

    #[test]
    fn idle_equals_threshold_still_counts_as_pause() {
        let prev = current(1, "WINWORD.EXE", None);
        let sample = sample("WINWORD.EXE", false, IDLE_PAUSE_SECS);
        let action = update_activity(Some(&prev), &sample, 5);
        assert_eq!(action, ActivityAction::EndAndPause);
    }

    #[test]
    fn idle_below_threshold_accumulates() {
        let prev = current(1, "WINWORD.EXE", None);
        let sample = sample("WINWORD.EXE", false, IDLE_PAUSE_SECS - 1);
        let action = update_activity(Some(&prev), &sample, 5);
        assert_eq!(action, ActivityAction::Accumulate);
    }

    // —— 截图触发 ——

    fn capture_context(
        first_tick: bool,
        stayed_secs: u64,
        busy_secs: u64,
        idle_secs: u64,
        since_last_capture: u64,
    ) -> CaptureContext {
        CaptureContext {
            first_tick,
            stayed_secs,
            busy_secs,
            idle_secs,
            since_last_capture,
        }
    }

    #[test]
    fn captures_on_first_tick_when_cooldown_elapsed() {
        assert!(should_capture(&capture_context(
            true,
            0,
            0,
            0,
            CAPTURE_COOLDOWN_SECS
        )));
    }

    #[test]
    fn no_capture_before_first_tick_without_other_triggers() {
        assert!(!should_capture(&capture_context(
            false,
            30,
            0,
            10,
            CAPTURE_COOLDOWN_SECS
        )));
    }

    #[test]
    fn captures_after_long_stay() {
        assert!(should_capture(&capture_context(
            false,
            STAY_CAPTURE_SECS,
            0,
            10,
            CAPTURE_COOLDOWN_SECS
        )));
    }

    #[test]
    fn captures_when_busy_long_enough_and_not_idle() {
        assert!(should_capture(&capture_context(
            false,
            0,
            BUSY_CAPTURE_SECS,
            20,
            CAPTURE_COOLDOWN_SECS
        )));
    }

    #[test]
    fn does_not_capture_busy_when_idle() {
        assert!(!should_capture(&capture_context(
            false,
            0,
            BUSY_CAPTURE_SECS,
            120,
            CAPTURE_COOLDOWN_SECS
        )));
    }

    #[test]
    fn does_not_capture_within_cooldown() {
        assert!(!should_capture(&capture_context(
            true,
            0,
            0,
            0,
            CAPTURE_COOLDOWN_SECS - 1
        )));
    }

    // —— 截图检查决策（首次观察落地前持续重试） ——

    #[test]
    fn retries_capture_until_first_observation_lands() {
        assert!(should_capture_check(
            true,
            false,
            true,
            false,
            &capture_context(false, 0, 0, 10, 0)
        ));
        assert!(should_capture_check(
            true,
            false,
            true,
            true,
            &capture_context(false, 0, 0, 10, 0)
        ));
    }

    #[test]
    fn no_capture_when_vision_disabled_even_without_observation() {
        assert!(!should_capture_check(
            true,
            false,
            false,
            false,
            &capture_context(true, 0, 0, 10, u64::MAX)
        ));
    }

    #[test]
    fn no_capture_when_companion_off_even_without_observation() {
        assert!(!should_capture_check(
            false,
            false,
            true,
            false,
            &capture_context(true, 0, 0, 10, u64::MAX)
        ));
    }

    #[test]
    fn observation_ready_respects_cooldown() {
        assert!(!should_capture_check(
            true,
            true,
            true,
            false,
            &capture_context(false, 0, 0, 10, CAPTURE_COOLDOWN_SECS - 1)
        ));
    }

    #[test]
    fn observation_ready_first_capture_with_cooldown_elapsed() {
        assert!(should_capture_check(
            true,
            true,
            true,
            false,
            &capture_context(true, 0, 0, 10, CAPTURE_COOLDOWN_SECS)
        ));
    }

    #[test]
    fn observation_ready_reappeared_file_with_cooldown_elapsed() {
        assert!(should_capture_check(
            true,
            true,
            true,
            true,
            &capture_context(false, 0, 0, 10, CAPTURE_COOLDOWN_SECS)
        ));
    }

    // —— 窗口元数据观察 ——

    #[test]
    fn window_observation_uses_metadata_and_window_source() {
        let observation = observation_from_window("POWERPNT.EXE", Some("报告.pptx - PowerPoint"));
        assert_eq!(observation.source, "window");
        assert_eq!(observation.app_name, "POWERPNT.EXE");
        assert_eq!(observation.file_key.as_deref(), Some("报告.pptx"));
        assert_eq!(
            observation.summary,
            "应用：POWERPNT.EXE，窗口标题：报告.pptx - PowerPoint"
        );
    }

    #[test]
    fn window_observation_without_title_keeps_app_only() {
        let observation = observation_from_window("WINWORD.EXE", None);
        assert_eq!(observation.summary, "应用：WINWORD.EXE");
        assert_eq!(observation.file_key, None);
    }

    #[test]
    fn metadata_observation_skips_cyral_foreground() {
        assert!(should_write_metadata_observation(true, false, false, false));
        assert!(!should_write_metadata_observation(true, false, false, true));
        assert!(!should_write_metadata_observation(true, true, false, false));
        assert!(!should_write_metadata_observation(true, false, true, false));
        assert!(!should_write_metadata_observation(
            false, false, false, false
        ));
    }

    #[test]
    fn companion_greeting_asks_about_today_tasks_and_todos() {
        assert!(COMPANION_GREETING_PROMPT.contains("任务或待办"));
        assert!(COMPANION_GREETING_PROMPT.contains("40"));
    }

    #[test]
    fn confirm_prompt_requires_a_warm_fact_based_opening() {
        assert_eq!(COMPANION_CONFIRM_INTERVAL_SECS, 900);
        assert!(COMPANION_CONFIRM_PROMPT.contains("自然、有温度"));
        assert!(COMPANION_CONFIRM_PROMPT.contains("不得猜测文件内容"));
        assert!(COMPANION_CONFIRM_PROMPT.contains("<cyral_final>"));
    }
}

#[cfg(test)]
mod speaker_tests {
    use super::*;

    fn context(
        kind: AnalyseKind,
        last_summary: Option<&'static str>,
        speaker_seen_secs: Option<u64>,
        current_summary: &'static str,
        now_unix_secs: i64,
    ) -> DecideContext<'static> {
        DecideContext {
            kind,
            last_summary,
            speaker_seen_secs,
            current_summary,
            now_unix_secs,
            proactive_allowed: true,
        }
    }

    #[test]
    fn empty_summary_yields_do_not_speak() {
        let r = decide_speak(&context(AnalyseKind::FirstCapture, None, None, "", 1000));
        assert_eq!(r, SpeakDecision::DoNotSpeak);
    }

    #[test]
    fn disabled_proactive_interaction_yields_do_not_speak() {
        let mut input = context(
            AnalyseKind::BusyPeriod,
            Some("看视频"),
            Some(SPEAK_COOLDOWN_SECS + 1),
            "查看网页",
            1000,
        );
        input.proactive_allowed = false;

        assert_eq!(decide_speak(&input), SpeakDecision::DoNotSpeak);
    }
    #[test]
    fn same_fingerprint_as_last_yields_do_not_speak() {
        let r = decide_speak(&context(
            AnalyseKind::BusyPeriod,
            Some("编辑文档"),
            Some(SPEAK_COOLDOWN_SECS + 1),
            "编辑文档",
            1000,
        ));
        assert_eq!(r, SpeakDecision::DoNotSpeak);
    }

    #[test]
    fn within_cooldown_yields_do_not_speak() {
        let r = decide_speak(&context(
            AnalyseKind::BusyPeriod,
            Some("看视频"),
            Some(SPEAK_COOLDOWN_SECS - 1),
            "查看网页",
            1000,
        ));
        assert_eq!(r, SpeakDecision::DoNotSpeak);
    }

    #[test]
    fn cooled_down_and_different_yields_speak() {
        let r = decide_speak(&context(
            AnalyseKind::BusyPeriod,
            Some("看视频"),
            Some(SPEAK_COOLDOWN_SECS + 1),
            "查看网页",
            1000,
        ));
        assert_eq!(
            r,
            SpeakDecision::SpeakAndGood(Speak {
                speaker_last_at: 1000,
                prev_summary: "查看网页".to_string(),
            })
        );
    }

    #[test]
    fn stayed_long_has_extended_cooldown() {
        let r = decide_speak(&context(
            AnalyseKind::StayedLong,
            Some("看视频"),
            Some(SPEAK_COOLDOWN_SECS * STAY_SPEAK_COOLDOWN_MULTIPLIER as u64 - 1),
            "查看网页",
            1000,
        ));
        assert_eq!(r, SpeakDecision::DoNotSpeak);
    }

    #[test]
    fn stayed_long_cooldown_expired_yields_speak() {
        let r = decide_speak(&context(
            AnalyseKind::StayedLong,
            Some("看视频"),
            Some(SPEAK_COOLDOWN_SECS * STAY_SPEAK_COOLDOWN_MULTIPLIER as u64 + 1),
            "查看网页",
            1000,
        ));
        assert_eq!(
            r,
            SpeakDecision::SpeakAndGood(Speak {
                speaker_last_at: 1000,
                prev_summary: "查看网页".to_string(),
            })
        );
    }

    #[test]
    fn first_capture_no_previous_speaks() {
        let r = decide_speak(&context(
            AnalyseKind::FirstCapture,
            None,
            None,
            "文档编辑",
            2000,
        ));
        assert_eq!(
            r,
            SpeakDecision::SpeakAndGood(Speak {
                speaker_last_at: 2000,
                prev_summary: "文档编辑".to_string(),
            })
        );
    }

    #[test]
    fn fingerprint_ignores_whitespace_and_punctuation() {
        assert_eq!(summary_fingerprint("你好， 世界！"), "你好世界");
        assert_eq!(
            summary_fingerprint("PHP 是世界上最好的语言"),
            "PHP是世界上最好的语言"
        );
    }

    #[test]
    fn reappeared_file_uses_standard_cooldown() {
        let r = decide_speak(&context(
            AnalyseKind::ReappearedFile,
            Some("看视频"),
            Some(SPEAK_COOLDOWN_SECS - 1),
            "报告.pptx - PowerPoint：过去记录到 2 次使用\n应用：POWERPNT.EXE",
            1000,
        ));
        assert_eq!(r, SpeakDecision::DoNotSpeak);
    }
}

#[cfg(test)]
mod file_identity_tests {
    use super::*;

    #[test]
    fn identifies_file_and_episode_topics_without_grouping_arbitrary_browser_titles() {
        assert_eq!(
            topic_identity("POWERPNT.EXE", Some("报告.pptx - PowerPoint")),
            Some((
                "file:POWERPNT.EXE:报告.pptx".to_string(),
                "file".to_string(),
                "报告.pptx".to_string(),
            ))
        );
        assert_eq!(
            topic_identity("chrome.exe", Some("繁花 第12集 - 腾讯视频")),
            Some((
                "media:繁花".to_string(),
                "media".to_string(),
                "繁花".to_string(),
            ))
        );
        assert_eq!(topic_identity("chrome.exe", Some("知乎 - 今日热榜")), None);
    }
    #[test]
    fn extracts_file_key_from_powerpoint_title() {
        assert_eq!(
            extract_file_key(Some("报告.pptx - PowerPoint")),
            Some("报告.pptx".to_string())
        );
    }

    #[test]
    fn extracts_file_key_from_word_and_notepad_titles() {
        assert_eq!(
            extract_file_key(Some("论文.docx - Word")),
            Some("论文.docx".to_string())
        );
        assert_eq!(
            extract_file_key(Some("笔记.txt - 记事本")),
            Some("笔记.txt".to_string())
        );
    }

    #[test]
    fn normalizes_extension_case_only() {
        assert_eq!(
            extract_file_key(Some("报告.PPTX - PowerPoint")),
            Some("报告.pptx".to_string())
        );
        assert_eq!(
            extract_file_key(Some("我的.报告.pptx - PowerPoint")),
            Some("我的.报告.pptx".to_string())
        );
    }

    #[test]
    fn strips_visual_studio_code_suffix() {
        assert_eq!(
            extract_file_key(Some("lib.rs - Visual Studio Code")),
            Some("lib.rs".to_string())
        );
    }

    #[test]
    fn browser_page_title_is_not_a_file() {
        assert_eq!(extract_file_key(Some("知乎 - 今日热榜")), None);
        assert_eq!(extract_file_key(Some("Bing")), None);
    }

    #[test]
    fn bare_filename_with_extension_is_a_file() {
        assert_eq!(
            extract_file_key(Some("报告.pptx")),
            Some("报告.pptx".to_string())
        );
        assert_eq!(extract_file_key(Some("报告")), None);
    }

    #[test]
    fn no_title_yields_no_file_key() {
        assert_eq!(extract_file_key(None), None);
        assert_eq!(extract_file_key(Some("   ")), None);
    }

    #[test]
    fn summarizes_past_observations_with_real_counts() {
        let events = vec![crate::storage::CyralActivityEvent {
            id: 1,
            event_type: "screen_observation".to_string(),
            source: "visual".to_string(),
            app_name: "POWERPNT.EXE".to_string(),
            window_title: Some("报告.pptx - PowerPoint".to_string()),
            summary: "正在编辑项目报告".to_string(),
            observed_at: 1_752_000_000,
        }];
        let text = summarize_activity_history(&events);
        assert!(text.contains("报告.pptx - PowerPoint"));
        assert!(text.contains("过去记录到 1 次观察"));
        assert!(text.contains("正在编辑项目报告"));
        assert!(!text.contains("分钟"));
    }
}

#[cfg(test)]
mod companion_start_feedback_tests {
    use super::*;

    #[test]
    fn startup_feedback_is_honest_when_no_external_window_was_observed() {
        assert_eq!(
            companion_start_feedback(None),
            "陪伴模式已开启，但暂未观察到其他应用；请切回需要我看的界面。"
        );
    }

    #[test]
    fn startup_feedback_identifies_the_observed_external_application() {
        let observation = observation_from_window("WINWORD.EXE", Some("论文.docx"));
        assert!(companion_start_feedback(Some(&observation)).contains("WINWORD.EXE"));
    }
}

pub(crate) fn companion_start_feedback(observation: Option<&Observation>) -> String {
    match observation {
        Some(observation) => format!(
            "陪伴模式已开启。我观察到你正在使用 {}。",
            observation.app_name
        ),
        None => "陪伴模式已开启，但暂未观察到其他应用；请切回需要我看的界面。".to_string(),
    }
}

/// 陪伴开启后的首次反馈不依赖模型：在 20 秒绝对截止前等待一个非 Cyral 前台，
/// 成功即基于真实观察反馈；若始终没有外部前台则如实说明。
pub(crate) fn spawn_companion_start_feedback(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let generation = app
            .try_state::<crate::CompanionSession>()
            .map(|session| crate::companion_stop_generation(&session))
            .unwrap_or(0);
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let alive = app
                .try_state::<crate::CompanionSession>()
                .map(|session| crate::companion_chain_alive(&session, generation))
                .unwrap_or(false);
            if !alive {
                return;
            }
            if let Some(sample) = activity_sampling::sample_foreground() {
                let is_cyral = sample.app_name.to_ascii_lowercase().starts_with("cyral");
                if !sample.locked && !is_cyral {
                    let observation = if let Some(database) = app.try_state::<Database>() {
                        observe_foreground(&sample.app_name, sample.title.as_deref(), &database)
                            .await
                    } else {
                        observation_from_window(&sample.app_name, sample.title.as_deref())
                    };
                    if let Some(session) = app.try_state::<crate::CompanionSession>() {
                        if let Ok(mut state) = session.0.lock() {
                            if state.active {
                                state.last_observation = Some(observation.clone());
                            }
                        }
                    }
                    persist_observation(&app, &observation);
                    crate::ensure_pet_visible(&app);
                    let _ = app.emit_to(
                        "pet",
                        "assistant-message",
                        companion_start_feedback(Some(&observation)),
                    );
                    return;
                }
            }
            if Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let alive = app
            .try_state::<crate::CompanionSession>()
            .map(|session| crate::companion_chain_alive(&session, generation))
            .unwrap_or(false);
        if alive {
            crate::ensure_pet_visible(&app);
            let _ = app.emit_to("pet", "assistant-message", companion_start_feedback(None));
        }
    });
}
