#[cfg(all(not(debug_assertions), not(feature = "custom-protocol")))]
compile_error!("Release builds must enable custom-protocol so Cyral uses embedded frontend assets");

use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use chrono::{Datelike, Timelike};
#[cfg(test)]
use futures_util::StreamExt;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tauri::{
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, State,
};

pub mod activity;
pub mod email;
pub mod execution;
pub mod storage;
pub mod windows_actions;
#[cfg(test)]
use storage::initialize_database;
use storage::open_database_with_recovery;

// ponytail: 进程级共享 reqwest Client，开启连接池复用，避免每个请求都重建 TCP/TLS 握手。
// 若未来并发请求量显著上升，可拆分按超时分类的多个 Client。
fn shared_http_client(timeout_secs: u64) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .pool_max_idle_per_host(4);

    // reqwest 会自动识别 HTTP(S)_PROXY 等环境变量，但不会读取 Windows 的
    // WinINET 用户代理。Chrome/Edge 使用后者时，浏览器授权成功而令牌交换会直连超时。
    #[cfg(windows)]
    if !proxy_environment_is_configured() {
        if let Some(proxy_url) = windows_user_proxy_url() {
            match reqwest::Proxy::all(&proxy_url) {
                Ok(proxy) => builder = builder.proxy(proxy),
                Err(error) => eprintln!("忽略无效的 Windows 系统代理配置：{error}"),
            }
        }
    }

    builder.build().unwrap_or_else(|error| {
        eprintln!("无法创建共享 HTTP 客户端：{error}");
        reqwest::Client::builder()
            .build()
            .expect("无超时的 HTTP 客户端应可构建")
    })
}

#[cfg(windows)]
fn proxy_environment_is_configured() -> bool {
    [
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ]
    .iter()
    .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

#[cfg(windows)]
fn normalize_windows_proxy_endpoint(value: &str) -> Option<String> {
    let endpoint = value.trim();
    if endpoint.is_empty() || endpoint.eq_ignore_ascii_case("<local>") {
        return None;
    }
    if endpoint.contains("://") {
        Some(endpoint.to_string())
    } else {
        Some(format!("http://{endpoint}"))
    }
}

#[cfg(windows)]
fn parse_windows_proxy_server(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if !value.contains('=') {
        return normalize_windows_proxy_endpoint(value);
    }

    let entries: HashMap<String, String> = value
        .split(';')
        .filter_map(|entry| entry.split_once('='))
        .map(|(scheme, endpoint)| {
            (
                scheme.trim().to_ascii_lowercase(),
                endpoint.trim().to_string(),
            )
        })
        .collect();
    entries
        .get("https")
        .or_else(|| entries.get("http"))
        .and_then(|endpoint| normalize_windows_proxy_endpoint(endpoint))
}

#[cfg(windows)]
fn windows_user_proxy_url() -> Option<String> {
    use std::os::windows::ffi::OsStringExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
    };

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(Some(0)).collect()
    }

    unsafe fn read_dword(key: HKEY, name: &str) -> Option<u32> {
        let name = wide(name);
        let mut value = 0u32;
        let mut byte_len = std::mem::size_of::<u32>() as u32;
        (RegQueryValueExW(
            key,
            PCWSTR(name.as_ptr()),
            None,
            None,
            Some((&mut value as *mut u32).cast::<u8>()),
            Some(&mut byte_len),
        ) == ERROR_SUCCESS)
            .then_some(value)
    }

    unsafe fn read_string(key: HKEY, name: &str) -> Option<String> {
        let name = wide(name);
        let mut byte_len = 0u32;
        if RegQueryValueExW(
            key,
            PCWSTR(name.as_ptr()),
            None,
            None,
            None,
            Some(&mut byte_len),
        ) != ERROR_SUCCESS
            || byte_len < 2
        {
            return None;
        }
        let mut buffer = vec![0u16; byte_len as usize / 2 + 1];
        if RegQueryValueExW(
            key,
            PCWSTR(name.as_ptr()),
            None,
            None,
            Some(buffer.as_mut_ptr().cast::<u8>()),
            Some(&mut byte_len),
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let length = buffer
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(buffer.len());
        Some(
            std::ffi::OsString::from_wide(&buffer[..length])
                .to_string_lossy()
                .to_string(),
        )
    }

    let path = wide(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings");
    let mut key = HKEY::default();
    unsafe {
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            None,
            KEY_READ,
            &mut key,
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let enabled = read_dword(key, "ProxyEnable") == Some(1);
        let proxy_server = enabled.then(|| read_string(key, "ProxyServer")).flatten();
        let _ = RegCloseKey(key);
        proxy_server.and_then(|value| parse_windows_proxy_server(&value))
    }
}

/// 默认月度预算（未配置时）：10 美元。
const DEFAULT_BUDGET_USD: f64 = 10.0;
/// 估算单价（美元 / 百万 token）：MVP 用统一保守值，UI 标注"估算"。
const ESTIMATED_COST_PER_1M_TOKENS: f64 = 1.0;
/// 80% 提醒阈值。
const BUDGET_WARN_RATIO: f64 = 0.8;

/// 待落库的用量缓冲：各请求路径只 push，落库由 flush 统一处理，避免到处穿 DB。
static USAGE_BUFFER: Mutex<Vec<crate::storage::UsageRecord>> = Mutex::new(Vec::new());
/// 上次已提醒的预算比例，用于"80%/100% 跨阈值只提醒一次"。
static LAST_WARNED_BUDGET_RATIO: Mutex<f64> = Mutex::new(0.0);

/// 从响应解析 usage 并压入缓冲（不落库、不阻塞请求）。只取 token 数字，不碰正文。
fn record_usage_from_response(body: &serde_json::Value, url: &str, response_text: &str) {
    let Ok(response) = serde_json::from_str::<serde_json::Value>(response_text) else {
        return;
    };
    let usage = &response["usage"];
    let (prompt, completion) = if url.contains("/responses") {
        (
            usage["input_tokens"].as_i64().unwrap_or(0),
            usage["output_tokens"].as_i64().unwrap_or(0),
        )
    } else {
        (
            usage["prompt_tokens"].as_i64().unwrap_or(0),
            usage["completion_tokens"].as_i64().unwrap_or(0),
        )
    };
    let total = usage["total_tokens"]
        .as_i64()
        .unwrap_or(prompt.saturating_add(completion));
    if total <= 0 && prompt <= 0 && completion <= 0 {
        return; // 供应商未返回 usage，不记账。
    }
    let model = body["model"].as_str().unwrap_or("").to_string();
    let protocol = if url.contains("/responses") {
        "responses".to_string()
    } else {
        "chat_completions".to_string()
    };
    let est_cost = total as f64 * ESTIMATED_COST_PER_1M_TOKENS / 1_000_000.0;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    if let Ok(mut buffer) = USAGE_BUFFER.lock() {
        buffer.push(crate::storage::UsageRecord {
            provider_name: String::new(),
            model,
            protocol,
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: total,
            est_cost_usd: est_cost,
            created_at: now,
        });
    }
}

/// 把缓冲中的用量记录写入 ledger（幂等：写后清空）。
fn flush_usage_records(connection: &Connection) -> usize {
    let records = USAGE_BUFFER
        .lock()
        .map(|mut buffer| std::mem::take(&mut *buffer))
        .unwrap_or_default();
    let mut written = 0;
    for record in &records {
        if crate::storage::insert_usage_record(connection, record).is_ok() {
            written += 1;
        }
    }
    written
}

/// 本地时区的自然月键（如 "2026-08"）。
fn local_month_key(now: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(now, 0)
        .single()
        .map(|dt| dt.format("%Y-%m").to_string())
        .unwrap_or_else(|| "1970-01".to_string())
}

fn load_monthly_budget(connection: &Connection) -> f64 {
    connection
        .query_row(
            "SELECT monthly_budget_usd FROM model_provider WHERE id = 1",
            [],
            |row| row.get::<_, f64>(0),
        )
        .unwrap_or(DEFAULT_BUDGET_USD)
}

/// 本月已用（美元）与预算上限。
fn monthly_budget_status(connection: &Connection, now: i64) -> (f64, f64) {
    let month = local_month_key(now);
    let (_, used) = crate::storage::monthly_usage(connection, &month).unwrap_or((0, 0.0));
    let budget = load_monthly_budget(connection);
    (used, budget)
}

/// 纯函数：是否应拦截云端请求（已用 >= 预算）。
fn budget_blocked(used: f64, budget: f64) -> bool {
    budget > 0.0 && used >= budget
}

/// 纯函数：跨阈值（80% / 100%）生成一次提醒文案。
fn budget_warning(used: f64, budget: f64, last_warned: f64) -> Option<String> {
    if budget <= 0.0 {
        return None;
    }
    let ratio = used / budget;
    if ratio >= 1.0 {
        return Some(format!(
            "本月 API 预算（${budget:.2}）已用完，云端请求已暂停。可在模型配置中调高预算后继续。"
        ));
    }
    if ratio >= BUDGET_WARN_RATIO && last_warned < BUDGET_WARN_RATIO {
        return Some(format!(
            "本月 API 费用已达预算的 {:.0}%，注意控制用量。",
            ratio * 100.0
        ));
    }
    None
}

/// 刷新缓冲并返回是否应拦截云端请求。
pub(crate) fn cloud_budget_blocked(database: &Database) -> bool {
    let Ok(connection) = database.0.lock() else {
        return false;
    };
    flush_usage_records(&connection);
    let (used, budget) = monthly_budget_status(&connection, now_unix());
    budget_blocked(used, budget)
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn local_day_start_unix() -> i64 {
    let now = chrono::Local::now();
    now.timestamp()
        - i64::from(now.hour()) * 3_600
        - i64::from(now.minute()) * 60
        - i64::from(now.second())
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

// ---- 自由活动：移动 / 悬浮球 / 移动许可（会话级） ----

/// 无交互多久后收起为悬浮球（秒）。
const COMPACT_AFTER_SECS: i64 = 300;
/// 普通模式收起为悬浮球后，最长保持 10 分钟。
const COMPACT_AUTO_RESTORE_SECS: i64 = 600;
/// 单段行走距离下限（px）：不得过短，避免频繁切换。
const WALK_MIN_PX: i64 = 200;
/// 行走距离按工作区缩放：最短=短边×0.20（不低于 WALK_MIN_PX），最长=长边×0.70。
const WALK_DIST_MIN_RATIO: f64 = 0.20;
const WALK_DIST_MAX_RATIO: f64 = 0.70;
/// 判定“靠近屏幕边缘”的窗口边距（px）：窗口距边缘小于该值才允许对应方向的边缘探头动画。
const EDGE_PEEK_PX: i32 = 80;
/// 行走速度区间（px/s）。
const WALK_SPEED_MIN_PX_S: f64 = 80.0;
const WALK_SPEED_MAX_PX_S: f64 = 120.0;
/// 每段行走后的停留待机时长区间（秒）：不得过短。
const STAY_MIN_SECS: i64 = 20;
const STAY_MAX_SECS: i64 = 60;
const COMPACT_SIZE: u32 = 77;
const NORMAL_PET_SIZE: (u32, u32) = (300, 260);
const EDGE_MARGIN: u32 = 24;
const WAYPOINT_RETRIES: u32 = 20;
const PET_MOVER_INTERVAL_MS: u64 = 33;
const COMPACT_TRANSITION_MS: u64 = 1_800;
/// 拖动结束时，落点距行走目标小于该半径视为已到达（结束本段进入停留）。
const WALK_DROP_ARRIVE_PX: f64 = 24.0;

fn pet_window_logical_size(compact: bool) -> (f64, f64) {
    if compact {
        (COMPACT_SIZE as f64, COMPACT_SIZE as f64)
    } else {
        (NORMAL_PET_SIZE.0 as f64, NORMAL_PET_SIZE.1 as f64)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum MotionState {
    NormalIdle,
    Walking {
        from: (i32, i32),
        to: (i32, i32),
        speed_px_s: f64,
        started_at_ms: i64,
    },
    Compacting,
    Compact,
    Restoring,
}

/// 自由活动运行状态（会话级；移动许可与 walk 可用性为进程内标志）。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PetMotion {
    state: MotionState,
    movement_allowed: bool,
    walk_available: bool,
    locked: bool,
    last_user_interaction_at: i64,
    /// 进入悬浮球的时刻；仅普通模式的 10 分钟自动恢复使用。
    compact_started_at: Option<i64>,
    last_normal_position: (i32, i32),
    last_walk_target: Option<(i32, i32)>,
    stay_until: i64,
    /// 自动移动/形态切换期间为 true：Moved/Resized 事件不得持久化或联动面板。
    auto_moving: bool,
    /// 用户正在拖动桌宠：自动移动让位（不 set_position），松开后从落点继续。
    dragging: bool,
}

impl Default for PetMotion {
    fn default() -> Self {
        Self {
            state: MotionState::NormalIdle,
            movement_allowed: true,
            walk_available: false,
            locked: false,
            last_user_interaction_at: now_unix(),
            compact_started_at: None,
            last_normal_position: (0, 0),
            last_walk_target: None,
            stay_until: 0,
            auto_moving: false,
            dragging: false,
        }
    }
}

struct PetMotionState(Mutex<PetMotion>);

fn mark_pet_interaction(motion: &PetMotionState) {
    if let Ok(mut state) = motion.0.lock() {
        state.last_user_interaction_at = now_unix();
    }
}

fn apply_movement_permission(state: &mut PetMotion, enabled: bool) {
    state.movement_allowed = enabled;
    if enabled {
        state.stay_until = 0;
    }
}

/// 简单可测的 xorshift 随机源。
#[derive(Clone, Copy)]
struct XorShift(u64);

impl XorShift {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn next_range(&mut self, lo: i64, hi: i64) -> i64 {
        if hi <= lo {
            return lo;
        }
        lo + (self.next_u64() % (hi - lo + 1) as u64) as i64
    }
}

fn fresh_rng() -> XorShift {
    XorShift::new(now_unix_ms() as u64 ^ (now_unix() as u64).rotate_left(32))
}

/// 陪伴主动搭话每轮随机等待 10–15 分钟，避免形成固定报时行为。
const PROACTIVE_MIN_INTERVAL_SECS: i64 = 600;
const PROACTIVE_MAX_INTERVAL_SECS: i64 = 900;
const PROACTIVE_RETRY_DELAYS_SECS: [i64; 2] = [60, 300];

fn draw_proactive_interval_secs(rng: &mut XorShift) -> i64 {
    rng.next_range(PROACTIVE_MIN_INTERVAL_SECS, PROACTIVE_MAX_INTERVAL_SECS)
}

fn distance(a: (i32, i32), b: (i32, i32)) -> f64 {
    let dx = (a.0 - b.0) as f64;
    let dy = (a.1 - b.1) as f64;
    (dx * dx + dy * dy).sqrt()
}

fn point_in_rect(point: (i32, i32), rect: (i32, i32, u32, u32), margin: i32) -> bool {
    point.0 >= rect.0 - margin
        && point.0 <= rect.0 + rect.2 as i32 + margin
        && point.1 >= rect.1 - margin
        && point.1 <= rect.1 + rect.3 as i32 + margin
}

/// 按工作区尺寸计算单段行走距离区间：短边 20%（下限 WALK_MIN_PX），长边 70%。
fn walk_distance_range(work_w: i64, work_h: i64) -> (i64, i64) {
    let short = work_w.min(work_h) as f64;
    let long = work_w.max(work_h) as f64;
    let min_dist = (short * WALK_DIST_MIN_RATIO)
        .round()
        .max(WALK_MIN_PX as f64) as i64;
    let max_dist = (long * WALK_DIST_MAX_RATIO)
        .round()
        .max((min_dist + 1) as f64) as i64;
    (min_dist, max_dist)
}

/// 宠物窗口贴近屏幕的哪些边缘（用于允许对应方向的边缘探头动画）。
fn near_screen_edges(
    position: (i32, i32),
    window_size: (u32, u32),
    monitor_position: (i32, i32),
    monitor_size: (u32, u32),
    margin: i32,
) -> Vec<&'static str> {
    let mut edges = Vec::new();
    if position.0 - monitor_position.0 <= margin {
        edges.push("left");
    }
    if monitor_position.0 + monitor_size.0 as i32 - (position.0 + window_size.0 as i32) <= margin {
        edges.push("right");
    }
    if position.1 - monitor_position.1 <= margin {
        edges.push("top");
    }
    if monitor_position.1 + monitor_size.1 as i32 - (position.1 + window_size.1 as i32) <= margin {
        edges.push("bottom");
    }
    edges
}

/// 在工作区内挑选下一段行走终点：距离按工作区缩放、避开可见窗口、
/// 不与上一个目标过近（防紧邻折返）；重试上限后沿可通行方向延伸回退。
fn pick_walk_target(
    rng: &mut XorShift,
    current: (i32, i32),
    window_size: (u32, u32),
    monitor_position: (i32, i32),
    monitor_size: (u32, u32),
    blocked: &[(i32, i32, u32, u32)],
    previous_target: Option<(i32, i32)>,
) -> (i32, i32) {
    let min_x = monitor_position.0 + EDGE_MARGIN as i32;
    let min_y = monitor_position.1 + EDGE_MARGIN as i32;
    let max_x =
        monitor_position.0 + monitor_size.0 as i32 - window_size.0 as i32 - EDGE_MARGIN as i32;
    let max_y =
        monitor_position.1 + monitor_size.1 as i32 - window_size.1 as i32 - EDGE_MARGIN as i32;
    let (min_dist, max_dist) = walk_distance_range((max_x - min_x) as i64, (max_y - min_y) as i64);
    for _ in 0..WAYPOINT_RETRIES {
        let distance_px = rng.next_range(min_dist, max_dist) as f64;
        let angle = rng.next_u64() as f64 / u64::MAX as f64 * std::f64::consts::TAU;
        let target = (
            current.0 + (angle.cos() * distance_px).round() as i32,
            current.1 + (angle.sin() * distance_px).round() as i32,
        );
        if target.0 < min_x || target.0 > max_x || target.1 < min_y || target.1 > max_y {
            continue;
        }
        if let Some(previous) = previous_target {
            if distance(target, previous) < 120.0 {
                continue;
            }
        }
        if blocked.iter().any(|rect| point_in_rect(target, *rect, 12)) {
            continue;
        }
        return target;
    }
    // 回退：沿可通行方向逐段延伸，取合法最远点。
    let mut fallback = current;
    let dx = if max_x > min_x { 1 } else { -1 };
    let dy = if max_y > min_y { 1 } else { -1 };
    for _ in 0..WAYPOINT_RETRIES {
        let candidate = (
            (fallback.0 + dx * 24).clamp(min_x, max_x),
            (fallback.1 + dy * 24).clamp(min_y, max_y),
        );
        if blocked
            .iter()
            .any(|rect| point_in_rect(candidate, *rect, 12))
        {
            break;
        }
        fallback = candidate;
        if distance(fallback, current) >= min_dist as f64 {
            break;
        }
    }
    fallback
}

/// 悬浮球锚点：当前显示器右边缘中部，完整可见可点。
fn compact_anchor_position(
    monitor_position: (i32, i32),
    monitor_size: (u32, u32),
    compact_size: u32,
    margin: u32,
) -> (i32, i32) {
    (
        monitor_position.0 + monitor_size.0 as i32 - compact_size as i32 - margin as i32,
        monitor_position.1 + (monitor_size.1 as i32 - compact_size as i32) / 2,
    )
}

/// 线性插值：progress ∈ [0,1]。
fn interpolate_position(from: (i32, i32), to: (i32, i32), progress: f64) -> (i32, i32) {
    let progress = progress.clamp(0.0, 1.0);
    (
        (from.0 as f64 + (to.0 - from.0) as f64 * progress).round() as i32,
        (from.1 as f64 + (to.1 - from.1) as f64 * progress).round() as i32,
    )
}

/// 拖动结束时进行中的行走如何继续：以落点为新起点走向原目标（行动与动画不中断）；
/// 落点已到达目标附近时返回 None（本段行走结束，由调用方转入停留）。
fn rebase_walk_after_drag(
    drop: (i32, i32),
    to: (i32, i32),
    speed_px_s: f64,
    now_ms: i64,
) -> Option<MotionState> {
    if distance(drop, to) <= WALK_DROP_ARRIVE_PX {
        return None;
    }
    Some(MotionState::Walking {
        from: drop,
        to,
        speed_px_s,
        started_at_ms: now_ms,
    })
}

/// 左键是否仍被按住（用于判定原生窗口拖动是否结束）。
#[cfg(windows)]
fn left_mouse_button_pressed() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    let state = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) };
    (state as u16) & 0x8000 != 0
}

#[cfg(not(windows))]
fn left_mouse_button_pressed() -> bool {
    false
}

/// 是否该收起为悬浮球：仅普通模式下，正常待机、未锁定、未被拖动且无交互超时。
fn should_compact(state: &PetMotion, companion_active: bool, now: i64) -> bool {
    !companion_active
        && state.state == MotionState::NormalIdle
        && !state.locked
        && !state.dragging
        && now - state.last_user_interaction_at >= COMPACT_AFTER_SECS
}

/// 已收起的桌宠应在陪伴开启时立即恢复；普通模式下最多收起 10 分钟。
fn should_restore_from_compact(state: &PetMotion, companion_active: bool, now: i64) -> bool {
    if companion_active {
        return state.state == MotionState::Compact || state.state == MotionState::Compacting;
    }
    state.state == MotionState::Compact
        && state
            .compact_started_at
            .is_some_and(|started_at| now - started_at >= COMPACT_AUTO_RESTORE_SECS)
}

/// 是否该出发行走：正常待机、停留到期、移动许可=允许、walk 资源可用、未锁定、未被拖动。
fn should_walk(state: &PetMotion, now: i64) -> bool {
    state.state == MotionState::NormalIdle
        && now >= state.stay_until
        && state.movement_allowed
        && state.walk_available
        && !state.locked
        && !state.dragging
}

fn emit_pet_motion(app: &AppHandle, state: &MotionState) {
    let (state_name, action) = match state {
        MotionState::NormalIdle => ("normal-idle", "idle"),
        MotionState::Walking { .. } => ("walking", "walk"),
        MotionState::Compacting => ("compacting", "exit"),
        MotionState::Compact => ("compact", "compact"),
        MotionState::Restoring => ("restoring", "enter"),
    };
    // 实时计算贴近的边缘，供前端决定是否允许对应方向的边缘探头动画。
    let near_edges = app
        .get_webview_window("pet")
        .and_then(|window| {
            let position = window.outer_position().ok()?;
            let size = window.outer_size().ok()?;
            let monitor = window.current_monitor().ok().flatten()?;
            let monitor_position = monitor.position();
            let monitor_size = monitor.size();
            Some(near_screen_edges(
                (position.x, position.y),
                (size.width, size.height),
                (monitor_position.x, monitor_position.y),
                (monitor_size.width, monitor_size.height),
                EDGE_PEEK_PX,
            ))
        })
        .unwrap_or_default();
    let _ = app.emit_to(
        "pet",
        "pet-motion",
        serde_json::json!({ "state": state_name, "action": action, "nearEdges": near_edges }),
    );
}

fn visible_window_rect(app: &AppHandle, label: &str) -> Option<(i32, i32, u32, u32)> {
    let window = app.get_webview_window(label)?;
    if !window.is_visible().unwrap_or(false) {
        return None;
    }
    let position = window.outer_position().ok()?;
    let size = window.outer_size().ok()?;
    Some((position.x, position.y, size.width, size.height))
}

/// 悬浮球被点击/打断后恢复正常形态；open_composer=true 时恢复后唤起对话框。
/// 幂等：非悬浮球状态直接返回成功。
fn restore_from_compact_inner(
    app: &AppHandle,
    motion: &PetMotionState,
    launch_composer: bool,
) -> Result<(), String> {
    let last_position = {
        let mut state = motion.0.lock().map_err(|error| error.to_string())?;
        if state.state != MotionState::Compact && state.state != MotionState::Compacting {
            return Ok(());
        }
        state.state = MotionState::Restoring;
        state.auto_moving = true;
        state.compact_started_at = None;
        state.last_user_interaction_at = now_unix();
        state.last_normal_position
    };
    if let Some(window) = app.get_webview_window("pet") {
        let (width, height) = pet_window_logical_size(false);
        let target = window
            .primary_monitor()
            .ok()
            .flatten()
            .map(|monitor| {
                let position = monitor.position();
                let size = monitor.size();
                restore_position_in_visible_work_area(
                    last_position,
                    (NORMAL_PET_SIZE.0, NORMAL_PET_SIZE.1),
                    (position.x, position.y),
                    (size.width, size.height),
                    EDGE_MARGIN,
                )
            })
            .unwrap_or(last_position);
        let _ = window.set_size(LogicalSize::new(width, height));
        let _ = window.set_position(PhysicalPosition::new(target.0, target.1));
        if let Ok(mut state) = motion.0.lock() {
            if state.state == MotionState::Restoring {
                state.last_normal_position = target;
            }
        }
    }
    emit_pet_motion(app, &MotionState::Restoring);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(COMPACT_TRANSITION_MS)).await;
        if let Some(motion) = app.try_state::<PetMotionState>() {
            if let Ok(mut state) = motion.0.lock() {
                if state.state == MotionState::Restoring {
                    state.state = MotionState::NormalIdle;
                    state.auto_moving = false;
                    state.stay_until =
                        now_unix() + fresh_rng().next_range(STAY_MIN_SECS, STAY_MAX_SECS);
                }
            }
        }
        emit_pet_motion(&app, &MotionState::NormalIdle);
        if launch_composer {
            let _ = open_composer(app);
        }
    });
    Ok(())
}

/// 托盘是独立救援入口：不依赖前端形态与 Rust 状态一致，始终把 Cyral 带回当前主屏。
fn rescue_pet_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("pet") else {
        return;
    };
    let saved_position = app
        .try_state::<PetMotionState>()
        .and_then(|motion| motion.0.lock().ok().map(|state| state.last_normal_position))
        .unwrap_or((0, 0));
    let target = window
        .primary_monitor()
        .ok()
        .flatten()
        .map(|monitor| {
            let position = monitor.position();
            let size = monitor.size();
            restore_position_in_visible_work_area(
                saved_position,
                NORMAL_PET_SIZE,
                (position.x, position.y),
                (size.width, size.height),
                EDGE_MARGIN,
            )
        })
        .unwrap_or(saved_position);
    let (width, height) = pet_window_logical_size(false);
    let _ = window.set_size(LogicalSize::new(width, height));
    let _ = window.set_position(PhysicalPosition::new(target.0, target.1));
    let _ = window.show();
    let _ = window.set_focus();
    if let Some(motion) = app.try_state::<PetMotionState>() {
        if let Ok(mut state) = motion.0.lock() {
            state.state = MotionState::NormalIdle;
            state.auto_moving = false;
            state.dragging = false;
            state.compact_started_at = None;
            state.last_normal_position = target;
            state.last_user_interaction_at = now_unix();
        }
    }
    emit_pet_motion(app, &MotionState::NormalIdle);
}
/// 消息/提醒/托盘等事件到达时，若处于悬浮球形态则先恢复正常形态（不弹对话框）。
fn ensure_pet_visible(app: &AppHandle) {
    if let Some(motion) = app.try_state::<PetMotionState>() {
        let is_compact = motion
            .0
            .lock()
            .map(|state| {
                state.state == MotionState::Compact || state.state == MotionState::Compacting
            })
            .unwrap_or(false);
        if is_compact {
            let _ = restore_from_compact_inner(app, &motion, false);
        }
    }
}

#[tauri::command]
fn get_pet_motion(motion: State<'_, PetMotionState>) -> Result<PetMotion, String> {
    motion
        .0
        .lock()
        .map(|state| state.clone())
        .map_err(|error| error.to_string())
}

/// 由 pet 前端在所有事件监听器注册完成后调用，解除提醒与启动消息的投递门控。
#[tauri::command]
fn mark_pet_frontend_ready(readiness: State<'_, RuntimeReadiness>) {
    readiness.0.store(true, Ordering::Release);
}

/// 前端加载动画 manifest 后上报哪些动作可用（当前只关心 walk）。
#[tauri::command]
fn set_animation_available(motion: State<'_, PetMotionState>, walk: bool) -> Result<(), String> {
    motion
        .0
        .lock()
        .map_err(|error| error.to_string())?
        .walk_available = walk;
    Ok(())
}

/// 用户对桌宠的任何指针交互：刷新"无交互"计时，推迟收起为悬浮球。
#[tauri::command]
fn pet_pointer_active(motion: State<'_, PetMotionState>) -> Result<(), String> {
    mark_pet_interaction(&motion);
    Ok(())
}

/// 用户开始拖动桌宠：自动移动让位（暂停位置插值、不发起新动作）；
/// 收起过渡中被拖动则取消收起、回到正常形态。拖动结束由移动循环检测鼠标释放。
#[tauri::command]
fn pet_drag_started(app: AppHandle, motion: State<'_, PetMotionState>) -> Result<(), String> {
    let cancelled_compact = {
        let mut state = motion.0.lock().map_err(|error| error.to_string())?;
        state.dragging = true;
        state.last_user_interaction_at = now_unix();
        if state.state == MotionState::Compacting {
            state.state = MotionState::NormalIdle;
            state.auto_moving = false;
            state.compact_started_at = None;
            true
        } else {
            false
        }
    };
    if cancelled_compact {
        emit_pet_motion(&app, &MotionState::NormalIdle);
    }
    Ok(())
}

/// 点击悬浮球：恢复正常形态，可选唤起对话框。
#[tauri::command]
fn restore_from_compact(
    app: AppHandle,
    motion: State<'_, PetMotionState>,
    open_composer: bool,
) -> Result<(), String> {
    restore_from_compact_inner(&app, &motion, open_composer)
}

/// 决策循环（1s）：无交互超时收起为悬浮球；停留到期且许可/资源齐备时出发行走。
async fn pet_motion_ticker(app: AppHandle) {
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    loop {
        ticker.tick().await;
        let Some(motion_state) = app.try_state::<PetMotionState>() else {
            continue;
        };
        let now = now_unix();
        let companion_active = app
            .try_state::<CompanionSession>()
            .and_then(|session| session.0.lock().ok().map(|state| state.active))
            .unwrap_or(false);

        // 1) 陪伴开启时立即退出悬浮球；普通模式下悬浮球最多保留 10 分钟。
        let restore_now = motion_state
            .0
            .lock()
            .map(|state| should_restore_from_compact(&state, companion_active, now))
            .unwrap_or(false);
        if restore_now {
            let _ = restore_from_compact_inner(&app, &motion_state, false);
            continue;
        }

        // 2) 无交互超时 → 收起为悬浮球（仅普通模式、未锁定且正常待机）。
        let compact_now = {
            let Ok(state) = motion_state.0.lock() else {
                continue;
            };
            should_compact(&state, companion_active, now)
        };
        if compact_now {
            {
                let Ok(mut state) = motion_state.0.lock() else {
                    continue;
                };
                state.state = MotionState::Compacting;
                state.auto_moving = true;
            }
            emit_pet_motion(&app, &MotionState::Compacting);
            tokio::time::sleep(Duration::from_millis(COMPACT_TRANSITION_MS)).await;
            let still_compacting = motion_state
                .0
                .lock()
                .map(|state| state.state == MotionState::Compacting && !state.dragging)
                .unwrap_or(false);
            if !still_compacting {
                // 过渡中被拖动：取消收起、回到正常形态，避免拖动中途吸附到边缘。
                let cancelled = motion_state
                    .0
                    .lock()
                    .map(|mut state| {
                        if state.state == MotionState::Compacting {
                            state.state = MotionState::NormalIdle;
                            state.auto_moving = false;
                            state.compact_started_at = None;
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false);
                if cancelled {
                    emit_pet_motion(&app, &MotionState::NormalIdle);
                }
                continue;
            }
            let anchor = app.get_webview_window("pet").and_then(|window| {
                let monitor = window.current_monitor().ok().flatten()?;
                let position = monitor.position();
                let size = monitor.size();
                Some(compact_anchor_position(
                    (position.x, position.y),
                    (size.width, size.height),
                    COMPACT_SIZE,
                    EDGE_MARGIN,
                ))
            });
            if let Some(anchor) = anchor {
                if let Some(window) = app.get_webview_window("pet") {
                    let (width, height) = pet_window_logical_size(true);
                    let _ = window.set_size(LogicalSize::new(width, height));
                    let _ = window.set_position(PhysicalPosition::new(anchor.0, anchor.1));
                }
                {
                    let Ok(mut state) = motion_state.0.lock() else {
                        continue;
                    };
                    state.state = MotionState::Compact;
                    state.auto_moving = false;
                    state.compact_started_at = Some(now);
                }
                // 窗口查询/事件发送可能回到 UI 线程，必须在释放 PetMotion 锁后执行。
                emit_pet_motion(&app, &MotionState::Compact);
            }
            continue;
        }

        // 2) 停留到期且许可=允许、walk 可用、未锁定 → 出发行走。
        let walk_now = {
            let Ok(state) = motion_state.0.lock() else {
                continue;
            };
            should_walk(&state, now)
        };
        if walk_now {
            let Some(window) = app.get_webview_window("pet") else {
                continue;
            };
            let (Ok(current), Ok(window_size), Ok(monitor)) = (
                window.outer_position(),
                window.outer_size(),
                window.current_monitor(),
            ) else {
                continue;
            };
            let Some(monitor) = monitor else {
                continue;
            };
            let monitor_position = monitor.position();
            let monitor_size = monitor.size();
            let blocked = [
                visible_window_rect(&app, "composer"),
                visible_window_rect(&app, "panel"),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
            let previous_target = motion_state
                .0
                .lock()
                .ok()
                .and_then(|state| state.last_walk_target);
            let mut rng = fresh_rng();
            let target = pick_walk_target(
                &mut rng,
                (current.x, current.y),
                (window_size.width, window_size.height),
                (monitor_position.x, monitor_position.y),
                (monitor_size.width, monitor_size.height),
                &blocked,
                previous_target,
            );
            let speed =
                rng.next_range(WALK_SPEED_MIN_PX_S as i64, WALK_SPEED_MAX_PX_S as i64) as f64;
            let from = (current.x, current.y);
            {
                let Ok(mut state) = motion_state.0.lock() else {
                    continue;
                };
                state.state = MotionState::Walking {
                    from,
                    to: target,
                    speed_px_s: speed,
                    started_at_ms: now_unix_ms(),
                };
                state.auto_moving = true;
                state.last_walk_target = Some(target);
            }
            emit_pet_motion(
                &app,
                &MotionState::Walking {
                    from,
                    to: target,
                    speed_px_s: speed,
                    started_at_ms: 0,
                },
            );
        }
    }
}

/// 拖动结束结算（由移动循环在检测到鼠标释放后调用）：
/// - 行走中：以落点为新起点继续走向原目标（动画与行动不中断）；落点已到目标则停留。
/// - 正常待机：落点即新位置，持久化一次并联动面板（拖动期间 Moved 事件不写库）。
/// - 悬浮球：仅刷新交互，不覆盖正常形态的恢复位置。
fn finish_pet_drag(app: &AppHandle, motion: &PetMotionState) {
    let drop = app
        .get_webview_window("pet")
        .and_then(|window| window.outer_position().ok())
        .map(|position| (position.x, position.y));
    let mut persist_position: Option<(i32, i32)> = None;
    let mut settled = false;
    {
        let Ok(mut state) = motion.0.lock() else {
            return;
        };
        state.dragging = false;
        state.last_user_interaction_at = now_unix();
        let Some(drop) = drop else {
            return;
        };
        match state.state {
            MotionState::Walking { to, speed_px_s, .. } => {
                match rebase_walk_after_drag(drop, to, speed_px_s, now_unix_ms()) {
                    Some(walking) => {
                        state.state = walking;
                        state.last_normal_position = drop;
                    }
                    None => {
                        let stay = fresh_rng().next_range(STAY_MIN_SECS, STAY_MAX_SECS);
                        state.state = MotionState::NormalIdle;
                        state.auto_moving = false;
                        state.stay_until = now_unix() + stay;
                        state.last_normal_position = drop;
                        persist_position = Some(drop);
                        settled = true;
                    }
                }
            }
            MotionState::NormalIdle => {
                state.last_normal_position = drop;
                persist_position = Some(drop);
            }
            _ => {}
        }
    }
    if let Some((x, y)) = persist_position {
        if let Some(database) = app.try_state::<Database>() {
            if let Ok(connection) = database.0.lock() {
                let _ = save_pet_position(&connection, x, y);
            }
        }
        if let Some(panel) = app.get_webview_window("panel") {
            if panel.is_visible().unwrap_or(false) {
                let _ = position_panel(app);
            }
        }
    }
    if settled {
        emit_pet_motion(app, &MotionState::NormalIdle);
    }
}

/// 移动执行循环（~30Hz）：Walking 态按速度插值 set_position，到达后停留并持久化一次。
/// 用户拖动期间让位（不移动窗口），松开后按落点结算：行走中从落点继续走向原目标。
async fn pet_motion_mover(app: AppHandle) {
    let mut interval = tokio::time::interval(Duration::from_millis(PET_MOVER_INTERVAL_MS));
    loop {
        interval.tick().await;
        let Some(motion_state) = app.try_state::<PetMotionState>() else {
            continue;
        };
        let dragging = {
            let Ok(state) = motion_state.0.lock() else {
                continue;
            };
            state.dragging
        };
        if dragging {
            if left_mouse_button_pressed() {
                continue; // 拖动中：让位给系统拖动，不插值、不结算。
            }
            finish_pet_drag(&app, &motion_state);
            continue;
        }
        let walking = {
            let Ok(state) = motion_state.0.lock() else {
                continue;
            };
            match state.state {
                MotionState::Walking {
                    from,
                    to,
                    speed_px_s,
                    started_at_ms,
                } => Some((from, to, speed_px_s, started_at_ms)),
                _ => None,
            }
        };
        let Some((from, to, speed, started_at_ms)) = walking else {
            continue;
        };
        let total = distance(from, to);
        let elapsed_s = (now_unix_ms() - started_at_ms) as f64 / 1000.0;
        let progress = if total > 0.0 {
            elapsed_s * speed / total
        } else {
            1.0
        };
        let position = interpolate_position(from, to, progress);
        if let Some(window) = app.get_webview_window("pet") {
            let _ = window.set_position(PhysicalPosition::new(position.0, position.1));
        }
        if progress >= 1.0 {
            let stay = fresh_rng().next_range(STAY_MIN_SECS, STAY_MAX_SECS);
            {
                let Ok(mut state) = motion_state.0.lock() else {
                    continue;
                };
                state.state = MotionState::NormalIdle;
                state.auto_moving = false;
                state.stay_until = now_unix() + stay;
                state.last_normal_position = to;
            }
            // settle：自动移动结束才持久化一次位置。
            if let Some(database) = app.try_state::<Database>() {
                if let Ok(connection) = database.0.lock() {
                    let _ = save_pet_position(&connection, to.0, to.1);
                }
            }
            emit_pet_motion(&app, &MotionState::NormalIdle);
        }
    }
}

pub const CYRAL_CORE_BEHAVIOR: &str = include_str!("../prompts/cyral-core.md");
const MEMORY_EXTRACTION_PROMPT: &str = "你是 Cyral 的记忆整理器。只提取用户明确陈述、未来仍有价值的事实，不推断敏感属性，不记录寒暄、临时情绪或助手自己的话。输出严格 JSON：{\"memories\":[{\"kind\":\"fact|preference|person|project|constraint|event\",\"content\":\"第三人称原子事实\",\"sourceExcerpt\":\"用户原话的短摘录\",\"confidence\":0.0}]}。没有值得保存的内容时输出 {\"memories\":[]}.";

struct Database(Mutex<Connection>);
struct EmailTasks(Mutex<email::EmailTaskStore>);
struct ExecutionCandidates(Mutex<windows_actions::CandidateVault>);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct InboxStatus {
    account_address: String,
    inbox_unread_count: u64,
    new_since_last_check: u64,
    new_today_observed_by_cyral: u64,
    baseline_established_now: bool,
    checked_at: i64,
    message_content_read: bool,
}
/// 前端是否已经注册完宠物窗口的消息监听器。
/// 调度器在此之前不得投递并确认提醒，否则启动期间可能出现“已标记但用户没看到”。
struct RuntimeReadiness(AtomicBool);

struct RuntimeLifecycle {
    marker_path: PathBuf,
    recovered_from_unclean_exit: bool,
}

fn begin_runtime_session(marker_path: &std::path::Path, started_at: i64) -> std::io::Result<bool> {
    let recovered_from_unclean_exit = marker_path.exists();
    fs::write(marker_path, started_at.to_string())?;
    Ok(recovered_from_unclean_exit)
}

fn finish_runtime_session(marker_path: &std::path::Path) -> std::io::Result<()> {
    match fs::remove_file(marker_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn pet_frontend_ready(app: &AppHandle) -> bool {
    app.try_state::<RuntimeReadiness>()
        .is_some_and(|readiness| readiness.0.load(Ordering::Acquire))
}

/// 陪伴模式的运行状态（会话内，不跨开机）。`last_observation` 记录最近一次观察摘要，
/// 供运行时上下文让模型感知。
struct CompanionSession(Mutex<CompanionState>);

#[derive(Clone, Debug, Default)]
struct CompanionState {
    active: bool,
    /// 最近一次观察的结构化摘要；由活动采样线程写入，供对话模型如实转述。
    last_observation: Option<crate::activity::Observation>,
    /// 当前前台是否正是 Cyral 自己（用户在对话/面板中）；此时最近观察保留非 Cyral 应用。
    user_talking_to_cyral: bool,
    /// 停止代次：每次停止陪伴递增，用于取消陪伴侧进行中的异步链。
    stop_generation: u64,
    /// 下一次主动搭话的绝对时间；每一轮随机抽取一次。
    next_proactive_at: Option<i64>,
    /// 已领取但尚未返回的主动搭话任务，防止 5 秒轮询重复发起模型请求。
    proactive_in_flight: bool,
    /// 当前周期的可重试失败次数（最多两次）。
    proactive_retry_count: u8,
    /// 面板可见的最近状态，不保存模型正文或推理。
    last_proactive_result: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ProactiveOutcome {
    Success,
    Retryable(&'static str),
    Skipped(&'static str),
}

fn schedule_next_proactive(state: &mut CompanionState, now: i64) {
    let mut rng = fresh_rng();
    state.next_proactive_at = Some(now + draw_proactive_interval_secs(&mut rng));
    state.proactive_retry_count = 0;
}

/// 陪伴的唯一开启动作：从这里抽取第一轮随机搭话时间。
pub(crate) fn activate_companion(state: &mut CompanionState) {
    state.active = true;
    state.proactive_in_flight = false;
    state.last_proactive_result = Some("已计划下一次主动搭话".to_string());
    schedule_next_proactive(state, now_unix());
}

/// 领取到期任务。领取后先置 in-flight，避免轮询重复提交。
pub(crate) fn claim_due_proactive(state: &mut CompanionState, now: i64) -> Option<u64> {
    if state.active
        && !state.proactive_in_flight
        && state.next_proactive_at.is_some_and(|due| now >= due)
    {
        state.proactive_in_flight = true;
        state.next_proactive_at = None;
        state.last_proactive_result = Some("正在尝试主动搭话".to_string());
        Some(state.stop_generation)
    } else {
        None
    }
}

/// 完成一次主动搭话：成功/跳过进入新随机周期；可恢复错误最多重试两次。
pub(crate) fn finish_proactive(
    state: &mut CompanionState,
    generation: u64,
    outcome: ProactiveOutcome,
) {
    if !state.active || state.stop_generation != generation {
        return;
    }
    state.proactive_in_flight = false;
    let now = now_unix();
    match outcome {
        ProactiveOutcome::Success => {
            state.last_proactive_result = Some("上一次主动搭话已发送".to_string());
            schedule_next_proactive(state, now);
        }
        ProactiveOutcome::Skipped(reason) => {
            state.last_proactive_result = Some(format!("本轮跳过：{reason}"));
            schedule_next_proactive(state, now);
        }
        ProactiveOutcome::Retryable(reason) => {
            let attempt = state.proactive_retry_count as usize;
            if let Some(delay) = PROACTIVE_RETRY_DELAYS_SECS.get(attempt) {
                state.proactive_retry_count += 1;
                state.next_proactive_at = Some(now + delay);
                state.last_proactive_result =
                    Some(format!("主动搭话失败，{} 秒后重试：{reason}", delay));
            } else {
                state.last_proactive_result = Some(format!("本轮失败：{reason}"));
                schedule_next_proactive(state, now);
            }
        }
    }
}

/// 停止陪伴：置 active=false 并递增停止代次（取消进行中的陪伴侧异步链）。
fn halt_companion(state: &mut CompanionState) {
    state.active = false;
    state.next_proactive_at = None;
    state.proactive_in_flight = false;
    state.proactive_retry_count = 0;
    state.last_proactive_result = Some("陪伴已停止，已取消主动搭话计划".to_string());
    state.stop_generation = state.stop_generation.wrapping_add(1);
}

/// 读取当前停止代次（供陪伴侧异步链在开始时快照）。
pub(crate) fn companion_stop_generation(session: &CompanionSession) -> u64 {
    session
        .0
        .lock()
        .map(|state| state.stop_generation)
        .unwrap_or(0)
}

/// 陪伴链是否仍存活：陪伴开启且停止代次未变（即未被双击/命令取消）。
pub(crate) fn companion_chain_alive(session: &CompanionSession, generation: u64) -> bool {
    session
        .0
        .lock()
        .map(|state| state.active && state.stop_generation == generation)
        .unwrap_or(false)
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct PetState {
    locked: bool,
    position: Option<(i32, i32)>,
    last_active_at: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Message {
    role: String,
    content: String,
    created_at: i64,
    source: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct MemoryAtom {
    id: i64,
    kind: String,
    content: String,
    source_excerpt: String,
    confidence: f64,
    created_at: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct MemoryCandidate {
    kind: String,
    content: String,
    source_excerpt: String,
    confidence: f64,
}

#[derive(Deserialize)]
struct MemoryExtraction {
    memories: Vec<MemoryCandidate>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct TodoItem {
    id: i64,
    title: String,
    deadline_local: String,
    remind_before_minutes: i64,
    source_text: String,
    status: String,
    created_at: i64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TodoReminder {
    id: i64,
    message: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
#[cfg(test)]
struct TodoCandidate {
    title: String,
    deadline_local: String,
    remind_before_minutes: i64,
}

#[derive(Deserialize)]
#[cfg(test)]
struct TodoExtraction {
    todo: Option<TodoCandidate>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct PrivacyConfig {
    screen_observation: bool,
    auto_location: bool,
    #[serde(default = "default_proactive_interaction")]
    proactive_interaction: bool,
    #[serde(default)]
    file_search_desktop: bool,
    #[serde(default)]
    file_search_documents: bool,
    #[serde(default)]
    file_search_downloads: bool,
}

fn default_proactive_interaction() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq)]
struct LocationSnapshot {
    latitude: f64,
    longitude: f64,
    accuracy_meters: f64,
    city: Option<String>,
    region: Option<String>,
    country: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum WeatherTime {
    Current,
    DayOffset(u8),
    NextDays(u8),
    #[cfg(test)]
    MonthDay {
        month: u32,
        day: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum WeatherMetric {
    Summary,
    Temperature,
    Precipitation,
}

#[derive(Clone, Debug, PartialEq)]
struct WeatherIntent {
    city: Option<String>,
    time: WeatherTime,
    metric: WeatherMetric,
}

#[derive(Clone, Debug, PartialEq)]
struct ReverseGeocode {
    city: Option<String>,
    district: Option<String>,
    region: Option<String>,
    country: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelProviderSettings {
    provider_name: String,
    base_url: String,
    protocol: String,
    model: String,
    monthly_budget_usd: f64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelProviderInput {
    provider_name: String,
    base_url: String,
    protocol: String,
    model: String,
    api_key: String,
    monthly_budget_usd: f64,
}

#[derive(Debug, PartialEq)]
struct ModelRequest {
    url: String,
    fallback_url: Option<String>,
    body: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentToolCall {
    id: String,
    name: String,
    arguments: serde_json::Value,
}

/// 工具调用循环内部使用的对话单元：普通消息、原生工具调用回填与工具结果。
#[derive(Clone, Debug)]
enum AgentContextItem {
    Plain(Message),
    NativeCalls(Vec<AgentToolCall>),
    NativeResult { call_id: String, output: String },
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ToolCallMode {
    Native,
    JsonPlan,
}

const AGENT_TOOL_POLICY: &str = "你可以自行决定是否调用运行时工具。凡涉及当前时间、用户当前位置、实时或未来天气等时效事实，必须调用对应工具，不得依靠训练数据猜测。凡涉及当前屏幕内容（能否看到屏幕、用户在做什么、要求观察屏幕），必须调用 get_screen_observation，不得凭空猜测屏幕内容。凡涉及创建待办、提醒或日程：只有事项、明确日期和明确时间都齐全时才能调用 prepare_todo_review；相对日期必须先调用 get_current_datetime 消解；日期、时间或事项任何一项不清楚时，直接追问缺失项，不得调用工具、不得猜测、不得声称已创建。prepare_todo_review 成功只表示已生成待审核草稿并打开待办面板，用户在面板确认后才是真正创建。用户询问有没有新邮件、今天新增邮件、未读邮件数量或收件箱状态时，必须调用 get_inbox_status，不得凭空声称无法查看或猜测数量；该工具只读取账户、数量和邮件标识，不读取正文。用户进一步询问今天的新邮件来自谁或主题是什么时，必须调用 get_recent_mail_senders；最多读取最近 5 封的发件人与主题，不得读取正文。用户询问刚才或最近一封邮件是否发送成功时，必须调用 get_latest_email_status，不得根据聊天内容猜测。凡涉及准备邮件：必须取得明确收件邮箱、主题与正文；信息不足时一次合并追问，最多三轮，不得猜测地址。用户拖入的文件只授权当前对话使用；作为附件时必须把系统上下文给出的 attachmentCandidateId 原样传给 prepare_email_review，严禁修改文件内容。信息完整后只能调用 prepare_email_review 打开审核卡，模型没有发送工具，必须由用户在审核卡亲自点击发送。凡涉及把待办标记为完成，必须调用 mark_todo_completed，不得声称已完成。凡涉及桌宠是否允许移动（用户说“不要打扰我/呆在那里别动/不许动”或“可以动了/允许移动/自由移动/开始自由移动/到处走走”），必须调用 set_movement_allowed 切换真实状态，不得只口头承诺。凡涉及回忆或查找本地文件：有明确文件名时调用 find_local_files；提到过去打开、编辑或某个时间点时，先调用 recall_file_activity 查询 Cyral 的本地观察记录，再调用 find_local_files 定位。用户明确要求打开文件时，必须先用 find_local_files 取得程序生成的一次性 openCandidateId，再调用 open_file_candidate；没有该 ID 或存在多项候选时不得猜选。用户明确要求打开软件时，先调用 find_installed_apps；只有结果提供 openCandidateId 才能调用 open_app_candidate，多项候选时让用户缩小范围。打开动作必须来自当前用户对话，主动关心或后台任务不得触发。不得要求或接受绝对路径。每轮合并询问缺失信息，最多追问三轮。点击播放、覆盖保存、删除、安装、支付、提交和命令执行必须由用户亲自完成，当前没有这些执行工具。工具失败时如实说明，不得编造结果。已有工具结果会以系统消息返回；必要时可以继续调用下一项工具。没有工具调用或已获得工具结果后，只能输出 <cyral_final>用户可见正文</cyral_final> 或 {\"final\":\"用户可见正文\"}；不得输出分析、推理、计划、工具 JSON 或任何内部过程。";
const FALLBACK_TOOL_PROTOCOL: &str = "如果需要调用工具，只输出严格 JSON：{\"toolCalls\":[{\"id\":\"call_1\",\"name\":\"工具名\",\"arguments\":{}}]}，不要输出解释、标签或占位符正文。如果不需要工具或已经获得足够工具结果，严格遵循 <cyral_final>最终正文</cyral_final> 输出协议。";

fn agent_tool_definitions(protocol: &str) -> serde_json::Value {
    let tools = [
        (
            "get_current_datetime",
            "获取用户电脑当前的日期、时间、星期和时区。查询当前时间或计算相对日期时使用。",
            serde_json::json!({"type":"object","properties":{},"additionalProperties":false}),
        ),
        (
            "get_current_location",
            "在用户已授权时获取当前位置的经纬度、精度和可选可读地名。用户说这里、附近、当前位置时使用。",
            serde_json::json!({"type":"object","properties":{},"additionalProperties":false}),
        ),
        (
            "get_weather",
            "按城市或经纬度查询真实天气。当前位置天气应先调用 get_current_location，再把坐标传给本工具。",
            serde_json::json!({
                "type":"object",
                "properties":{
                    "city":{"type":"string","description":"明确城市；与经纬度二选一"},
                    "latitude":{"type":"number"},
                    "longitude":{"type":"number"},
                    "use_current_location":{"type":"boolean","description":"用户说这里、附近或当前位置时设为 true"},
                    "date":{"type":"string","description":"current、today、tomorrow、YYYY-MM-DD；省略表示 current"},
                    "days":{"type":"integer","minimum":1,"maximum":7},
                    "metric":{"type":"string","enum":["summary","temperature","precipitation"]}
                },
                "additionalProperties":false
            }),
        ),
        (
            "get_screen_observation",
            "立即观察当前屏幕并返回观察结果（应用、文件、窗口标题、场景摘要）。当用户询问能否看到屏幕、问你在做什么、或要求观察屏幕时使用；仅当陪伴模式开启且屏幕观察授权开启时可用，否则如实返回不可用原因。",
            serde_json::json!({"type":"object","properties":{},"additionalProperties":false}),
        ),
        (
            "prepare_todo_review",
            "将信息完整的待办保存为待审核草稿，并自动打开待办审核面板。仅当事项、明确日期和明确时间都已确定时调用；成功后必须告诉用户去审核确认。",
            serde_json::json!({
                "type":"object",
                "properties":{
                    "title":{"type":"string","description":"简洁、可执行的待办事项"},
                    "deadlineLocal":{"type":"string","description":"本地截止时间，严格 YYYY-MM-DDTHH:mm"}
                },
                "required":["title","deadlineLocal"],
                "additionalProperties":false
            }),
        ),        (
            "mark_todo_completed",
            "当用户表示完成/做完某项待办时，按标题把对应待办标记为已完成。标题不明确或匹配到多项时返回候选，由用户确认具体是哪一项。",
            serde_json::json!({
                "type":"object",
                "properties":{
                    "title":{"type":"string","description":"用户提到的待办标题"}
                },
                "additionalProperties":false
            }),
        ),

        (
            "recall_file_activity",
            "从 Cyral 最近 30 天的本地屏幕观察记录中回忆曾打开的文件名、应用和观察时间。用户提到过去某时编辑或打开过文件时先调用；不返回路径，不读取文件内容。",
            serde_json::json!({
                "type":"object",
                "properties":{
                    "query":{"type":"string","description":"可选文件名或主题关键词，不含路径","maxLength":80},
                    "extension":{"type":"string","description":"可选扩展名，如 pptx、pdf、docx；不含点","maxLength":16},
                    "observedAfter":{"type":"integer","description":"可选起始 Unix 秒时间戳"},
                    "observedBefore":{"type":"integer","description":"可选结束 Unix 秒时间戳"}
                },
                "additionalProperties":false
            }),
        ),
        (
            "find_local_files",
            "仅在用户于隐私设置明确授权的桌面、文档、下载目录内，按文件名查找本地文件。只接收关键词和可选扩展名，绝不接收或使用路径；不会读取文件内容。",
            serde_json::json!({
                "type":"object",
                "properties":{
                    "query":{"type":"string","description":"文件名中的关键词，不含路径","minLength":1,"maxLength":80},
                    "extension":{"type":"string","description":"可选扩展名，如 pptx、pdf、docx；不含点","maxLength":16},
                    "observedAfter":{"type":"integer","description":"可选起始 Unix 秒时间戳；用于让该时段内确实观察过的同名文件优先"},
                    "observedBefore":{"type":"integer","description":"可选结束 Unix 秒时间戳"}
                },
                "required":["query"],
                "additionalProperties":false
            }),
        ),
        (
            "find_installed_apps",
            "按软件名称查找 Windows 当前用户或全局开始菜单中的已安装软件。只接受名称，不接受路径；唯一结果会返回一次性 openCandidateId，多项结果必须先让用户缩小范围。",
            serde_json::json!({
                "type":"object",
                "properties":{
                    "query":{"type":"string","description":"软件显示名称，不含路径","minLength":1,"maxLength":80}
                },
                "required":["query"],
                "additionalProperties":false
            }),
        ),
        (
            "open_app_candidate",
            "打开 find_installed_apps 在本轮生成的唯一软件候选。只能使用工具返回的一次性 candidateId；用户未明确要求打开软件时不得调用。",
            serde_json::json!({
                "type":"object",
                "properties":{
                    "candidateId":{"type":"string","description":"find_installed_apps 返回的一次性 openCandidateId","minLength":1,"maxLength":64}
                },
                "required":["candidateId"],
                "additionalProperties":false
            }),
        ),
        (
            "open_file_candidate",
            "用系统默认程序打开 find_local_files 在本轮生成的唯一授权文件候选。只能使用工具返回的一次性 candidateId；用户未明确要求打开文件时不得调用。",
            serde_json::json!({
                "type":"object",
                "properties":{
                    "candidateId":{"type":"string","description":"find_local_files 返回的一次性 openCandidateId","minLength":1,"maxLength":64}
                },
                "required":["candidateId"],
                "additionalProperties":false
            }),
        ),
        (
            "read_dropped_text_file",
            "仅当用户明确要求阅读、总结或分析已拖入的文本/代码文件时，读取该文件内容。只接受当前对话拖拽上下文给出的 candidateId；不支持二进制文件或超过 1 MiB 的文件。",
            serde_json::json!({
                "type":"object",
                "properties":{
                    "candidateId":{"type":"string","description":"当前对话拖拽上下文给出的 attachmentCandidateId","minLength":1,"maxLength":64}
                },
                "required":["candidateId"],
                "additionalProperties":false
            }),
        ),
        (
            "get_inbox_status",
            "立即检查已连接 Gmail，返回当前未读总数、距上次检查新增数和 Cyral 今天观察到的新增数。用户询问新邮件、今日新增、未读邮件或收件箱状态时必须调用；不读取邮件正文。",
            serde_json::json!({"type":"object","properties":{},"additionalProperties":false}),
        ),
        (
            "get_recent_mail_senders",
            "当用户进一步询问今天新增邮件来自谁或主题是什么时，读取今天最近最多 5 封邮件的发件人与主题；不读取正文或附件。",
            serde_json::json!({"type":"object","properties":{},"additionalProperties":false}),
        ),
        (
            "get_latest_email_status",
            "查询最近一封审核邮件的真实发送状态。用户询问刚才的邮件是否发送、发送成功或发送失败时必须调用；该工具不能发送或重试邮件。",
            serde_json::json!({"type":"object","properties":{},"additionalProperties":false}),
        ),
        (
            "prepare_email_review",
            "准备一封待用户审核的邮件并打开审核卡。只有明确收件邮箱、主题和正文后才能调用；成功只表示等待审核，绝不表示已经发送。",
            serde_json::json!({
                "type":"object",
                "properties":{
                    "recipients":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":20,"description":"明确的完整邮箱地址"},
                    "subject":{"type":"string","minLength":1,"maxLength":200},
                    "body":{"type":"string","minLength":1,"maxLength":100000},
                    "attachmentCandidateIds":{"type":"array","items":{"type":"string"},"maxItems":10,"description":"仅使用本轮文件查找或拖拽上下文返回、且用户明确指定的文件候选 ID"}
                },
                "required":["recipients","subject","body"],
                "additionalProperties":false
            }),
        ),
        (
            "set_movement_allowed",
            "切换桌宠是否允许在桌面自动移动。用户明确说“不要打扰我/呆在那里别动/不许动”时 enabled=false（原地待机）；用户说“可以动了/允许移动/自由移动/开始自由移动/到处走走”时 enabled=true。用户没有明确表达移动意愿时不要调用。",
            serde_json::json!({
                "type":"object",
                "properties":{
                    "enabled":{"type":"boolean","description":"true=允许自动移动；false=原地待机不移动"}
                },
                "additionalProperties":false
            }),
        ),
    ];
    serde_json::Value::Array(
        tools
            .into_iter()
            .map(|(name, description, parameters)| {
                if protocol == "responses" {
                    serde_json::json!({
                        "type":"function",
                        "name":name,
                        "description":description,
                        "parameters":parameters,
                        "strict":true
                    })
                } else {
                    serde_json::json!({
                        "type":"function",
                        "function":{
                            "name":name,
                            "description":description,
                            "parameters":parameters,
                            "strict":true
                        }
                    })
                }
            })
            .collect(),
    )
}

fn parse_tool_arguments(raw: &str) -> Result<serde_json::Value, String> {
    serde_json::from_str(raw).map_err(|error| format!("模型返回了无效工具参数：{error}"))
}

fn extract_native_tool_calls(
    protocol: &str,
    response: &serde_json::Value,
) -> Result<Vec<AgentToolCall>, String> {
    let mut calls = Vec::new();
    if protocol == "responses" {
        for item in response["output"].as_array().into_iter().flatten() {
            if item["type"].as_str() != Some("function_call") {
                continue;
            }
            let name = item["name"].as_str().ok_or("工具调用缺少名称")?;
            let arguments = item["arguments"].as_str().unwrap_or("{}");
            calls.push(AgentToolCall {
                id: item["call_id"].as_str().unwrap_or(name).to_string(),
                name: name.to_string(),
                arguments: parse_tool_arguments(arguments)?,
            });
        }
    } else {
        for item in response["choices"][0]["message"]["tool_calls"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let name = item["function"]["name"]
                .as_str()
                .ok_or("工具调用缺少名称")?;
            let arguments = item["function"]["arguments"].as_str().unwrap_or("{}");
            calls.push(AgentToolCall {
                id: item["id"].as_str().unwrap_or(name).to_string(),
                name: name.to_string(),
                arguments: parse_tool_arguments(arguments)?,
            });
        }
    }
    Ok(calls)
}

fn parse_json_tool_plan(raw: &str) -> Result<Vec<AgentToolCall>, String> {
    for (start, _) in raw.match_indices('{').rev() {
        let mut stream =
            serde_json::Deserializer::from_str(&raw[start..]).into_iter::<serde_json::Value>();
        if let Some(Ok(value)) = stream.next() {
            if let Some(calls) = value.get("toolCalls").and_then(serde_json::Value::as_array) {
                return calls
                    .iter()
                    .enumerate()
                    .map(|(index, call)| {
                        Ok(AgentToolCall {
                            id: call["id"]
                                .as_str()
                                .map(ToOwned::to_owned)
                                .unwrap_or_else(|| format!("fallback_{}", index + 1)),
                            name: call["name"].as_str().ok_or("工具计划缺少名称")?.to_string(),
                            arguments: call
                                .get("arguments")
                                .cloned()
                                .unwrap_or_else(|| serde_json::json!({})),
                        })
                    })
                    .collect();
            }
        }
    }
    Err("模型没有返回可执行的工具计划".to_string())
}

fn build_provider_candidates(
    provider_name: &str,
    base_url: &str,
    protocol: &str,
    model: &str,
    monthly_budget_usd: f64,
) -> Vec<ModelProviderSettings> {
    let provider_name = if provider_name.trim().is_empty() {
        "自定义供应商"
    } else {
        provider_name.trim()
    };
    let base = base_url.trim().trim_end_matches('/');
    let mut bases = vec![base.to_string()];
    if base.ends_with("/v1") {
        bases.push(base.trim_end_matches("/v1").to_string());
    } else if !base.ends_with("/v1")
        && !base.ends_with("/responses")
        && !base.ends_with("/chat/completions")
    {
        bases.push(format!("{base}/v1"));
    }
    let protocols: &[&str] = match protocol {
        "responses" => &["responses"],
        "chat_completions" => &["chat_completions"],
        _ => &["chat_completions", "responses"],
    };
    let mut candidates = Vec::new();
    for candidate_protocol in protocols {
        for candidate_base in &bases {
            let candidate = ModelProviderSettings {
                provider_name: provider_name.to_string(),
                base_url: candidate_base.clone(),
                protocol: (*candidate_protocol).to_string(),
                model: model.trim().to_string(),
                monthly_budget_usd,
            };
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
    }
    candidates
}

fn is_endpoint_compatibility_error(error: &str) -> bool {
    error.contains("无法识别的数据") || error.contains("HTTP 404")
}

#[cfg(test)]
fn is_retryable_stream_error(error: &str) -> bool {
    error.contains("读取流式响应失败")
        || error.contains("error decoding response body")
        || error.contains("connection closed before message completed")
}

enum ProbeFailure {
    Retryable(String),
    Fatal(String),
}

fn apply_model_auth(
    request: reqwest::RequestBuilder,
    api_key: &str,
    url: &str,
) -> reqwest::RequestBuilder {
    if api_key.trim().is_empty() && is_loopback_model_url(url) {
        request
    } else {
        request.bearer_auth(api_key.trim())
    }
}

async fn probe_provider(
    client: &reqwest::Client,
    provider: &ModelProviderSettings,
    api_key: &str,
) -> Result<(), ProbeFailure> {
    let messages = vec![Message {
        role: "user".to_string(),
        content: "Reply OK".to_string(),
        created_at: 0,
        source: "chat".to_string(),
    }];
    let mut request = build_model_request(provider, &messages).map_err(ProbeFailure::Fatal)?;
    request.body["stream"] = serde_json::Value::Bool(false);
    if provider.protocol == "responses" {
        request.body["max_output_tokens"] = serde_json::json!(1);
    } else {
        request.body["max_tokens"] = serde_json::json!(1);
    }
    let response = apply_model_auth(client.post(&request.url), api_key, &request.url)
        .json(&request.body)
        .send()
        .await
        .map_err(|error| {
            ProbeFailure::Retryable(format_error_chain("无法连接模型供应商", &error))
        })?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let detail = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|json| {
            json["error"]["message"]
                .as_str()
                .or_else(|| json["message"].as_str())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| status.to_string());
    match status.as_u16() {
        200..=299 if !body.trim_start().starts_with('<') => Ok(()),
        401 | 403 => Err(ProbeFailure::Fatal(format!(
            "API Key 无效或无权访问：{detail}"
        ))),
        402 => Err(ProbeFailure::Fatal(format!("账户余额不足：{detail}"))),
        429 => Err(ProbeFailure::Fatal(format!(
            "请求过于频繁或额度受限：{detail}"
        ))),
        400 if detail.to_lowercase().contains("model") => {
            Err(ProbeFailure::Fatal(format!("模型名称不可用：{detail}")))
        }
        _ => Err(ProbeFailure::Retryable(format!(
            "{}：{detail}",
            request.url
        ))),
    }
}

fn build_model_request(
    provider: &ModelProviderSettings,
    messages: &[Message],
) -> Result<ModelRequest, String> {
    let base_url = provider.base_url.trim_end_matches('/');
    let prepared_messages: Vec<_> = messages
        .iter()
        .map(|message| (&message.role, message.content.clone()))
        .collect();
    let (endpoint, body) =
        match provider.protocol.as_str() {
            "responses" => {
                let input: Vec<_> = prepared_messages
                    .iter()
                    .map(|(role, content)| serde_json::json!({ "role": role, "content": content }))
                    .collect();
                (
                    "responses",
                    serde_json::json!({
                        "model": provider.model,
                        "instructions": CYRAL_CORE_BEHAVIOR,
                        "input": input,
                        "max_output_tokens": 1024,
                        "stream": true,
                    }),
                )
            }
            "chat_completions" => {
                let mut api_messages =
                    vec![serde_json::json!({ "role": "system", "content": CYRAL_CORE_BEHAVIOR })];
                api_messages.extend(prepared_messages.iter().map(
                    |(role, content)| serde_json::json!({ "role": role, "content": content }),
                ));
                (
                    "chat/completions",
                    serde_json::json!({
                        "model": provider.model,
                        "messages": api_messages,
                        "max_tokens": 1024,
                        "stream": true,
                    }),
                )
            }
            _ => return Err("不支持当前接口协议，请重新保存模型配置".to_string()),
        };
    let url = if base_url.ends_with(endpoint) {
        base_url.to_string()
    } else {
        format!("{base_url}/{endpoint}")
    };
    let base_has_path = base_url
        .split_once("://")
        .map(|(_, remainder)| remainder.contains('/'))
        .unwrap_or(true);
    let fallback_url = (!base_has_path).then(|| format!("{base_url}/v1/{endpoint}"));
    Ok(ModelRequest {
        url,
        fallback_url,
        body,
    })
}

/// 按协议序列化工具调用循环上下文为请求正文的 messages/input 数组。
/// 原生工具路径遵循供应商协议（chat tool_call/tool、responses function_call/function_call_output），
/// 严格 JSON 规划回退保持 system 文本结果消息；两条路径共享同一工具结果语义载荷。
fn serialize_agent_history(
    protocol: &str,
    context: &[AgentContextItem],
) -> Result<Vec<serde_json::Value>, String> {
    let mut items = Vec::new();
    for item in context {
        match item {
            AgentContextItem::Plain(message) => {
                items.push(serde_json::json!({"role": message.role, "content": message.content}));
            }
            AgentContextItem::NativeCalls(calls) => {
                if protocol == "responses" {
                    items.extend(calls.iter().map(|call| {
                        serde_json::json!({
                            "type": "function_call",
                            "call_id": call.id,
                            "name": call.name,
                            "arguments": call.arguments.to_string(),
                        })
                    }));
                } else {
                    items.push(serde_json::json!({
                        "role": "assistant",
                        "content": serde_json::Value::Null,
                        "tool_calls": calls
                            .iter()
                            .map(|call| {
                                serde_json::json!({
                                    "id": call.id,
                                    "type": "function",
                                    "function": {
                                        "name": call.name,
                                        "arguments": call.arguments.to_string(),
                                    },
                                })
                            })
                            .collect::<Vec<_>>(),
                    }));
                }
            }
            AgentContextItem::NativeResult { call_id, output } => {
                if protocol == "responses" {
                    items.push(serde_json::json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": output,
                    }));
                } else {
                    items.push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": output,
                    }));
                }
            }
        }
    }
    Ok(items)
}

fn build_agent_request(
    provider: &ModelProviderSettings,
    context: &[AgentContextItem],
    native_tools: bool,
) -> Result<ModelRequest, String> {
    // 复用 build_model_request 的 URL/端点骨架，再以协议序列化覆盖消息部分。
    let mut request = build_model_request(provider, &[])?;
    request.body["stream"] = serde_json::json!(false);
    let policy = if native_tools {
        AGENT_TOOL_POLICY.to_string()
    } else {
        format!(
            "{AGENT_TOOL_POLICY}\n{FALLBACK_TOOL_PROTOCOL}\n可用工具 schema：{}",
            agent_tool_definitions("chat_completions")
        )
    };
    if native_tools {
        request.body["tools"] = agent_tool_definitions(&provider.protocol);
        request.body["tool_choice"] = serde_json::json!("auto");
    }
    let history = serialize_agent_history(&provider.protocol, context)?;
    match provider.protocol.as_str() {
        "responses" => {
            request.body["instructions"] =
                serde_json::json!(format!("{}\n\n{}", CYRAL_CORE_BEHAVIOR, policy));
            request.body["input"] = serde_json::Value::Array(history);
        }
        "chat_completions" => {
            let mut messages = vec![
                serde_json::json!({"role": "system", "content": CYRAL_CORE_BEHAVIOR}),
                serde_json::json!({"role": "system", "content": policy}),
            ];
            messages.extend(history);
            request.body["messages"] = serde_json::Value::Array(messages);
        }
        _ => return Err("不支持当前接口协议".to_string()),
    }
    Ok(request)
}

fn extract_model_reply(protocol: &str, response: &serde_json::Value) -> Result<String, String> {
    let protocol_reply = match protocol {
        "responses" => response
            .get("output_text")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| {
                response["output"].as_array().map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item["content"].as_array())
                        .flatten()
                        .filter(|content| content["type"] == "output_text")
                        .filter_map(|content| content["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
            }),
        "chat_completions" => response["choices"][0]["message"]["content"]
            .as_str()
            .map(ToOwned::to_owned),
        _ => None,
    };
    let reply = protocol_reply
        .or_else(|| {
            response["choices"][0]["message"]["content"]
                .as_str()
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            response
                .get("output_text")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_default();
    if reply.trim().is_empty() {
        Err("模型返回成功，但没有可显示的文本".to_string())
    } else {
        Ok(reply.trim().to_string())
    }
}

/// 只读取协议定义的用户可见正文槽位。这里不接受协议间的兼容回退，避免把未知字段、
/// 推理字段或原始响应误当成最终正文。
fn extract_protocol_visible_reply(
    protocol: &str,
    response: &serde_json::Value,
) -> Result<String, String> {
    let reply = match protocol {
        "responses" => response
            .get("output_text")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| {
                response["output"].as_array().map(|items| {
                    items
                        .iter()
                        .filter(|item| item["type"] == "message")
                        .filter_map(|item| item["content"].as_array())
                        .flatten()
                        .filter(|content| content["type"] == "output_text")
                        .filter_map(|content| content["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
            }),
        "chat_completions" => response["choices"][0]["message"]["content"]
            .as_str()
            .map(ToOwned::to_owned),
        _ => None,
    }
    .filter(|text| !text.trim().is_empty())
    .ok_or_else(|| "模型返回成功，但没有可显示的文本".to_string())?;

    clean_normalized_text(&reply)
        .ok_or_else(|| "模型没有返回可安全显示的最终正文，请重试".to_string())
}
fn extract_cyral_final(output: &str) -> Result<String, String> {
    const OPEN: &str = "<cyral_final>";
    const CLOSE: &str = "</cyral_final>";
    let start = output
        .rfind(OPEN)
        .map(|index| index + OPEN.len())
        .ok_or_else(|| "模型没有返回可安全显示的最终正文，请重试".to_string())?;
    let end = output[start..]
        .find(CLOSE)
        .map(|index| start + index)
        .ok_or_else(|| "模型没有返回可安全显示的最终正文，请重试".to_string())?;
    let final_answer = output[start..end].trim();
    if final_answer.is_empty() {
        Err("模型没有返回可安全显示的最终正文，请重试".to_string())
    } else {
        Ok(final_answer.to_string())
    }
}

fn extract_final_json(output: &str) -> Result<String, String> {
    for (start, _) in output.match_indices('{').rev() {
        let candidate = &output[start..];
        let mut stream =
            serde_json::Deserializer::from_str(candidate).into_iter::<serde_json::Value>();
        if let Some(Ok(value)) = stream.next() {
            if let Some(final_answer) = value["final"]
                .as_str()
                .map(str::trim)
                .filter(|answer| !answer.is_empty())
            {
                return Ok(final_answer.to_string());
            }
        }
    }
    Err("模型没有返回可安全显示的最终正文，请重试".to_string())
}

fn extract_normalized_final(output: &str) -> Result<String, String> {
    extract_cyral_final(output).or_else(|_| extract_final_json(output))
}

/// 清理整理器输出：去代码块围栏、去外层引号，并拒绝空壳占位（尖括号占位说明、
/// 空文本、历史泄漏字面"最终答案"）。
fn clean_normalized_text(text: &str) -> Option<String> {
    let mut cleaned = text.trim().to_string();
    for _ in 0..2 {
        if cleaned.starts_with("```") {
            cleaned = cleaned[3..].trim().to_string();
        }
        if cleaned.ends_with("```") {
            cleaned = cleaned[..cleaned.len() - 3].trim().to_string();
        }
        if cleaned.len() >= 2 && cleaned.starts_with('"') && cleaned.ends_with('"') {
            cleaned = cleaned[1..cleaned.len() - 1].trim().to_string();
        }
    }
    // 历史泄漏样式：模型以"最终答案："开头时剥掉标记，保留正文。
    for prefix in ["最终答案：", "最终答案:", "最终答案 "] {
        if let Some(rest) = cleaned.strip_prefix(prefix) {
            cleaned = rest.trim().to_string();
            break;
        }
    }
    if cleaned.is_empty() {
        return None;
    }
    // 指令复述/分析回声：模型把整理任务或指令本身当成了输出。
    const INSTRUCTION_ECHO_FRAGMENTS: &[&str] = &[
        "从下面的模型输出中提取真正回复用户的最终正文",
        "The user is asking me to extract",
        "这里给的是\"模型输出\"吗",
        "So the body text is simply",
        "They want me to",
        "We need to understand",
        "We need to respond",
        "We need to answer",
        "I need to correct",
        "The user's message is",
        "因此输出应为",
        "我们需要理解指令",
        "我们需要先分析",
        "先分析用户",
    ];
    if INSTRUCTION_ECHO_FRAGMENTS
        .iter()
        .any(|fragment| cleaned.contains(fragment))
    {
        return None;
    }
    // 整体是一个尖括号占位说明（如 "<这里填写提取出的正文>"）。
    if cleaned.starts_with('<') && cleaned.ends_with('>') && cleaned.len() <= 40 {
        return None;
    }
    if cleaned == "最终答案" {
        return None;
    }
    Some(cleaned)
}

/// 从整理器输出中提取可展示正文：只接受标签或 JSON 的结构化最终正文。
/// 禁止从整段、末段或末行猜测，否则推理文本会被误当成用户可见正文。
#[cfg(test)]
fn extract_final_text_from_normalizer(reply: &str) -> Option<String> {
    extract_normalized_final(reply)
        .ok()
        .and_then(|text| clean_normalized_text(&text))
}

/// API 的 content/output_text 是协议定义的可见正文槽位；推理槽位已在上游过滤。
/// 标签或 final JSON 仍优先，兼容不遵循标签指令但正确区分正文槽位的供应商。
fn extract_safe_visible_reply(reply: &str) -> Result<String, String> {
    let text = extract_normalized_final(reply)?;
    clean_normalized_text(&text)
        .ok_or_else(|| "模型没有返回可安全显示的最终正文，请重试".to_string())
}

#[cfg(test)]
fn extract_stream_delta(protocol: &str, event: &serde_json::Value) -> Option<String> {
    let protocol_delta = match protocol {
        "responses" if event["type"] == "response.output_text.delta" => {
            event["delta"].as_str().map(ToOwned::to_owned)
        }
        "chat_completions" => event["choices"][0]["delta"]["content"]
            .as_str()
            .map(ToOwned::to_owned),
        _ => None,
    };
    let event_type = event.get("type").and_then(serde_json::Value::as_str);
    let generic_delta_is_visible = event_type
        .map(|kind| kind.contains("output_text") || kind.contains("content_block_delta"))
        .unwrap_or(true);
    protocol_delta
        .or_else(|| {
            event["choices"][0]["delta"]["content"]
                .as_str()
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            generic_delta_is_visible
                .then(|| event["delta"].as_str().map(ToOwned::to_owned))
                .flatten()
        })
        .or_else(|| event["text"].as_str().map(ToOwned::to_owned))
}

#[cfg(test)]
fn parse_sse_data(protocol: &str, data: &str) -> Option<String> {
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return None;
    }
    serde_json::from_str(data)
        .ok()
        .and_then(|event| extract_stream_delta(protocol, &event))
}

#[cfg(test)]
fn parse_sse_line(protocol: &str, line: &str) -> Option<String> {
    let line = line.trim();
    line.strip_prefix("data:")
        .map(str::trim_start)
        .or_else(|| line.starts_with('{').then_some(line))
        .and_then(|data| parse_sse_data(protocol, data))
}

#[allow(dead_code)]
#[cfg(test)]
fn take_sse_event(buffer: &mut String) -> Option<String> {
    let (boundary, delimiter_length) = match (buffer.find("\n\n"), buffer.find("\r\n\r\n")) {
        (Some(lf), Some(crlf)) if lf < crlf => (lf, 2),
        (Some(_), Some(crlf)) => (crlf, 4),
        (Some(lf), None) => (lf, 2),
        (None, Some(crlf)) => (crlf, 4),
        (None, None) => return None,
    };
    let event = buffer[..boundary].replace('\r', "");
    buffer.drain(..boundary + delimiter_length);
    Some(event)
}

#[cfg(test)]
fn extract_sse_reply(protocol: &str, body: &str) -> Option<String> {
    let reply = body
        .lines()
        .filter_map(|line| parse_sse_line(protocol, line))
        .collect::<String>();
    (!reply.is_empty()).then_some(reply)
}

#[cfg(test)]
#[allow(dead_code)]
async fn request_model_stream_once(
    provider: &ModelProviderSettings,
    api_key: &str,
    messages: &[Message],
    _app: &AppHandle,
) -> Result<String, String> {
    let request = build_model_request(provider, messages)?;
    let client = shared_http_client(90);
    let response = apply_model_auth(client.post(&request.url), api_key, &request.url)
        .header(reqwest::header::ACCEPT, "text/event-stream")
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .json(&request.body)
        .send()
        .await
        .map_err(|error| format_error_chain("无法连接模型供应商", &error))?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.map_err(|error| error.to_string())?;
        let json = serde_json::from_str::<serde_json::Value>(&body).ok();
        let detail = json
            .as_ref()
            .and_then(|value| {
                value["error"]["message"]
                    .as_str()
                    .or_else(|| value["message"].as_str())
            })
            .unwrap_or("模型供应商未提供错误详情");
        return Err(format!("模型供应商请求失败（HTTP {status}）：{detail}"));
    }

    let mut stream = response.bytes_stream();
    let mut line_buffer = String::new();
    let mut raw_body = String::new();
    let mut reply = String::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("读取流式响应失败：{error}"))?;
        let text = String::from_utf8_lossy(&chunk);
        raw_body.push_str(&text);
        line_buffer.push_str(&text);
        while let Some(boundary) = line_buffer.find('\n') {
            let line = line_buffer[..boundary].to_string();
            line_buffer.drain(..=boundary);
            if let Some(delta) = parse_sse_line(&provider.protocol, &line) {
                reply.push_str(&delta);
            }
        }
    }
    if let Some(delta) = parse_sse_line(&provider.protocol, &line_buffer) {
        reply.push_str(&delta);
    }
    if reply.trim().is_empty() {
        if let Some(sse_reply) = extract_sse_reply(&provider.protocol, &raw_body) {
            return extract_cyral_final(&sse_reply);
        }
        let json = serde_json::from_str(&raw_body)
            .map_err(|_| "模型返回了无法识别的数据；请确认 Base URL 指向 API".to_string())?;
        let raw = extract_model_reply(&provider.protocol, &json)?;
        extract_cyral_final(&raw)
    } else {
        extract_cyral_final(&reply)
    }
}

#[cfg(test)]
#[allow(dead_code)]
async fn request_model_stream(
    provider: &ModelProviderSettings,
    api_key: &str,
    messages: &[Message],
    app: &AppHandle,
) -> Result<String, String> {
    let first = request_model_stream_once(provider, api_key, messages, app).await;
    match first {
        Err(error) if is_retryable_stream_error(&error) => {
            request_model_stream_once(provider, api_key, messages, app).await
        }
        result => result,
    }
}

/// 构造"普通 JSON 响应"模式的模型请求：复用 build_model_request 的 URL/协议骨架，
/// 但强制 stream=false。非流式读取路径（request_model、chat_plain 等）必须使用
/// 本函数，否则流式供应商会返回 SSE 文本导致 JSON 解析失败。
fn build_plain_json_request(
    provider: &ModelProviderSettings,
    messages: &[Message],
) -> Result<ModelRequest, String> {
    let mut request = build_model_request(provider, messages)?;
    request.body["stream"] = serde_json::json!(false);
    Ok(request)
}

#[allow(dead_code)]
async fn request_model(
    provider: &ModelProviderSettings,
    api_key: &str,
    messages: &[Message],
) -> Result<String, String> {
    let request = build_plain_json_request(provider, messages)?;
    let client = shared_http_client(90);
    let (mut status, mut response_text) =
        send_model_request(&client, &request.url, api_key, &request.body).await?;
    let mut response_json = serde_json::from_str::<serde_json::Value>(&response_text);
    if status.is_success() && response_json.is_err() {
        if let Some(fallback_url) = &request.fallback_url {
            (status, response_text) =
                send_model_request(&client, fallback_url, api_key, &request.body).await?;
            response_json = serde_json::from_str(&response_text);
        }
    }
    let response_json = response_json.map_err(|_| {
        if status.is_success() {
            "模型返回了无法识别的数据（可能为流式响应或网页）；请检查 Base URL 与接口协议配置"
                .to_string()
        } else {
            format!("模型供应商请求失败（HTTP {status}）")
        }
    })?;
    if !status.is_success() {
        let detail = response_json["error"]["message"]
            .as_str()
            .or_else(|| response_json["message"].as_str())
            .unwrap_or("未提供错误详情");
        return Err(format!("模型供应商请求失败（HTTP {status}）：{detail}"));
    }
    extract_model_reply(&provider.protocol, &response_json)
}

/// 轻量单次模型对话，复用 request_model 实现。用于陪伴分析摘要等非对话主路径的
/// 简短模型调用，不携带对话历史、记忆或隐私上下文。
pub(crate) async fn chat_plain(
    provider: &ModelProviderSettings,
    api_key: &str,
    messages: &[Message],
) -> Result<String, String> {
    let raw = request_model(provider, api_key, messages).await?;
    // 只把结构化且可展示的正文发给气泡；解析失败时拒绝展示，绝不猜测末段或末行。
    extract_safe_visible_reply(&raw)
}

#[derive(Debug, PartialEq)]
enum AgentDecision {
    ToolCalls(Vec<AgentToolCall>),
    Final(String),
}

fn extract_agent_decision(
    protocol: &str,
    response: &serde_json::Value,
) -> Result<AgentDecision, String> {
    let native_calls = extract_native_tool_calls(protocol, response)?;
    if !native_calls.is_empty() {
        return Ok(AgentDecision::ToolCalls(native_calls));
    }
    let raw = extract_protocol_visible_reply(protocol, response)?;
    if let Ok(calls) = parse_json_tool_plan(&raw) {
        if !calls.is_empty() {
            return Ok(AgentDecision::ToolCalls(calls));
        }
    }
    let final_answer = extract_normalized_final(&raw)
        .ok()
        .and_then(|text| clean_normalized_text(&text))
        .unwrap_or(raw);
    Ok(AgentDecision::Final(final_answer))
}

async fn send_agent_decision_request(
    provider: &ModelProviderSettings,
    api_key: &str,
    context: &[AgentContextItem],
    native_tools: bool,
) -> Result<AgentDecision, String> {
    let request = build_agent_request(provider, context, native_tools)?;
    let client = shared_http_client(90);
    let (status, body) = send_model_request(&client, &request.url, api_key, &request.body).await?;
    if !status.is_success() {
        let detail = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|value| {
                value["error"]["message"]
                    .as_str()
                    .or_else(|| value["message"].as_str())
                    .map(ToOwned::to_owned)
            })
            .unwrap_or_else(|| "模型供应商未提供错误详情".to_string());
        return Err(format!("模型供应商请求失败（HTTP {status}）：{detail}"));
    }
    let response = serde_json::from_str::<serde_json::Value>(&body)
        .map_err(|_| "模型返回了无法识别的数据；请确认 Base URL 指向 API".to_string())?;
    extract_agent_decision(&provider.protocol, &response)
}

fn should_fallback_from_native_tools(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("http 400")
        && ["tools", "tool_choice", "function calling", "function_call"]
            .iter()
            .any(|marker| lower.contains(marker))
}

async fn request_agent_decision(
    provider: &ModelProviderSettings,
    api_key: &str,
    context: &[AgentContextItem],
) -> Result<(AgentDecision, ToolCallMode), String> {
    match send_agent_decision_request(provider, api_key, context, true).await {
        Ok(decision) => Ok((decision, ToolCallMode::Native)),
        Err(native_error) if should_fallback_from_native_tools(&native_error) => {
            send_agent_decision_request(provider, api_key, context, false)
                .await
                .map(|decision| (decision, ToolCallMode::JsonPlan))
                .map_err(|fallback_error| {
                    format!("工具调用失败：{native_error}；兼容规划也失败：{fallback_error}")
                })
        }
        Err(error) => Err(error),
    }
}

/// 统一的工具结果语义载荷：原生与 JSON 规划回退共用同一内容，仅传输方式不同。
fn tool_result_json(call: &AgentToolCall, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "type":"cyral_tool_result",
        "toolCallId":call.id,
        "tool":call.name,
        "result":result,
        "instruction":"这是程序真实执行的工具结果。根据结果继续调用必要工具，或生成最终正文；不得改写或臆造事实。"
    })
}

fn tool_result_message(call: &AgentToolCall, result: serde_json::Value) -> Message {
    Message {
        role: "system".to_string(),
        content: tool_result_json(call, result).to_string(),
        created_at: 0,
        source: "chat".to_string(),
    }
}

/// 文件名搜索的固定安全预算。目录、路径上限均由本机决定，模型无权覆盖。
const FILE_SEARCH_MAX_RESULTS: usize = 20;
const FILE_SEARCH_MAX_DEPTH: usize = 4;
const FILE_SEARCH_MAX_ENTRIES: usize = 4_000;

#[derive(Clone, Debug)]
struct LocalFileSearchRoot {
    label: &'static str,
    path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalFileMatch {
    root: String,
    relative_path: String,
    name: String,
    extension: String,
    size_bytes: u64,
    modified_at: i64,
    observation_count: usize,
    last_observed_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalFileSearchResult {
    matches: Vec<LocalFileMatch>,
    scan_limited: bool,
    open_candidate_id: Option<String>,
    requires_user_selection: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct FileActivityRecall {
    name: String,
    application: String,
    observation_count: usize,
    first_observed_at: i64,
    last_observed_at: i64,
}

fn timestamp_in_window(timestamp: i64, after: Option<i64>, before: Option<i64>) -> bool {
    after.is_none_or(|start| timestamp >= start) && before.is_none_or(|end| timestamp <= end)
}

fn recall_file_activity(
    connection: &Connection,
    query: Option<&str>,
    extension: Option<&str>,
    observed_after: Option<i64>,
    observed_before: Option<i64>,
) -> Result<Vec<FileActivityRecall>, String> {
    if observed_after
        .zip(observed_before)
        .is_some_and(|(after, before)| after > before)
    {
        return Err("文件回忆的开始时间不能晚于结束时间".to_string());
    }
    let query = query
        .map(normalized_file_search_query)
        .transpose()?
        .unwrap_or_default();
    let extension = normalized_file_extension(extension)?;
    let events = storage::load_cyral_activity_events(connection, 500)
        .map_err(|error| format!("读取本地活动记录失败：{error}"))?;
    let mut recalled: Vec<FileActivityRecall> = Vec::new();
    for event in events {
        if !timestamp_in_window(event.observed_at, observed_after, observed_before) {
            continue;
        }
        let Some(name) = crate::activity::extract_file_key(event.window_title.as_deref()) else {
            continue;
        };
        let lower_name = name.to_lowercase();
        if !query.is_empty()
            && !lower_name.contains(&query)
            && !event.summary.to_lowercase().contains(&query)
        {
            continue;
        }
        if extension.as_deref().is_some_and(|wanted| {
            name.rsplit_once('.')
                .map(|(_, actual)| !file_extension_matches(wanted, actual))
                .unwrap_or(true)
        }) {
            continue;
        }
        if let Some(existing) = recalled
            .iter_mut()
            .find(|item| item.name.eq_ignore_ascii_case(&name))
        {
            existing.observation_count += 1;
            existing.first_observed_at = existing.first_observed_at.min(event.observed_at);
            if event.observed_at > existing.last_observed_at {
                existing.last_observed_at = event.observed_at;
                existing.application = event.app_name;
            }
        } else {
            recalled.push(FileActivityRecall {
                name,
                application: event.app_name,
                observation_count: 1,
                first_observed_at: event.observed_at,
                last_observed_at: event.observed_at,
            });
        }
    }
    recalled.sort_by(|left, right| {
        right
            .observation_count
            .cmp(&left.observation_count)
            .then_with(|| right.last_observed_at.cmp(&left.last_observed_at))
            .then_with(|| left.name.cmp(&right.name))
    });
    recalled.truncate(10);
    Ok(recalled)
}

fn enrich_and_rank_file_matches(
    connection: &Connection,
    matches: &mut [LocalFileMatch],
    observed_after: Option<i64>,
    observed_before: Option<i64>,
) -> Result<(), String> {
    if observed_after
        .zip(observed_before)
        .is_some_and(|(after, before)| after > before)
    {
        return Err("文件搜索的开始时间不能晚于结束时间".to_string());
    }
    let events = storage::load_cyral_activity_events(connection, 500)
        .map_err(|error| format!("读取本地活动记录失败：{error}"))?;
    for candidate in matches.iter_mut() {
        for event in &events {
            if !timestamp_in_window(event.observed_at, observed_after, observed_before) {
                continue;
            }
            let Some(name) = crate::activity::extract_file_key(event.window_title.as_deref())
            else {
                continue;
            };
            if candidate.name.eq_ignore_ascii_case(&name) {
                candidate.observation_count += 1;
                candidate.last_observed_at = Some(
                    candidate
                        .last_observed_at
                        .map_or(event.observed_at, |known| known.max(event.observed_at)),
                );
            }
        }
    }
    matches.sort_by(|left, right| {
        right
            .observation_count
            .cmp(&left.observation_count)
            .then_with(|| right.last_observed_at.cmp(&left.last_observed_at))
            .then_with(|| right.modified_at.cmp(&left.modified_at))
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    Ok(())
}

fn is_file_recall_request(content: &str) -> bool {
    ["哪里", "在哪", "位置", "找不到", "帮我找", "找一下"]
        .iter()
        .any(|word| content.contains(word))
        && [
            "文件", "文档", "简历", "作业", "ppt", "PPT", "pdf", "PDF", "word", "Word",
        ]
        .iter()
        .any(|word| content.contains(word))
}

fn file_recall_context(messages: &[Message]) -> Option<Message> {
    let start = messages
        .iter()
        .rposition(|message| message.role == "user" && is_file_recall_request(&message.content))?;
    if messages.len().saturating_sub(start) > 8 {
        return None;
    }
    let count = messages[start + 1..]
        .iter()
        .filter(|message| {
            message.role == "assistant"
                && (message.content.contains('？') || message.content.contains('?'))
        })
        .count()
        .min(2);
    let instruction = if count >= 2 {
        "已追问 2 轮；不得继续追问。必须先调用 recall_file_activity，再以最可信文件名调用 find_local_files；线索不完整时也要搜索或如实报告未授权/无结果。"
    } else {
        "信息不足时把文件类型和大概打开时间合并为一个问题；最多追问 2 轮。已有名称则直接调用 find_local_files；提到过去打开或编辑时间时先调用 recall_file_activity。"
    };
    Some(Message {
        role: "system".to_string(),
        content: format!("文件找回流程（程序约束）：已追问 {count} 轮。{instruction} 只转述真实工具结果，不得声称读取了文件内容。"),
        created_at: 0,
        source: "runtime".to_string(),
    })
}

fn file_recall_requires_tool(messages: &[Message]) -> bool {
    file_recall_context(messages).is_some_and(|context| context.content.contains("不得继续追问"))
}

fn is_email_request(content: &str) -> bool {
    ["邮件", "邮箱", "发给", "寄给"]
        .iter()
        .any(|word| content.contains(word))
        && ["发", "写", "寄", "回复"]
            .iter()
            .any(|word| content.contains(word))
}

fn email_clarification_context(messages: &[Message]) -> Option<Message> {
    let start = messages
        .iter()
        .rposition(|message| message.role == "user" && is_email_request(&message.content))?;
    if messages.len().saturating_sub(start) > 12 {
        return None;
    }
    let count = messages[start + 1..]
        .iter()
        .filter(|message| {
            message.role == "assistant"
                && (message.content.contains('？') || message.content.contains('?'))
        })
        .count()
        .min(3);
    let instruction = if count >= 3 {
        "已达到 3 轮追问上限。信息仍不完整时不得继续追问、猜测或调用 prepare_email_review；明确说明本次准备已停止，用户可重新发起并一次性提供收件邮箱、主题和正文。"
    } else {
        "缺少信息时必须把收件邮箱、主题、正文、语气和用户明确提到的附件合并成一次问题；总追问不得超过 3 轮。信息完整后立即调用 prepare_email_review。"
    };
    Some(Message {
        role: "system".to_string(),
        content: format!("邮件准备流程（程序约束）：已追问 {count} 轮。{instruction}"),
        created_at: 0,
        source: "runtime".to_string(),
    })
}

fn normalized_file_search_query(query: &str) -> Result<String, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("文件搜索关键词不能为空".to_string());
    }
    if query.chars().count() > 80
        || query.contains(['/', '\\'])
        || query.chars().any(char::is_control)
    {
        return Err(
            "文件搜索关键词无效，请提供不含路径且不超过 80 个字符的文件名关键词".to_string(),
        );
    }
    Ok(query.to_lowercase())
}

fn file_extension_matches(wanted: &str, actual: &str) -> bool {
    match wanted.to_ascii_lowercase().as_str() {
        "ppt" | "powerpoint" | "演示文稿" => matches!(actual, "ppt" | "pptx"),
        "word" | "文档" => matches!(actual, "doc" | "docx"),
        "excel" | "表格" => matches!(actual, "xls" | "xlsx"),
        other => actual.eq_ignore_ascii_case(other),
    }
}

fn normalized_file_extension(extension: Option<&str>) -> Result<Option<String>, String> {
    let Some(extension) = extension else {
        return Ok(None);
    };
    let extension = extension.trim();
    if extension.is_empty() {
        return Ok(None);
    }
    if extension.chars().count() > 16
        || extension.starts_with('.')
        || extension.contains(['/', '\\'])
        || extension.chars().any(char::is_control)
    {
        return Err("文件扩展名无效，请提供不含点和路径的扩展名".to_string());
    }
    Ok(Some(extension.to_lowercase()))
}

/// 搜索前先规范化每个根目录；后续所有子项都必须解析回该根目录之内。
/// 不读取文件内容、不跟随符号链接，并以深度、条目数和结果数强制收束遍历。
fn search_local_files(
    roots: &[LocalFileSearchRoot],
    query: &str,
    extension: Option<&str>,
) -> Result<LocalFileSearchResult, String> {
    let query = normalized_file_search_query(query)?;
    let extension = normalized_file_extension(extension)?;
    let mut canonical_roots = Vec::new();
    for root in roots {
        let path = root
            .path
            .canonicalize()
            .map_err(|error| format!("无法访问已授权的{}目录：{error}", root.label))?;
        if path.is_dir() && !canonical_roots.iter().any(|(_, known)| known == &path) {
            canonical_roots.push((root.label, path));
        }
    }
    if canonical_roots.is_empty() {
        return Err(
            "没有可访问的已授权目录，请在隐私设置中开启桌面、文档或下载目录后重试".to_string(),
        );
    }

    let mut matches = Vec::new();
    let mut scanned = 0usize;
    let mut scan_limited = false;
    'roots: for (label, root) in canonical_roots {
        let mut directories = vec![(root.clone(), 0usize)];
        while let Some((directory, depth)) = directories.pop() {
            let Ok(entries) = fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                scanned += 1;
                if scanned > FILE_SEARCH_MAX_ENTRIES {
                    scan_limited = true;
                    break 'roots;
                }
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if file_type.is_symlink() {
                    continue;
                }
                let Ok(path) = entry.path().canonicalize() else {
                    continue;
                };
                if !path.starts_with(&root) {
                    continue;
                }
                if file_type.is_dir() {
                    if depth < FILE_SEARCH_MAX_DEPTH {
                        directories.push((path, depth + 1));
                    }
                    continue;
                }
                if !file_type.is_file() {
                    continue;
                }
                let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                if !name.to_lowercase().contains(&query) {
                    continue;
                }
                let actual_extension = path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .unwrap_or_default()
                    .to_lowercase();
                if extension
                    .as_deref()
                    .is_some_and(|wanted| !file_extension_matches(wanted, &actual_extension))
                {
                    continue;
                }
                let Ok(metadata) = fs::metadata(&path) else {
                    continue;
                };
                let relative_path = path
                    .strip_prefix(&root)
                    .ok()
                    .map(|relative| relative.to_string_lossy().to_string())
                    .unwrap_or_default();
                matches.push(LocalFileMatch {
                    root: label.to_string(),
                    relative_path,
                    name: name.to_string(),
                    extension: actual_extension,
                    size_bytes: metadata.len(),
                    modified_at: metadata
                        .modified()
                        .ok()
                        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                        .map(|duration| duration.as_secs() as i64)
                        .unwrap_or_default(),
                    observation_count: 0,
                    last_observed_at: None,
                });
            }
        }
    }
    matches.sort_by(|left, right| {
        right
            .modified_at
            .cmp(&left.modified_at)
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    matches.truncate(FILE_SEARCH_MAX_RESULTS);
    Ok(LocalFileSearchResult {
        requires_user_selection: matches.len() > 1,
        matches,
        scan_limited,
        open_candidate_id: None,
    })
}

fn authorized_file_search_roots(
    app: &AppHandle,
    privacy: &PrivacyConfig,
) -> Result<Vec<LocalFileSearchRoot>, String> {
    let mut roots = Vec::new();
    if privacy.file_search_desktop {
        roots.push(LocalFileSearchRoot {
            label: "桌面",
            path: app
                .path()
                .desktop_dir()
                .map_err(|error| format!("无法解析桌面目录：{error}"))?,
        });
    }
    if privacy.file_search_documents {
        roots.push(LocalFileSearchRoot {
            label: "文档",
            path: app
                .path()
                .document_dir()
                .map_err(|error| format!("无法解析文档目录：{error}"))?,
        });
    }
    if privacy.file_search_downloads {
        roots.push(LocalFileSearchRoot {
            label: "下载",
            path: app
                .path()
                .download_dir()
                .map_err(|error| format!("无法解析下载目录：{error}"))?,
        });
    }
    if roots.is_empty() {
        return Err(
            "本地文件搜索尚未授权；请用户在隐私设置中开启桌面、文档或下载目录后保存".to_string(),
        );
    }
    Ok(roots)
}

fn register_unique_file_open_candidate(
    roots: &[LocalFileSearchRoot],
    result: &mut LocalFileSearchResult,
    candidates: &Mutex<windows_actions::CandidateVault>,
) -> Result<(), String> {
    result.requires_user_selection = result.matches.len() > 1 || result.scan_limited;
    result.open_candidate_id = None;
    if result.matches.len() != 1 || result.scan_limited {
        return Ok(());
    }
    let matched = &result.matches[0];
    let root = roots
        .iter()
        .find(|root| root.label == matched.root)
        .ok_or("文件候选无法对应到授权目录")?;
    let authorized_roots = roots
        .iter()
        .map(|root| root.path.clone())
        .collect::<Vec<_>>();
    let candidate_id = candidates
        .lock()
        .map_err(|error| error.to_string())?
        .register_authorized_file(
            &authorized_roots,
            &root.path.join(&matched.relative_path),
            &matched.name,
        )?;
    result.open_candidate_id = Some(candidate_id);
    Ok(())
}
/// 工具执行所需的运行时上下文：应用句柄、陪伴会话、隐私授权、数据库。
struct ToolRuntime<'a> {
    app: &'a AppHandle,
    companion: &'a CompanionSession,
    privacy: &'a PrivacyConfig,
    database: &'a Database,
    pet_motion: &'a PetMotionState,
    execution_origin: execution::ExecutionOrigin,
    execution_candidates: &'a Mutex<windows_actions::CandidateVault>,
    email_tasks: &'a EmailTasks,
    user_message: &'a str,
}

fn begin_audited_execution(
    runtime: &ToolRuntime<'_>,
    action: execution::DesktopAction,
    task_kind: &str,
    target_label: &str,
) -> Result<(execution::ExecutionTask, i64), String> {
    let mut task = execution::ExecutionTask::new_for_user_request(
        runtime.execution_origin,
        action,
        runtime.user_message,
    )
    .map_err(|error| error.to_string())?;
    task.start().map_err(|error| error.to_string())?;
    let connection = runtime
        .database
        .0
        .lock()
        .map_err(|error| error.to_string())?;
    let run_id =
        storage::record_execution_started(&connection, task_kind, target_label, now_unix())
            .map_err(|error| format!("无法记录桌面任务：{error}"))?;
    Ok((task, run_id))
}

fn finish_audited_execution(
    runtime: &ToolRuntime<'_>,
    run_id: i64,
    task: &mut execution::ExecutionTask,
    succeeded: bool,
) -> Result<(), String> {
    let (status, error_code) = if succeeded {
        task.complete().map_err(|error| error.to_string())?;
        ("completed", None)
    } else {
        task.fail().map_err(|error| error.to_string())?;
        ("failed", Some("platform_open_failed"))
    };
    let connection = runtime
        .database
        .0
        .lock()
        .map_err(|error| error.to_string())?;
    storage::finish_execution_run(&connection, run_id, status, error_code, now_unix())
        .map_err(|error| format!("无法更新桌面任务记录：{error}"))?;
    let _ = storage::delete_expired_execution_data(&connection, now_unix());
    Ok(())
}

/// 屏幕观察的权限前置检查：陪伴模式开启 + 屏幕观察授权。
fn ensure_screen_observation_allowed(
    companion_active: bool,
    screen_observation_granted: bool,
) -> Result<(), String> {
    if !companion_active {
        return Err("陪伴模式未开启，无法观察屏幕。请先输入 /陪伴模式".to_string());
    }
    if !screen_observation_granted {
        return Err("屏幕观察授权未开启，无法观察屏幕。请在隐私设置中开启".to_string());
    }
    Ok(())
}

/// 手动查看屏幕时，若 Cyral 自己占据前台，暂时隐藏其两个窗口以取得底下的真实前台。
/// 仅由已授权的 get_screen_observation 调用；采样后立刻恢复原有可见窗口。
async fn sample_external_foreground_for_manual_request(
    app: &AppHandle,
) -> Option<crate::activity::ActivitySample> {
    let first = crate::activity::activity_sampling::sample_foreground()?;
    if !first.app_name.to_ascii_lowercase().starts_with("cyral") {
        return Some(first);
    }
    let pet = app.get_webview_window("pet");
    let composer = app.get_webview_window("composer");
    let pet_was_visible = pet
        .as_ref()
        .and_then(|window| window.is_visible().ok())
        .unwrap_or(false);
    let composer_was_visible = composer
        .as_ref()
        .and_then(|window| window.is_visible().ok())
        .unwrap_or(false);
    if let Some(window) = composer.as_ref() {
        let _ = window.hide();
    }
    if let Some(window) = pet.as_ref() {
        let _ = window.hide();
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let external = crate::activity::activity_sampling::sample_foreground().filter(|sample| {
        !sample.locked && !sample.app_name.to_ascii_lowercase().starts_with("cyral")
    });
    if pet_was_visible {
        if let Some(window) = pet.as_ref() {
            let _ = window.show();
        }
    }
    if composer_was_visible {
        if let Some(window) = composer.as_ref() {
            let _ = window.show();
            let _ = window.set_focus();
        }
    }
    external
}
async fn execute_agent_tool(
    call: &AgentToolCall,
    runtime: &ToolRuntime<'_>,
) -> Result<serde_json::Value, String> {
    match call.name.as_str() {
        "get_current_datetime" => {
            let now = chrono::Local::now();
            Ok(serde_json::json!({
                "date":now.format("%Y-%m-%d").to_string(),
                "time":now.format("%H:%M:%S").to_string(),
                "weekday":now.weekday().to_string(),
                "timezone":now.offset().to_string()
            }))
        }
        "get_current_location" => {
            if !runtime.privacy.auto_location {
                return Err("地理位置尚未授权；请用户在隐私设置中开启并保存".to_string());
            }
            let location = add_reverse_geocoded_name(current_location().await?).await;
            Ok(serde_json::json!({
                "latitude":location.latitude,
                "longitude":location.longitude,
                "accuracyMeters":location.accuracy_meters,
                "city":location.city,
                "region":location.region,
                "country":location.country,
                "displayName":location_label(&location)
            }))
        }
        "get_weather" => {
            let intent = weather_intent_from_tool_arguments(&call.arguments)?;
            let data = if intent.city.is_some() {
                fetch_weather(&intent).await?
            } else {
                let location = if call.arguments["use_current_location"].as_bool() == Some(true) {
                    if !runtime.privacy.auto_location {
                        return Err("地理位置尚未授权；请用户在隐私设置中开启并保存".to_string());
                    }
                    add_reverse_geocoded_name(current_location().await?).await
                } else {
                    LocationSnapshot {
                        latitude: call.arguments["latitude"]
                            .as_f64()
                            .ok_or("get_weather 缺少纬度")?,
                        longitude: call.arguments["longitude"]
                            .as_f64()
                            .ok_or("get_weather 缺少经度")?,
                        accuracy_meters: 0.0,
                        city: None,
                        region: None,
                        country: None,
                    }
                };
                fetch_weather_at_location(&location, &intent).await?
            };
            Ok(serde_json::json!({"source":"Open-Meteo","data":data}))
        }
        "get_screen_observation" => {
            let companion_active = runtime
                .companion
                .0
                .lock()
                .map(|state| state.active)
                .unwrap_or(false);
            ensure_screen_observation_allowed(
                companion_active,
                runtime.privacy.screen_observation,
            )?;
            let sample = sample_external_foreground_for_manual_request(runtime.app)
                .await
                .ok_or("未能取得其他应用的前台界面；请先切回需要我看的界面后重试")?; // 统一观察路径：视觉模型可用时截图增强，否则/失败回退前台窗口元数据。
            let observation = crate::activity::observe_foreground(
                &sample.app_name,
                sample.title.as_deref(),
                runtime.database,
            )
            .await;
            // 观察结果回流：写入 last_observation，后续对话可基于事实作答。
            if let Ok(mut state) = runtime.companion.0.lock() {
                state.last_observation = Some(observation.clone());
            }
            crate::activity::persist_observation(runtime.app, &observation);
            Ok(serde_json::json!({
                "observed": true,
                "talkingToCyral": false,
                "app": observation.app_name,
                "fileKey": observation.file_key,
                "windowTitle": observation.window_title,
                "summary": observation.summary,
                "source": observation.source,
                "observedAt": observation.captured_at,
            }))
        }
        "prepare_todo_review" => {
            let title = call.arguments["title"]
                .as_str()
                .ok_or("prepare_todo_review 缺少 title 参数")?
                .trim()
                .to_string();
            let deadline_local = call.arguments["deadlineLocal"]
                .as_str()
                .ok_or("prepare_todo_review 缺少 deadlineLocal 参数")?
                .trim()
                .to_string();
            if title.is_empty() {
                return Err("待办事项不能为空；请先向用户确认具体事项".to_string());
            }
            let deadline = deadline_timestamp(&deadline_local)
                .ok_or("截止时间格式无效，必须是 YYYY-MM-DDTHH:mm")?;
            let now = now_unix();
            if deadline <= now {
                return Err("截止时间必须晚于当前本地时间；请向用户确认日期和时间".to_string());
            }
            let id = {
                let connection = runtime
                    .database
                    .0
                    .lock()
                    .map_err(|error| error.to_string())?;
                save_todo_draft(
                    &connection,
                    &title,
                    &deadline_local,
                    120,
                    &format!("对话创建：{title}"),
                )
                .map_err(|error| error.to_string())?
            };
            let panel = runtime
                .app
                .get_webview_window("panel")
                .ok_or("待办草稿已保存，但审核面板不可用；请从右键菜单打开待办提醒")?;
            panel.show().map_err(|error| error.to_string())?;
            panel.set_focus().map_err(|error| error.to_string())?;
            runtime
                .app
                .emit_to("panel", "todo-review-request", id)
                .map_err(|error| error.to_string())?;
            Ok(serde_json::json!({
                "draftCreated": true,
                "draftId": id,
                "title": title,
                "deadlineLocal": deadline_local,
                "reminders": ["2 hours before", "30 minutes before", "5 minutes before"],
                "reviewOpened": true
            }))
        }
        "mark_todo_completed" => {
            let title = call.arguments["title"]
                .as_str()
                .ok_or("mark_todo_completed 缺少 title 参数")?
                .trim()
                .to_string();
            if title.is_empty() {
                return Err("mark_todo_completed 的 title 不能为空".to_string());
            }
            let connection = runtime
                .database
                .0
                .lock()
                .map_err(|error| error.to_string())?;
            let todos = load_open_todos(&connection).map_err(|error| error.to_string())?;
            let candidates = find_todo_candidates(&todos, &title);
            match candidates.as_slice() {
                [] => Err(format!("没有找到标题含“{title}”的待办，请确认事项名称")),
                [one] => {
                    let completed_title = one.title.clone();
                    complete_todo(&connection, one.id).map_err(|error| error.to_string())?;
                    Ok(serde_json::json!({
                        "completed": true,
                        "id": one.id,
                        "title": completed_title,
                    }))
                }
                many => {
                    let candidates: Vec<String> =
                        many.iter().map(|todo| todo.title.clone()).collect();
                    Ok(serde_json::json!({
                        "ambiguous": true,
                        "candidates": candidates,
                        "hint": "请用户指明具体是哪一项"
                    }))
                }
            }
        }
        "get_inbox_status" => {
            serde_json::to_value(check_gmail_inbox_status(runtime.database).await?)
                .map_err(|error| error.to_string())
        }
        "get_recent_mail_senders" => {
            let messages = recent_gmail_metadata_for_today(runtime.database).await?;
            Ok(serde_json::json!({
                "count": messages.len(),
                "messages": messages.into_iter().map(|message| serde_json::json!({
                    "sender": message.sender,
                    "subject": message.subject,
                })).collect::<Vec<_>>(),
                "messageContentRead": false,
                "maxResults": 5
            }))
        }
        "get_latest_email_status" => {
            let review = runtime
                .email_tasks
                .0
                .lock()
                .map_err(|error| error.to_string())?
                .latest()?;
            Ok(serde_json::json!({
                "reviewId": review.id,
                "accountAddress": review.account_label,
                "recipients": review.recipients,
                "subject": review.subject,
                "status": review.status,
                "statusMessage": review.status_message,
                "canRetryAutomatically": false
            }))
        }
        "prepare_email_review" => {
            if runtime.execution_origin != execution::ExecutionOrigin::UserChat {
                return Err("邮件只能由当前用户对话发起".to_string());
            }
            let recipients = call.arguments["recipients"]
                .as_array()
                .ok_or("prepare_email_review 缺少 recipients 参数")?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(ToOwned::to_owned)
                        .ok_or_else(|| "recipients 必须全部是邮箱地址字符串".to_string())
                })
                .collect::<Result<Vec<_>, _>>()?;
            let subject = call.arguments["subject"]
                .as_str()
                .ok_or("prepare_email_review 缺少 subject 参数")?
                .to_string();
            let body = call.arguments["body"]
                .as_str()
                .ok_or("prepare_email_review 缺少 body 参数")?
                .to_string();
            let attachment_candidate_ids = call
                .arguments
                .get("attachmentCandidateIds")
                .and_then(|value| value.as_array())
                .map(|values| {
                    values
                        .iter()
                        .map(|value| {
                            value
                                .as_str()
                                .map(ToOwned::to_owned)
                                .ok_or_else(|| "附件候选 ID 必须是字符串".to_string())
                        })
                        .collect::<Result<Vec<_>, _>>()
                })
                .transpose()?
                .unwrap_or_default();
            let attachments = {
                let vault = runtime
                    .execution_candidates
                    .lock()
                    .map_err(|error| error.to_string())?;
                attachment_candidate_ids
                    .iter()
                    .map(|candidate_id| {
                        let registered = vault.snapshot_authorized_file(candidate_id)?;
                        let metadata = registered
                            .path
                            .metadata()
                            .map_err(|_| "无法读取附件，请重新查找".to_string())?;
                        Ok(email::EmailAttachment {
                            sha256: email::attachment_sha256(&registered.path)?,
                            path: registered.path,
                            authorized_root: registered.authorized_root,
                            label: registered.display_name,
                            size: metadata.len(),
                            modified: metadata.modified().ok(),
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()?
            };
            let account = {
                let connection = runtime
                    .database
                    .0
                    .lock()
                    .map_err(|error| error.to_string())?;
                let mut accounts = storage::load_email_accounts(&connection)
                    .map_err(|error| error.to_string())?
                    .into_iter()
                    .filter(|account| account.enabled && email_account_has_credential(account))
                    .collect::<Vec<_>>();
                accounts.sort_by_key(|account| !account.is_default);
                accounts
                    .into_iter()
                    .next()
                    .ok_or("尚未配置已连接的邮箱，请先在隐私设置的应用偏好中完成账户连接")?
            };
            let body = if account.signature.trim().is_empty() {
                body
            } else {
                format!("{}\n\n{}", body.trim_end(), account.signature.trim())
            };
            let review = runtime
                .email_tasks
                .0
                .lock()
                .map_err(|error| error.to_string())?
                .prepare(
                    email::EmailDraftInput {
                        account_id: account.id,
                        recipients,
                        subject,
                        body,
                        attachment_candidate_ids,
                    },
                    account.account_address,
                    attachments,
                )?;
            let panel = runtime
                .app
                .get_webview_window("panel")
                .ok_or("邮件草稿已准备，但审核面板不可用")?;
            panel.show().map_err(|error| error.to_string())?;
            panel.set_focus().map_err(|error| error.to_string())?;
            runtime
                .app
                .emit_to("panel", "email-review-request", &review.id)
                .map_err(|error| error.to_string())?;
            Ok(serde_json::json!({
                "draftPrepared": true,
                "reviewId": review.id,
                "reviewOpened": true,
                "sent": false,
                "instruction": "请用户在审核卡检查并亲自点击发送"
            }))
        }
        "recall_file_activity" => {
            let _authorized_roots = authorized_file_search_roots(runtime.app, runtime.privacy)?;
            let query = call.arguments.get("query").and_then(|value| value.as_str());
            let extension = call
                .arguments
                .get("extension")
                .and_then(|value| value.as_str());
            let observed_after = call
                .arguments
                .get("observedAfter")
                .map(|value| {
                    value.as_i64().ok_or(
                        "recall_file_activity 的 observedAfter 必须是 Unix 秒时间戳".to_string(),
                    )
                })
                .transpose()?;
            let observed_before = call
                .arguments
                .get("observedBefore")
                .map(|value| {
                    value.as_i64().ok_or(
                        "recall_file_activity 的 observedBefore 必须是 Unix 秒时间戳".to_string(),
                    )
                })
                .transpose()?;
            let connection = runtime
                .database
                .0
                .lock()
                .map_err(|error| error.to_string())?;
            let recalled = recall_file_activity(
                &connection,
                query,
                extension,
                observed_after,
                observed_before,
            )?;
            serde_json::to_value(recalled)
                .map_err(|error| format!("序列化文件活动回忆失败：{error}"))
        }
        "find_local_files" => {
            let query = call.arguments["query"]
                .as_str()
                .ok_or("find_local_files 缺少 query 参数")?;
            let extension = call
                .arguments
                .get("extension")
                .and_then(|value| value.as_str());
            let observed_after = call
                .arguments
                .get("observedAfter")
                .map(|value| {
                    value
                        .as_i64()
                        .ok_or("find_local_files 的 observedAfter 必须是 Unix 秒时间戳".to_string())
                })
                .transpose()?;
            let observed_before = call
                .arguments
                .get("observedBefore")
                .map(|value| {
                    value.as_i64().ok_or(
                        "find_local_files 的 observedBefore 必须是 Unix 秒时间戳".to_string(),
                    )
                })
                .transpose()?;
            let roots = authorized_file_search_roots(runtime.app, runtime.privacy)?;
            let mut result = search_local_files(&roots, query, extension)?;
            let connection = runtime
                .database
                .0
                .lock()
                .map_err(|error| error.to_string())?;
            enrich_and_rank_file_matches(
                &connection,
                &mut result.matches,
                observed_after,
                observed_before,
            )?;
            drop(connection);
            register_unique_file_open_candidate(&roots, &mut result, runtime.execution_candidates)?;
            serde_json::to_value(result).map_err(|error| format!("序列化文件搜索结果失败：{error}"))
        }
        "find_installed_apps" => {
            let query = call.arguments["query"]
                .as_str()
                .ok_or("find_installed_apps 缺少 query 参数")?;
            let (roots, matches) = windows_actions::search_installed_apps(query)?;
            let open_candidate_id = if matches.len() == 1 {
                Some(
                    runtime
                        .execution_candidates
                        .lock()
                        .map_err(|error| error.to_string())?
                        .register_installed_app(&roots, &matches[0])?,
                )
            } else {
                None
            };
            let visible_matches = matches
                .iter()
                .map(|app| serde_json::json!({"name": app.name, "source": app.source}))
                .collect::<Vec<_>>();
            Ok(serde_json::json!({
                "matches": visible_matches,
                "openCandidateId": open_candidate_id,
                "requiresUserSelection": matches.len() > 1,
            }))
        }
        "open_app_candidate" => {
            let candidate_id = call.arguments["candidateId"]
                .as_str()
                .ok_or("open_app_candidate 缺少 candidateId 参数")?;
            let label = runtime
                .execution_candidates
                .lock()
                .map_err(|error| error.to_string())?
                .app_label(candidate_id)?;
            let (mut task, run_id) = begin_audited_execution(
                runtime,
                execution::DesktopAction::OpenInstalledApp,
                "open_app",
                &label,
            )?;
            let opened = {
                let mut candidates = runtime
                    .execution_candidates
                    .lock()
                    .map_err(|error| error.to_string())?;
                windows_actions::open_installed_app(&mut candidates, candidate_id)
            };
            finish_audited_execution(runtime, run_id, &mut task, opened.is_ok())?;
            let display_name = opened?;
            Ok(serde_json::json!({"opened": true, "target": display_name}))
        }
        "open_file_candidate" => {
            let candidate_id = call.arguments["candidateId"]
                .as_str()
                .ok_or("open_file_candidate 缺少 candidateId 参数")?;
            let label = runtime
                .execution_candidates
                .lock()
                .map_err(|error| error.to_string())?
                .file_label(candidate_id)?;
            let (mut task, run_id) = begin_audited_execution(
                runtime,
                execution::DesktopAction::OpenAuthorizedFile,
                "open_file",
                &label,
            )?;
            let opened = {
                let mut candidates = runtime
                    .execution_candidates
                    .lock()
                    .map_err(|error| error.to_string())?;
                windows_actions::open_authorized_file(&mut candidates, candidate_id)
            };
            finish_audited_execution(runtime, run_id, &mut task, opened.is_ok())?;
            let display_name = opened?;
            Ok(serde_json::json!({"opened": true, "target": display_name}))
        }
        "read_dropped_text_file" => {
            let candidate_id = call.arguments["candidateId"]
                .as_str()
                .ok_or("read_dropped_text_file 缺少 candidateId 参数")?;
            if !candidate_id.starts_with("drop-") {
                return Err("只能读取用户在当前对话中拖入的文件".to_string());
            }
            let file = runtime
                .execution_candidates
                .lock()
                .map_err(|error| error.to_string())?
                .snapshot_authorized_file(candidate_id)?;
            if !text_file_is_readable(&file.path) {
                return Err("该文件类型不允许读取内容，只能作为邮件附件".to_string());
            }
            let metadata =
                fs::metadata(&file.path).map_err(|_| "拖入文件已失效，请重新拖入".to_string())?;
            if metadata.len() > MAX_TEXT_FILE_BYTES {
                return Err("文本文件超过 1 MiB，不能读取内容".to_string());
            }
            let content = String::from_utf8(
                fs::read(&file.path).map_err(|_| "无法读取拖入的文本文件".to_string())?,
            )
            .map_err(|_| "文件不是有效 UTF-8 文本，不能读取内容".to_string())?;
            Ok(serde_json::json!({
                "name": file.display_name,
                "content": content,
                "sizeBytes": metadata.len()
            }))
        }
        "set_movement_allowed" => {
            let enabled = call.arguments["enabled"]
                .as_bool()
                .ok_or("set_movement_allowed 缺少 enabled 参数")?;
            let mut motion = runtime
                .pet_motion
                .0
                .lock()
                .map_err(|error| error.to_string())?;
            apply_movement_permission(&mut motion, enabled);
            Ok(serde_json::json!({ "movement_allowed": enabled }))
        }
        _ => Err(format!("未知工具：{}", call.name)),
    }
}

/// 把一轮工具结果写回循环上下文：原生路径回填 assistant 调用并按协议附加结果，
/// JSON 规划回退路径保持 system 文本结果消息；两条路径载荷一致。
fn push_tool_round(
    context: &mut Vec<AgentContextItem>,
    calls: Vec<AgentToolCall>,
    mode: ToolCallMode,
    results: Vec<serde_json::Value>,
) {
    if mode == ToolCallMode::Native {
        context.push(AgentContextItem::NativeCalls(calls.clone()));
        for (call, result) in calls.into_iter().zip(results) {
            let call_id = call.id.clone();
            context.push(AgentContextItem::NativeResult {
                call_id,
                output: tool_result_json(&call, result).to_string(),
            });
        }
    } else {
        for (call, result) in calls.into_iter().zip(results) {
            context.push(AgentContextItem::Plain(tool_result_message(&call, result)));
        }
    }
}

async fn run_agent_loop(
    provider: &ModelProviderSettings,
    api_key: &str,
    messages: &[Message],
    runtime: &ToolRuntime<'_>,
) -> Result<String, String> {
    let mut context: Vec<AgentContextItem> = messages
        .iter()
        .cloned()
        .map(AgentContextItem::Plain)
        .collect();
    let require_file_search = file_recall_requires_tool(messages);
    let mut file_search_attempted = false;
    for _ in 0..4 {
        let (decision, mode) = request_agent_decision(provider, api_key, &context).await?;
        match decision {
            AgentDecision::Final(_) if require_file_search && !file_search_attempted => {
                context.push(AgentContextItem::Plain(Message {
                    role: "system".to_string(),
                    content: "程序拒绝了未执行搜索的答复：已达到两轮澄清上限。现在必须调用 find_local_files；不得继续提问。".to_string(),
                    created_at: 0,
                    source: "runtime".to_string(),
                }));
            }
            AgentDecision::Final(answer) => return Ok(answer),
            AgentDecision::ToolCalls(calls) if calls.is_empty() => {
                return Err("模型返回了空工具调用".to_string());
            }
            AgentDecision::ToolCalls(calls) => {
                if calls.iter().any(|call| call.name == "find_local_files") {
                    file_search_attempted = true;
                }
                let mut results = Vec::with_capacity(calls.len());
                for call in &calls {
                    let result = match execute_agent_tool(call, runtime).await {
                        Ok(data) => serde_json::json!({"ok": true, "data": data}),
                        Err(error) => serde_json::json!({"ok": false, "error": error}),
                    };
                    results.push(result);
                }
                push_tool_round(&mut context, calls, mode, results);
            }
        }
    }
    Err("工具调用超过 4 轮，已停止以避免循环".to_string())
}

#[allow(dead_code)]
async fn send_model_request(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    body: &serde_json::Value,
) -> Result<(reqwest::StatusCode, String), String> {
    let response = apply_model_auth(client.post(url), api_key, url)
        .json(body)
        .send()
        .await
        .map_err(|error| format_error_chain("无法连接模型供应商", &error))?;
    let status = response.status();
    let response_text = response
        .text()
        .await
        .map_err(|error| format!("无法读取模型响应：{error}"))?;
    if status.is_success() {
        record_usage_from_response(body, url, &response_text);
    }
    Ok((status, response_text))
}

fn format_error_chain(prefix: &str, error: &dyn std::error::Error) -> String {
    let mut details = vec![error.to_string()];
    let mut source = error.source();
    while let Some(cause) = source {
        let detail = cause.to_string();
        if !details.contains(&detail) {
            details.push(detail);
        }
        source = cause.source();
    }
    format!("{prefix}：{}", details.join(" → "))
}

fn save_provider_settings(
    connection: &Connection,
    settings: &ModelProviderSettings,
) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO model_provider
            (id, provider_name, base_url, protocol, model, monthly_budget_usd)
         VALUES (1, ?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(id) DO UPDATE SET
            provider_name = excluded.provider_name,
            base_url = excluded.base_url,
            protocol = excluded.protocol,
            model = excluded.model,
            monthly_budget_usd = excluded.monthly_budget_usd",
        params![
            settings.provider_name,
            settings.base_url,
            settings.protocol,
            settings.model,
            settings.monthly_budget_usd
        ],
    )?;
    Ok(())
}

fn load_provider_settings(connection: &Connection) -> rusqlite::Result<ModelProviderSettings> {
    connection.query_row(
        "SELECT provider_name, base_url, protocol, model, monthly_budget_usd
         FROM model_provider WHERE id = 1",
        [],
        |row| {
            Ok(ModelProviderSettings {
                provider_name: row.get(0)?,
                base_url: row.get(1)?,
                protocol: row.get(2)?,
                model: row.get(3)?,
                monthly_budget_usd: row.get(4)?,
            })
        },
    )
}

fn save_message(connection: &Connection, role: &str, content: &str) -> rusqlite::Result<()> {
    save_message_with_source(connection, role, content, "chat")
}

pub(crate) fn save_message_with_source(
    connection: &Connection,
    role: &str,
    content: &str,
    source: &str,
) -> rusqlite::Result<()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    connection.execute(
        "INSERT INTO messages (role, content, created_at, source) VALUES (?1, ?2, ?3, ?4)",
        params![role, content, now, source],
    )?;
    Ok(())
}

fn load_recent_messages(connection: &Connection, limit: usize) -> rusqlite::Result<Vec<Message>> {
    let mut statement = connection.prepare(
        "SELECT role, content, created_at, source FROM messages ORDER BY id DESC LIMIT ?1",
    )?;
    let messages = statement
        .query_map(params![limit as i64], |row| {
            Ok(Message {
                role: row.get(0)?,
                content: row.get(1)?,
                created_at: row.get(2)?,
                source: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(messages.into_iter().rev().collect())
}

fn model_conversation_context(messages: Vec<Message>) -> Vec<Message> {
    messages
        .into_iter()
        .filter(|message| !message.content.chars().any(char::is_control))
        .collect()
}

fn should_extract_memories(user_turn_count: i64) -> bool {
    user_turn_count > 0 && user_turn_count % 5 == 0
}

fn save_memory_atom(
    connection: &Connection,
    kind: &str,
    content: &str,
    source_excerpt: &str,
    confidence: f64,
) -> rusqlite::Result<()> {
    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    connection.execute(
        "INSERT OR IGNORE INTO memory_atoms
            (kind, content, source_excerpt, confidence, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            kind,
            content.trim(),
            source_excerpt.trim(),
            confidence.clamp(0.0, 1.0),
            created_at
        ],
    )?;
    Ok(())
}

fn load_memory_atoms(connection: &Connection) -> rusqlite::Result<Vec<MemoryAtom>> {
    let mut statement = connection.prepare(
        "SELECT id, kind, content, source_excerpt, confidence, created_at
         FROM memory_atoms ORDER BY id DESC LIMIT 200",
    )?;
    let memories = statement
        .query_map([], |row| {
            Ok(MemoryAtom {
                id: row.get(0)?,
                kind: row.get(1)?,
                content: row.get(2)?,
                source_excerpt: row.get(3)?,
                confidence: row.get(4)?,
                created_at: row.get(5)?,
            })
        })?
        .collect();
    memories
}

fn update_memory_atom(connection: &Connection, id: i64, content: &str) -> rusqlite::Result<()> {
    connection.execute(
        "UPDATE memory_atoms SET content = ?1, confidence = 1.0 WHERE id = ?2",
        params![content.trim(), id],
    )?;
    Ok(())
}

fn delete_memory_atom(connection: &Connection, id: i64) -> rusqlite::Result<()> {
    connection.execute("DELETE FROM memory_atoms WHERE id = ?1", params![id])?;
    Ok(())
}

fn parse_memory_candidates(raw: &str) -> Result<Vec<MemoryCandidate>, String> {
    let trimmed = raw.trim();
    let json = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed)
        .strip_suffix("```")
        .unwrap_or(trimmed)
        .trim();
    let extraction: MemoryExtraction =
        serde_json::from_str(json).map_err(|error| format!("无法解析记忆抽取结果：{error}"))?;
    Ok(extraction
        .memories
        .into_iter()
        .filter(|memory| {
            !memory.content.trim().is_empty() && !memory.source_excerpt.trim().is_empty()
        })
        .collect())
}

fn memory_context(memories: &[MemoryAtom]) -> Option<Message> {
    if memories.is_empty() {
        return None;
    }
    let facts = memories
        .iter()
        .take(30)
        .map(|memory| format!("- {}", memory.content))
        .collect::<Vec<_>>()
        .join("\n");
    Some(Message {
        role: "system".to_string(),
        content: format!(
            "以下是用户可修改的本地长期记忆。仅在相关时自然使用，不要逐条复述：\n{facts}"
        ),
        created_at: 0,
        source: "chat".to_string(),
    })
}

/// 构建运行时状态上下文：陪伴模式、屏幕观察/地理位置授权、最近观察。
/// 供模型在"能否看到屏幕"等提问中基于事实回应，而非猜测。
fn build_runtime_context(
    companion: &CompanionSession,
    privacy: &PrivacyConfig,
    movement_allowed: bool,
) -> Option<Message> {
    let state = companion.0.lock().ok()?;
    let mut parts = vec![
        if state.active {
            "陪伴模式：开启".to_string()
        } else {
            "陪伴模式：未开启".to_string()
        },
        if movement_allowed {
            "移动许可：允许在桌面自动移动".to_string()
        } else {
            "移动许可：原地待机（用户要求不打扰，桌宠不自动移动）".to_string()
        },
        if privacy.screen_observation {
            "屏幕观察授权：已开启".to_string()
        } else {
            "屏幕观察授权：未开启".to_string()
        },
        if privacy.auto_location {
            "地理位置授权：已开启".to_string()
        } else {
            "地理位置授权：未开启".to_string()
        },
    ];
    if state.user_talking_to_cyral {
        parts.push("用户当前正在与 Cyral 对话（前台是 Cyral 自己）".to_string());
    }
    let observation_text = match &state.last_observation {
        Some(observation) => {
            let mut detail = format!("应用：{}", observation.app_name);
            if let Some(file_key) = &observation.file_key {
                detail.push_str(&format!("，文件：{file_key}"));
            }
            if let Some(window_title) = &observation.window_title {
                detail.push_str(&format!("，窗口标题：{window_title}"));
            }
            detail.push_str(&format!("，场景摘要：{}", observation.summary));
            format!("最近观察：{detail}")
        }
        None => "最近观察：暂无".to_string(),
    };
    parts.push(observation_text);
    Some(Message {
        role: "system".to_string(),
        content: format!(
            "【运行时状态】{}。以上是当前真实的运行状态。关于屏幕的提问只能基于“陪伴模式”和“最近观察”如实作答：\
             有观察就转述其中的事实（如“我最近观察到你正在用 XXX”）；没有观察且陪伴模式已开启时回答“我还没有观察到屏幕内容”；\
             没有观察且陪伴模式未开启时回答“陪伴模式未开启，我看不到你的屏幕”。\
             不得声称能实时看到屏幕，也不得编造未观察到的内容。\
             最近观察来自前台窗口（应用与窗口标题）信息，只能说明用户在用什么软件或文件，不代表能看到屏幕上的内容。\
             当用户当前正在与 Cyral 对话时，最近观察保留的是用户此前使用的非 Cyral 应用，回答时可以说“你刚才在用的 XXX”并说明现在在和我对话。\
             本条【运行时状态】始终反映当前真实状态，若与对话历史中此前的说法不一致，以本条为准，不要复述或采信历史中的旧断言。\
             “最近观察：暂无”只表示尚未抓到屏幕内容，不代表观察未开启；陪伴模式已开启时，不要说“请开启观察”。
             当陪伴模式已开启时，客户端会自行定期观察并主动搭话；不得声称没有主动发起对话的能力，也不得声称只能在用户叫到时回应。",
            parts.join("；"),
        ),
        created_at: 0,
                source: "chat".to_string(),
    })
}

/// 模式切换的 UI 同步：刷新面板模式卡片。模型的"切换感知"由每轮对话注入的
/// 运行时状态（build_runtime_context）承担，不单独向模型注入 system 消息。
fn notify_mode_change(app: &AppHandle, event: &str) -> Result<(), String> {
    let _ = app.emit_to("panel", "mode-changed", ());
    let _ = event;
    Ok(())
}

/// 隐私授权变更的 UI 提示：气泡告知用户。模型感知同样由每轮运行时状态承担。
fn notify_privacy_change(app: &AppHandle, scope: &str, enabled: bool) -> Result<(), String> {
    ensure_pet_visible(app);
    let text = if enabled {
        format!("你已授权{scope}，之后可以读取相关数据。")
    } else {
        format!("你已关闭{scope}授权，之后不再读取相关数据。")
    };
    let _ = app.emit_to("pet", "assistant-message", &text);
    Ok(())
}

/// 关闭屏幕观察授权时，若陪伴模式正在运行，必须立即停止采集（"未授权不得截图"）。
fn companion_must_stop_on_privacy_revoke(before: &PrivacyConfig, after: &PrivacyConfig) -> bool {
    before.screen_observation && !after.screen_observation
}

#[cfg(test)]
fn runtime_environment_context<Tz: chrono::TimeZone>(
    now: chrono::DateTime<Tz>,
    weather: Option<&str>,
) -> Message
where
    Tz::Offset: std::fmt::Display,
{
    let weekday = match now.weekday() {
        chrono::Weekday::Mon => "星期一",
        chrono::Weekday::Tue => "星期二",
        chrono::Weekday::Wed => "星期三",
        chrono::Weekday::Thu => "星期四",
        chrono::Weekday::Fri => "星期五",
        chrono::Weekday::Sat => "星期六",
        chrono::Weekday::Sun => "星期日",
    };
    let weather = weather.unwrap_or("未配置城市");
    Message {
        role: "system".to_string(),
        content: format!(
            "当前运行环境（直接作为事实使用，不要声称看不到时间）：{}年{}月{}日 {weekday} {}；天气：{weather}。",
            now.year(),
            now.month(),
            now.day(),
            now.format("%H:%M")
        ),
        created_at: 0,
                source: "chat".to_string(),
    }
}

#[cfg(test)]
fn local_time_answer<Tz: chrono::TimeZone>(
    message: &str,
    now: chrono::DateTime<Tz>,
) -> Option<String>
where
    Tz::Offset: std::fmt::Display,
{
    let asks_time = ["几点", "时间", "日期", "几号", "星期几", "周几"]
        .iter()
        .any(|keyword| message.contains(keyword));
    if !asks_time {
        return None;
    }
    let weekday = match now.weekday() {
        chrono::Weekday::Mon => "星期一",
        chrono::Weekday::Tue => "星期二",
        chrono::Weekday::Wed => "星期三",
        chrono::Weekday::Thu => "星期四",
        chrono::Weekday::Fri => "星期五",
        chrono::Weekday::Sat => "星期六",
        chrono::Weekday::Sun => "星期日",
    };
    Some(format!(
        "现在是 {}年{}月{}日 {weekday} {}。",
        now.year(),
        now.month(),
        now.day(),
        now.format("%H:%M")
    ))
}

#[cfg(test)]
fn parse_chinese_digit(value: char) -> Option<u8> {
    match value {
        '一' => Some(1),
        '二' | '两' => Some(2),
        '三' => Some(3),
        '四' => Some(4),
        '五' => Some(5),
        '六' => Some(6),
        '七' => Some(7),
        '八' => Some(8),
        '九' => Some(9),
        _ => value
            .to_digit(10)
            .and_then(|digit| u8::try_from(digit).ok()),
    }
}

#[cfg(test)]
fn parse_month_day(message: &str) -> Option<(u32, u32, String)> {
    let month_marker = message.find('月')?;
    let day_tail = &message[month_marker + '月'.len_utf8()..];
    let day_marker = day_tail.find('日').or_else(|| day_tail.find('号'))?;
    let month_start = message[..month_marker]
        .char_indices()
        .rev()
        .take_while(|(_, character)| character.is_ascii_digit())
        .last()
        .map(|(index, _)| index)?;
    let month = message[month_start..month_marker].parse::<u32>().ok()?;
    let day = day_tail[..day_marker].parse::<u32>().ok()?;
    let _ = chrono::NaiveDate::from_ymd_opt(2024, month, day)?;
    let token_end = month_marker + '月'.len_utf8() + day_marker + '日'.len_utf8();
    Some((month, day, message[month_start..token_end].to_string()))
}

#[cfg(test)]
fn parse_weather_intent(message: &str) -> Option<WeatherIntent> {
    if [
        "CPU",
        "GPU",
        "处理器",
        "显卡",
        "空调",
        "水温",
        "杯水",
        "体温",
        "烤箱",
    ]
    .iter()
    .any(|keyword| message.contains(keyword))
    {
        return None;
    }
    let is_weather = [
        "天气",
        "气温",
        "温度",
        "多少度",
        "下雨",
        "降雨",
        "有雨",
        "下雪",
        "冷不冷",
        "热不热",
        "冷吗",
        "热吗",
    ]
    .iter()
    .any(|keyword| message.contains(keyword));
    if !is_weather {
        return None;
    }

    let metric = if ["下雨", "降雨", "有雨", "雨吗"]
        .iter()
        .any(|keyword| message.contains(keyword))
    {
        WeatherMetric::Precipitation
    } else if ["气温", "温度", "多少度", "冷不冷", "热不热", "冷吗", "热吗"]
        .iter()
        .any(|keyword| message.contains(keyword))
    {
        WeatherMetric::Temperature
    } else {
        WeatherMetric::Summary
    };

    let month_day = parse_month_day(message);
    let time = if let Some((month, day, _)) = month_day.as_ref() {
        WeatherTime::MonthDay {
            month: *month,
            day: *day,
        }
    } else if message.contains("后天") {
        WeatherTime::DayOffset(2)
    } else if message.contains("明天") {
        WeatherTime::DayOffset(1)
    } else if let Some(index) = message.find("未来") {
        let count = message[index + "未来".len()..]
            .chars()
            .next()
            .and_then(parse_chinese_digit)
            .unwrap_or(3)
            .clamp(1, 7);
        WeatherTime::NextDays(count)
    } else {
        WeatherTime::Current
    };

    let city = {
        let mut candidate = message.to_string();
        for phrase in [
            "测试：",
            "测试:",
            "帮我查一下",
            "帮我看看",
            "请问",
            "查一下",
            "查询",
            "看一下",
            "看看",
            "帮我",
            "请",
            "我所在的城市",
            "当前位置",
            "我这里",
            "我这儿",
            "我这边",
            "这里",
            "这儿",
            "附近",
            "外面",
            "当地",
            "本地",
            "当前",
            "现在",
            "今天",
            "今日",
            "明天",
            "后天",
            "未来",
            "气温",
            "温度",
            "天气",
            "多少度",
            "是多少",
            "多少",
            "会不会",
            "会下雨吗",
            "下雨吗",
            "下雨",
            "降雨",
            "有雨",
            "怎么样",
            "如何",
            "冷不冷",
            "热不热",
            "冷吗",
            "热吗",
            "的",
            "吗",
            "呢",
            "？",
            "?",
            "。",
            "，",
            ",",
            " ",
        ] {
            candidate = candidate.replace(phrase, "");
        }
        candidate = candidate.replace("几天", "");
        for count in [
            '一', '二', '两', '三', '四', '五', '六', '七', '1', '2', '3', '4', '5', '6', '7',
        ] {
            candidate = candidate.replace(&format!("{count}天"), "");
        }
        if let Some((_, _, token)) = month_day {
            candidate = candidate.replace(&token, "");
        }
        let candidate = candidate.trim();
        let candidate = candidate
            .strip_suffix("地区")
            .or_else(|| candidate.strip_suffix('地'))
            .unwrap_or(candidate)
            .trim();
        (!candidate.is_empty() && candidate.chars().count() <= 20).then(|| candidate.to_string())
    };

    Some(WeatherIntent { city, time, metric })
}

#[cfg(test)]
fn weather_city_from_message(message: &str) -> Option<String> {
    parse_weather_intent(message).and_then(|intent| intent.city)
}

fn weather_intent_from_tool_arguments(
    arguments: &serde_json::Value,
) -> Result<WeatherIntent, String> {
    let city = arguments["city"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let has_coordinates =
        arguments["latitude"].as_f64().is_some() && arguments["longitude"].as_f64().is_some();
    let uses_current_location = arguments["use_current_location"].as_bool() == Some(true);
    if city.is_none() && !has_coordinates && !uses_current_location {
        return Err("get_weather 需要城市，或同时提供纬度和经度".to_string());
    }
    let days = arguments["days"].as_u64().unwrap_or(1);
    if !(1..=7).contains(&days) {
        return Err("天气查询天数必须在 1–7 天之间".to_string());
    }
    let metric = match arguments["metric"].as_str().unwrap_or("summary") {
        "summary" => WeatherMetric::Summary,
        "temperature" => WeatherMetric::Temperature,
        "precipitation" => WeatherMetric::Precipitation,
        _ => return Err("不支持该天气指标".to_string()),
    };
    let time = if days > 1 {
        WeatherTime::NextDays(days as u8)
    } else {
        match arguments["date"].as_str().unwrap_or("current") {
            "current" | "now" => WeatherTime::Current,
            "today" => WeatherTime::DayOffset(0),
            "tomorrow" => WeatherTime::DayOffset(1),
            "day_after_tomorrow" => WeatherTime::DayOffset(2),
            raw => {
                let target = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d").map_err(|_| {
                    "天气日期必须是 current、today、tomorrow 或 YYYY-MM-DD".to_string()
                })?;
                let offset = (target - chrono::Local::now().date_naive()).num_days();
                if !(0..=15).contains(&offset) {
                    return Err("目标日期超出未来 16 天预报范围".to_string());
                }
                WeatherTime::DayOffset(offset as u8)
            }
        }
    };
    Ok(WeatherIntent { city, time, metric })
}

fn weather_description(code: i64) -> &'static str {
    match code {
        0 => "晴",
        1 | 2 => "晴间多云",
        3 => "多云",
        45 | 48 => "有雾",
        51..=57 => "毛毛雨",
        61..=67 | 80..=82 => "有雨",
        71..=77 | 85 | 86 => "有雪",
        95..=99 => "雷暴",
        _ => "天气状况未知",
    }
}

async fn fetch_weather(intent: &WeatherIntent) -> Result<String, String> {
    let city = intent.city.as_deref().ok_or("天气请求缺少城市")?;
    let client = shared_http_client(8);
    let location: serde_json::Value = client
        .get("https://geocoding-api.open-meteo.com/v1/search")
        .query(&[
            ("name", city),
            ("count", "1"),
            ("language", "zh"),
            ("format", "json"),
        ])
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .json()
        .await
        .map_err(|error| error.to_string())?;
    let first = location["results"]
        .as_array()
        .and_then(|items| items.first())
        .ok_or_else(|| "找不到该城市".to_string())?;
    let latitude = first["latitude"]
        .as_f64()
        .ok_or_else(|| "城市缺少纬度".to_string())?;
    let longitude = first["longitude"]
        .as_f64()
        .ok_or_else(|| "城市缺少经度".to_string())?;
    let resolved_name = first["name"].as_str().unwrap_or(city);
    fetch_weather_for_coordinates(&client, resolved_name, latitude, longitude, intent).await
}

#[cfg(test)]
fn weather_date_for_month_day(month: u32, day: u32) -> Result<chrono::NaiveDate, String> {
    let today = chrono::Local::now().date_naive();
    let this_year = chrono::NaiveDate::from_ymd_opt(today.year(), month, day)
        .ok_or_else(|| "日期无效".to_string())?;
    if this_year >= today {
        Ok(this_year)
    } else {
        chrono::NaiveDate::from_ymd_opt(today.year() + 1, month, day)
            .ok_or_else(|| "日期无效".to_string())
    }
}

fn daily_weather_line(
    daily: &serde_json::Value,
    index: usize,
    label: &str,
    metric: WeatherMetric,
) -> Result<String, String> {
    let code = daily["weather_code"]
        .as_array()
        .and_then(|values| values.get(index))
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(-1);
    let maximum = daily["temperature_2m_max"]
        .as_array()
        .and_then(|values| values.get(index))
        .and_then(serde_json::Value::as_f64)
        .ok_or("预报缺少最高温度")?;
    let minimum = daily["temperature_2m_min"]
        .as_array()
        .and_then(|values| values.get(index))
        .and_then(serde_json::Value::as_f64)
        .ok_or("预报缺少最低温度")?;
    let precipitation = daily["precipitation_probability_max"]
        .as_array()
        .and_then(|values| values.get(index))
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0);
    Ok(match metric {
        WeatherMetric::Summary => format!(
            "{label}：{}，{minimum:.0}–{maximum:.0}°C，降雨概率 {precipitation:.0}%",
            weather_description(code)
        ),
        WeatherMetric::Temperature => {
            format!("{label}：最低 {minimum:.0}°C，最高 {maximum:.0}°C")
        }
        WeatherMetric::Precipitation => format!(
            "{label}：{}，降雨概率 {precipitation:.0}%",
            weather_description(code)
        ),
    })
}

async fn fetch_weather_for_coordinates(
    client: &reqwest::Client,
    place_label: &str,
    latitude: f64,
    longitude: f64,
    intent: &WeatherIntent,
) -> Result<String, String> {
    let weather: serde_json::Value = client
        .get("https://api.open-meteo.com/v1/forecast")
        .query(&[
            ("latitude", latitude.to_string()),
            ("longitude", longitude.to_string()),
            (
                "current",
                "temperature_2m,apparent_temperature,weather_code".to_string(),
            ),
            (
                "daily",
                "weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max"
                    .to_string(),
            ),
            ("forecast_days", "16".to_string()),
            ("timezone", "auto".to_string()),
        ])
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .json()
        .await
        .map_err(|error| error.to_string())?;
    match intent.time {
        WeatherTime::Current => {
            let current = &weather["current"];
            let temperature = current["temperature_2m"]
                .as_f64()
                .ok_or_else(|| "天气缺少温度".to_string())?;
            let apparent = current["apparent_temperature"]
                .as_f64()
                .ok_or_else(|| "天气缺少体感温度".to_string())?;
            let code = current["weather_code"].as_i64().unwrap_or(-1);
            Ok(match intent.metric {
                WeatherMetric::Temperature => {
                    format!("{place_label}当前 {temperature:.0}°C，体感 {apparent:.0}°C")
                }
                WeatherMetric::Precipitation => format!(
                    "{place_label}当前{}；实时接口不提供分钟级降雨概率。",
                    weather_description(code)
                ),
                WeatherMetric::Summary => format!(
                    "{place_label}当前{}，{temperature:.0}°C，体感 {apparent:.0}°C",
                    weather_description(code)
                ),
            })
        }
        WeatherTime::DayOffset(offset) => {
            let day = chrono::Local::now().date_naive() + chrono::Duration::days(i64::from(offset));
            let date = day.format("%Y-%m-%d").to_string();
            let index = weather["daily"]["time"]
                .as_array()
                .and_then(|values| {
                    values
                        .iter()
                        .position(|value| value.as_str() == Some(&date))
                })
                .ok_or("天气服务没有返回目标日期的预报")?;
            let relative = match offset {
                0 => "今天".to_string(),
                1 => "明天".to_string(),
                2 => "后天".to_string(),
                _ => day.format("%m月%d日").to_string(),
            };
            daily_weather_line(
                &weather["daily"],
                index,
                &format!("{place_label}{relative}"),
                intent.metric,
            )
        }
        WeatherTime::NextDays(count) => {
            let lines = (0..usize::from(count))
                .map(|index| {
                    let date = weather["daily"]["time"]
                        .as_array()
                        .and_then(|values| values.get(index))
                        .and_then(serde_json::Value::as_str)
                        .ok_or("天气服务没有返回足够的未来预报")?;
                    daily_weather_line(
                        &weather["daily"],
                        index,
                        &format!("{place_label} {date}"),
                        intent.metric,
                    )
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(lines.join("\n"))
        }
        #[cfg(test)]
        WeatherTime::MonthDay { month, day } => {
            let target = weather_date_for_month_day(month, day)?;
            let days_away = (target - chrono::Local::now().date_naive()).num_days();
            if !(0..=15).contains(&days_away) {
                return Err("该日期超出天气服务未来 16 天的预报范围".to_string());
            }
            let date = target.format("%Y-%m-%d").to_string();
            let index = weather["daily"]["time"]
                .as_array()
                .and_then(|values| {
                    values
                        .iter()
                        .position(|value| value.as_str() == Some(&date))
                })
                .ok_or("天气服务没有返回目标日期的预报")?;
            daily_weather_line(
                &weather["daily"],
                index,
                &format!("{place_label}{}", target.format("%m月%d日")),
                intent.metric,
            )
        }
    }
}

#[cfg(test)]
fn asks_for_location(message: &str) -> bool {
    if ["我的位置", "当前位置", "定位", "所在城市", "哪个城市"]
        .iter()
        .any(|keyword| message.contains(keyword))
    {
        return true;
    }
    message.contains('我')
        && ["在哪", "哪里", "哪儿"]
            .iter()
            .any(|keyword| message.contains(keyword))
}

#[cfg(test)]
fn asks_for_weather(message: &str) -> bool {
    parse_weather_intent(message).is_some()
}

#[cfg(test)]
fn needs_automatic_location(message: &str) -> bool {
    asks_for_location(message)
        || parse_weather_intent(message).is_some_and(|intent| intent.city.is_none())
}

fn location_label(location: &LocationSnapshot) -> String {
    let named = [
        location.country.as_deref(),
        location.region.as_deref(),
        location.city.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter(|part| is_human_location_name(part))
    .fold(Vec::new(), |mut parts, part| {
        if !parts.contains(&part) {
            parts.push(part);
        }
        parts
    })
    .join(" ");
    if named.is_empty() {
        format!(
            "纬度 {:.4}、经度 {:.4}",
            location.latitude, location.longitude
        )
    } else {
        named
    }
}

fn is_human_location_name(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && !trimmed.chars().any(char::is_control)
        && !(trimmed.chars().count() <= 5
            && trimmed
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '-'))
}

#[cfg(windows)]
async fn current_location() -> Result<LocationSnapshot, String> {
    use windows::Devices::Geolocation::{Geolocator, PositionAccuracy};

    let locator = Geolocator::new().map_err(|error| format!("无法启动 Windows 定位：{error}"))?;
    locator
        .SetDesiredAccuracy(PositionAccuracy::Default)
        .map_err(|error| format!("无法设置定位精度：{error}"))?;
    let position = locator
        .GetGeopositionAsync()
        .map_err(|error| format!("无法请求 Windows 定位：{error}"))?
        .await
        .map_err(|error| {
            format!("Windows 没有返回位置，请检查系统定位服务和 Cyral 权限：{error}")
        })?;
    let coordinate = position.Coordinate().map_err(|error| error.to_string())?;
    let point = coordinate.Point().map_err(|error| error.to_string())?;
    let basic = point.Position().map_err(|error| error.to_string())?;
    Ok(LocationSnapshot {
        latitude: basic.Latitude,
        longitude: basic.Longitude,
        accuracy_meters: coordinate.Accuracy().unwrap_or(0.0),
        city: None,
        region: None,
        country: None,
    })
}

#[cfg(not(windows))]
async fn current_location() -> Result<LocationSnapshot, String> {
    Err("当前版本只在 Windows 上支持自动定位".to_string())
}

async fn fetch_weather_at_location(
    location: &LocationSnapshot,
    intent: &WeatherIntent,
) -> Result<String, String> {
    let client = shared_http_client(8);
    let place_label = if location.city.as_deref().is_some_and(is_human_location_name) {
        location_label(location)
    } else {
        "你当前位置".to_string()
    };
    fetch_weather_for_coordinates(
        &client,
        &place_label,
        location.latitude,
        location.longitude,
        intent,
    )
    .await
}

async fn add_reverse_geocoded_name(mut location: LocationSnapshot) -> LocationSnapshot {
    if location.city.as_deref().is_some_and(is_human_location_name) {
        return location;
    }
    let client = shared_http_client(8);
    // 两个反地理编码源并行请求，先返回成功结果的胜出，避免串行 8s+8s 的最坏等待。
    let nominatim_request = client
        .get("https://nominatim.openstreetmap.org/reverse")
        .query(&[
            ("lat", location.latitude.to_string()),
            ("lon", location.longitude.to_string()),
            ("format", "jsonv2".to_string()),
            ("accept-language", "zh-CN".to_string()),
            ("zoom", "10".to_string()),
        ]);
    let bigdatacloud_request = client
        .get("https://api.bigdatacloud.net/data/reverse-geocode-client")
        .query(&[
            ("latitude", location.latitude.to_string()),
            ("longitude", location.longitude.to_string()),
            ("localityLanguage", "zh".to_string()),
        ]);
    let parse_nominatim = async {
        match nominatim_request.send().await {
            Ok(response) => match response.json::<serde_json::Value>().await {
                Ok(value) => parse_reverse_geocode(&value).filter(|resolved| {
                    resolved.city.as_deref().is_some_and(is_human_location_name)
                }),
                Err(_) => None,
            },
            Err(_) => None,
        }
    };
    let parse_bigdatacloud = async {
        match bigdatacloud_request.send().await {
            Ok(response) => match response.json::<serde_json::Value>().await {
                Ok(value) => parse_bigdatacloud_reverse_geocode(&value).filter(|resolved| {
                    resolved.city.as_deref().is_some_and(is_human_location_name)
                }),
                Err(_) => None,
            },
            Err(_) => None,
        }
    };
    let (nominatim, bigdatacloud) = futures_util::join!(parse_nominatim, parse_bigdatacloud);
    // 优先取城市名解析成功的那一个；两者都成功时以 Nominatim 为准。
    let resolved = if bigdatacloud
        .as_ref()
        .map(|item| item.city.is_some())
        .unwrap_or(false)
    {
        bigdatacloud.or(nominatim)
    } else {
        nominatim.or(bigdatacloud)
    };
    if let Some(resolved) = resolved {
        location.city = resolved.city.or(resolved.district);
        location.region = resolved.region;
        location.country = resolved.country;
    }
    location
}

fn parse_bigdatacloud_reverse_geocode(value: &serde_json::Value) -> Option<ReverseGeocode> {
    let read = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| value.get(*key).and_then(serde_json::Value::as_str))
            .filter(|name| is_human_location_name(name))
            .map(ToOwned::to_owned)
    };
    let city = read(&["city", "locality"]);
    let district = read(&["locality", "postcode"]);
    let region = read(&["principalSubdivision"]);
    let country = read(&["countryName"]);
    (city.is_some() || region.is_some() || country.is_some()).then_some(ReverseGeocode {
        city,
        district,
        region,
        country,
    })
}

fn parse_reverse_geocode(value: &serde_json::Value) -> Option<ReverseGeocode> {
    let address = value.get("address")?;
    let read = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| address.get(*key).and_then(serde_json::Value::as_str))
            .filter(|name| is_human_location_name(name))
            .map(ToOwned::to_owned)
    };
    let city = read(&["city", "municipality", "town", "village"]);
    let district = read(&["city_district", "district", "borough", "county"]);
    let region = read(&["state", "state_district", "region"]);
    let country = read(&["country"]);
    (city.is_some() || district.is_some() || region.is_some() || country.is_some()).then_some(
        ReverseGeocode {
            city,
            district,
            region,
            country,
        },
    )
}

#[cfg(test)]
fn looks_like_todo_candidate(message: &str) -> bool {
    let task_clues = [
        "提醒",
        "待办",
        "要交",
        "要开",
        "要做",
        "截止",
        "记得",
        "别忘",
        "需要完成",
    ];
    let time_clues = [
        "今天", "明天", "后天", "周一", "周二", "周三", "周四", "周五", "周六", "周日", "星期",
        "上午", "下午", "晚上", "点", "号", "月", "年", ":",
    ];
    task_clues.iter().any(|clue| message.contains(clue))
        && time_clues.iter().any(|clue| message.contains(clue))
}

#[cfg(test)]
fn parse_todo_candidate(raw: &str) -> Result<Option<TodoCandidate>, String> {
    let trimmed = raw.trim();
    let without_prefix = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    let json = without_prefix
        .strip_suffix("```")
        .unwrap_or(without_prefix)
        .trim();
    let extraction: TodoExtraction =
        serde_json::from_str(json).map_err(|error| format!("无法解析待办提取结果：{error}"))?;
    Ok(extraction.todo.filter(|todo| {
        !todo.title.trim().is_empty()
            && !todo.deadline_local.trim().is_empty()
            && todo.remind_before_minutes >= 0
    }))
}

fn save_todo_draft(
    connection: &Connection,
    title: &str,
    deadline_local: &str,
    remind_before_minutes: i64,
    source_text: &str,
) -> rusqlite::Result<i64> {
    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    connection.execute(
        "INSERT OR IGNORE INTO todos
            (title, deadline_local, remind_before_minutes, source_text, status, created_at)
         VALUES (?1, ?2, ?3, ?4, 'draft', ?5)",
        params![
            title.trim(),
            deadline_local.trim(),
            remind_before_minutes.max(0),
            source_text.trim(),
            created_at
        ],
    )?;
    Ok(connection.last_insert_rowid())
}

fn load_todos(connection: &Connection) -> rusqlite::Result<Vec<TodoItem>> {
    let mut statement = connection.prepare(
        "SELECT id, title, deadline_local, remind_before_minutes, source_text, status, created_at
         FROM todos ORDER BY CASE status WHEN 'draft' THEN 0 ELSE 1 END, id DESC",
    )?;
    let todos = statement
        .query_map([], |row| {
            Ok(TodoItem {
                id: row.get(0)?,
                title: row.get(1)?,
                deadline_local: row.get(2)?,
                remind_before_minutes: row.get(3)?,
                source_text: row.get(4)?,
                status: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?
        .collect();
    todos
}

/// 只加载待办状态（draft + active），用于完成匹配等操作。
fn load_open_todos(connection: &Connection) -> rusqlite::Result<Vec<TodoItem>> {
    let mut statement = connection.prepare(
        "SELECT id, title, deadline_local, remind_before_minutes, source_text, status, created_at
         FROM todos WHERE status IN ('draft', 'active') ORDER BY id ASC",
    )?;
    let todos = statement
        .query_map([], |row| {
            Ok(TodoItem {
                id: row.get(0)?,
                title: row.get(1)?,
                deadline_local: row.get(2)?,
                remind_before_minutes: row.get(3)?,
                source_text: row.get(4)?,
                status: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?
        .collect();
    todos
}

/// 按标题模糊匹配待办：先精确匹配，再包含匹配；返回全部候选由模型请用户明确。
fn find_todo_candidates<'a>(todos: &'a [TodoItem], title: &str) -> Vec<&'a TodoItem> {
    let needle = title.trim().to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    let exact: Vec<_> = todos
        .iter()
        .filter(|todo| todo.title.to_lowercase() == needle)
        .collect();
    if !exact.is_empty() {
        return exact;
    }
    todos
        .iter()
        .filter(|todo| todo.title.to_lowercase().contains(&needle))
        .collect()
}

/// 解析本地截止时间为 Unix 秒。
fn deadline_timestamp(deadline_local: &str) -> Option<i64> {
    use chrono::TimeZone;

    let deadline = chrono::NaiveDateTime::parse_from_str(deadline_local, "%Y-%m-%dT%H:%M").ok()?;
    chrono::Local
        .from_local_datetime(&deadline)
        .single()
        .map(|dt| dt.timestamp())
}

/// 三档提醒点：截止前 2 小时、30 分钟与 5 分钟各一次。
#[derive(Clone, Copy, Debug, PartialEq)]
enum ReminderPoint {
    TwoHours,
    ThirtyMinutes,
    FiveMinutes,
}

/// 调度器一次要发射的提醒。
struct DueReminder {
    todo: TodoItem,
    point: ReminderPoint,
}

/// 计算三档提醒点的时间戳，按截止时间由远至近排列。
fn reminder_points(deadline_local: &str) -> Option<[(ReminderPoint, i64); 3]> {
    let deadline = deadline_timestamp(deadline_local)?;
    Some([
        (ReminderPoint::TwoHours, deadline - 7_200),
        (ReminderPoint::ThirtyMinutes, deadline - 1_800),
        (ReminderPoint::FiveMinutes, deadline - 300),
    ])
}

fn load_due_todos(connection: &Connection, now: i64) -> rusqlite::Result<Vec<DueReminder>> {
    let mut statement = connection.prepare(
        "SELECT id, title, deadline_local, remind_before_minutes, source_text, status, created_at,
                reminded_2h_at, reminded_30m_at, reminded_5m_at
         FROM todos WHERE status = 'active'
         AND (snoozed_until IS NULL OR snoozed_until <= ?1) ORDER BY id ASC",
    )?;
    let rows = statement
        .query_map(params![now], |row| {
            Ok((
                TodoItem {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    deadline_local: row.get(2)?,
                    remind_before_minutes: row.get(3)?,
                    source_text: row.get(4)?,
                    status: row.get(5)?,
                    created_at: row.get(6)?,
                },
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, Option<i64>>(8)?,
                row.get::<_, Option<i64>>(9)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut due = Vec::new();
    for (todo, reminded_2h_at, reminded_30m_at, reminded_5m_at) in rows {
        let Some(points) = reminder_points(&todo.deadline_local) else {
            continue;
        };
        // 同一次轮询只弹最临近的一条；每个节点都有独立持久化标记，重启也不会重发。
        let pending = [
            (ReminderPoint::FiveMinutes, points[2].1, reminded_5m_at),
            (ReminderPoint::ThirtyMinutes, points[1].1, reminded_30m_at),
            (ReminderPoint::TwoHours, points[0].1, reminded_2h_at),
        ];
        if let Some((point, _, _)) = pending
            .into_iter()
            .find(|(_, scheduled_at, reminded_at)| reminded_at.is_none() && *scheduled_at <= now)
        {
            due.push(DueReminder { todo, point });
        }
    }
    Ok(due)
}

/// 标记某个提醒点已发射。三个节点各自独立，避免漏发或重复发。
fn mark_reminder_point(
    connection: &Connection,
    id: i64,
    point: ReminderPoint,
    at: i64,
) -> rusqlite::Result<()> {
    let column = match point {
        ReminderPoint::TwoHours => "reminded_2h_at",
        ReminderPoint::ThirtyMinutes => "reminded_30m_at",
        ReminderPoint::FiveMinutes => "reminded_5m_at",
    };
    connection.execute(
        &format!("UPDATE todos SET {column} = ?1 WHERE id = ?2 AND status = 'active'"),
        params![at, id],
    )?;
    Ok(())
}

/// 成功展示最近的一档提醒后，一并消费已经错过的更早节点。
/// 例如设备从睡眠中恢复时已进入截止前 5 分钟，只展示一次 5 分钟提醒，
/// 不会在随后两个轮询周期继续补发 30 分钟与 2 小时提醒。
fn mark_delivered_reminder(
    connection: &Connection,
    reminder: &DueReminder,
    at: i64,
) -> rusqlite::Result<()> {
    let Some(points) = reminder_points(&reminder.todo.deadline_local) else {
        return Ok(());
    };
    for (point, scheduled_at) in points {
        if scheduled_at <= at {
            mark_reminder_point(connection, reminder.todo.id, point, at)?;
        }
    }
    Ok(())
}

/// 创建/确认待办时跳过已经过去的提醒点，避免刚确认就补发历史提醒。
fn initialize_reminder_points(
    connection: &Connection,
    id: i64,
    deadline_local: &str,
    now: i64,
) -> rusqlite::Result<()> {
    let Some(points) = reminder_points(deadline_local) else {
        return Ok(());
    };
    for (point, scheduled_at) in points {
        if scheduled_at <= now {
            mark_reminder_point(connection, id, point, now)?;
        }
    }
    Ok(())
}
fn complete_todo(connection: &Connection, id: i64) -> rusqlite::Result<()> {
    connection.execute(
        "UPDATE todos SET status = 'completed' WHERE id = ?1 AND status = 'active'",
        params![id],
    )?;
    Ok(())
}

fn snooze_todo(connection: &Connection, id: i64, until: i64) -> rusqlite::Result<()> {
    connection.execute(
        "UPDATE todos SET reminded_2h_at = NULL, reminded_30m_at = NULL, reminded_5m_at = NULL,
            snoozed_until = ?1 WHERE id = ?2 AND status = 'active'",
        params![until, id],
    )?;
    Ok(())
}

fn todo_reminder_message(todo: &TodoItem, point: ReminderPoint, now: i64) -> String {
    let time = todo
        .deadline_local
        .split_once('T')
        .map(|(_, time)| time)
        .unwrap_or(&todo.deadline_local);
    if deadline_timestamp(&todo.deadline_local).is_some_and(|deadline| deadline <= now) {
        return format!("提醒你一下：“{}”已到截止时间（{time}）。", todo.title);
    }
    let lead_time = match point {
        ReminderPoint::TwoHours => "2 小时",
        ReminderPoint::ThirtyMinutes => "30 分钟",
        ReminderPoint::FiveMinutes => "5 分钟",
    };
    format!(
        "提醒你一下：距离“{}”到期还有 {lead_time}（{time}）。",
        todo.title
    )
}
fn start_reminder_scheduler(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            if !pet_frontend_ready(&app) {
                tokio::time::sleep(Duration::from_millis(250)).await;
                continue;
            }
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            let due = if let Some(database) = app.try_state::<Database>() {
                match database.0.lock() {
                    Ok(connection) => {
                        let _ = flush_usage_records(&connection);
                        load_due_todos(&connection, now).unwrap_or_default()
                    }
                    Err(_) => Vec::new(),
                }
            } else {
                Vec::new()
            };
            for reminder in due {
                ensure_pet_visible(&app);
                if let Some(window) = app.get_webview_window("pet") {
                    let _ = window.show();
                }
                if app
                    .emit_to(
                        "pet",
                        "todo-reminder",
                        TodoReminder {
                            id: reminder.todo.id,
                            message: todo_reminder_message(&reminder.todo, reminder.point, now),
                        },
                    )
                    .is_ok()
                {
                    if let Some(database) = app.try_state::<Database>() {
                        if let Ok(connection) = database.0.lock() {
                            let _ = mark_delivered_reminder(&connection, &reminder, now);
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(15)).await;
        }
    });
}

fn start_mailbox_scheduler(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut next_due: HashMap<i64, i64> = HashMap::new();
        let mut failures: HashMap<i64, u32> = HashMap::new();
        loop {
            if !pet_frontend_ready(&app) {
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
            let now = now_unix();
            let accounts = app
                .try_state::<Database>()
                .and_then(|database| {
                    database
                        .0
                        .lock()
                        .ok()
                        .and_then(|connection| storage::load_email_accounts(&connection).ok())
                })
                .unwrap_or_default()
                .into_iter()
                .filter(|account| account.enabled && account.inbox_enabled)
                .collect::<Vec<_>>();
            for account in accounts {
                if next_due.get(&account.id).copied().unwrap_or(0) > now {
                    continue;
                }
                let normal_delay = account.inbox_interval_minutes.clamp(5, 30) * 60;
                let credential = email_credential_entry(account.id)
                    .and_then(|entry| entry.get_password().map_err(|error| error.to_string()));
                let cursor_kind = if account.provider == "gmail" {
                    "gmail_history"
                } else {
                    "imap_uid"
                };
                let previous_cursor = app.try_state::<Database>().and_then(|database| {
                    database.0.lock().ok().and_then(|connection| {
                        storage::load_mailbox_cursor(&connection, account.id)
                            .ok()
                            .flatten()
                            .filter(|(kind, _)| kind == cursor_kind)
                            .map(|(_, value)| value)
                    })
                });
                let result: Result<email::ImapDelta, String> = async {
                    let secret = credential.map_err(|_| "邮箱凭据不可用".to_string())?;
                    if account.provider == "gmail" {
                        let mut gmail: email::GmailCredential = serde_json::from_str(&secret)
                            .map_err(|_| "Gmail 授权格式无效，请重新连接".to_string())?;
                        let client = shared_http_client(30);
                        if email::refresh_gmail_token(
                            &client,
                            &google_oauth_client_id()?,
                            google_oauth_client_secret(),
                            &mut gmail,
                            now,
                        )
                        .await?
                        {
                            email_credential_entry(account.id)?
                                .set_password(
                                    &serde_json::to_string(&gmail)
                                        .map_err(|_| "无法保存 Gmail 授权".to_string())?,
                                )
                                .map_err(|_| "无法更新 Gmail 授权".to_string())?;
                        }
                        email::check_gmail(&client, &gmail, previous_cursor.as_deref()).await
                    } else {
                        tokio::time::timeout(
                            Duration::from_secs(45),
                            email::check_imap(&account, &secret, previous_cursor.as_deref()),
                        )
                        .await
                        .map_err(|_| "IMAP 检查超时".to_string())?
                    }
                }
                .await;
                match result {
                    Ok(delta) => {
                        let salt = mail_hash_salt();
                        let new_messages = if let (Ok(salt), Some(database)) =
                            (salt, app.try_state::<Database>())
                        {
                            if let Ok(connection) = database.0.lock() {
                                (|| -> rusqlite::Result<Vec<email::InboxMessageMetadata>> {
                                    let transaction = connection.unchecked_transaction()?;
                                    let mut unseen = Vec::new();
                                    for message in delta.messages {
                                        let message_hash = hash_mail_message_id(
                                            &salt,
                                            account.id,
                                            &message.provider_message_id,
                                        );
                                        if storage::record_mail_notification_once(
                                            &transaction,
                                            account.id,
                                            &message_hash,
                                            now,
                                        )? {
                                            unseen.push(message);
                                        }
                                    }
                                    storage::save_mailbox_cursor(
                                        &transaction,
                                        account.id,
                                        cursor_kind,
                                        &delta.cursor,
                                        now,
                                    )?;
                                    storage::delete_expired_mail_receipts(&transaction, now)?;
                                    transaction.commit()?;
                                    Ok(unseen)
                                })()
                                .ok()
                            } else {
                                None
                            }
                        } else {
                            None
                        };
                        if let Some(messages) = new_messages {
                            failures.remove(&account.id);
                            next_due.insert(account.id, now.saturating_add(normal_delay));
                            if !messages.is_empty() {
                                let notification = if account.inbox_detail_level == "metadata" {
                                    let summaries = messages
                                        .iter()
                                        .take(3)
                                        .map(|message| {
                                            let sender =
                                                message.sender.chars().take(80).collect::<String>();
                                            let subject = message
                                                .subject
                                                .chars()
                                                .take(100)
                                                .collect::<String>();
                                            format!("{sender}：{subject}")
                                        })
                                        .collect::<Vec<_>>()
                                        .join("；");
                                    format!(
                                        "{} 收到 {} 封新邮件。{}",
                                        account.account_address,
                                        messages.len(),
                                        summaries
                                    )
                                } else {
                                    format!(
                                        "{} 收到 {} 封新邮件。",
                                        account.account_address,
                                        messages.len()
                                    )
                                };
                                ensure_pet_visible(&app);
                                if let Some(window) = app.get_webview_window("pet") {
                                    let _ = window.show();
                                }
                                let _ = app.emit_to("pet", "assistant-message", notification);
                            }
                        } else {
                            let count = failures.entry(account.id).or_default();
                            *count = count.saturating_add(1);
                            let delay_minutes = 10_i64.saturating_mul(1_i64 << (*count - 1).min(3));
                            next_due
                                .insert(account.id, now.saturating_add(delay_minutes.min(60) * 60));
                        }
                    }
                    Err(_) => {
                        let count = failures.entry(account.id).or_default();
                        *count = count.saturating_add(1);
                        let delay_minutes = 10_i64.saturating_mul(1_i64 << (*count - 1).min(3));
                        next_due.insert(account.id, now.saturating_add(delay_minutes.min(60) * 60));
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    });
}

fn confirm_todo(
    connection: &Connection,
    id: i64,
    title: &str,
    deadline_local: &str,
    remind_before_minutes: i64,
) -> rusqlite::Result<()> {
    connection.execute(
        "UPDATE todos SET title = ?1, deadline_local = ?2,
            remind_before_minutes = ?3, status = 'active'
         WHERE id = ?4 AND status = 'draft'",
        params![
            title.trim(),
            deadline_local.trim(),
            remind_before_minutes.max(0),
            id
        ],
    )?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    initialize_reminder_points(connection, id, deadline_local.trim(), now)?;
    Ok(())
}

fn load_privacy_config(connection: &Connection) -> rusqlite::Result<PrivacyConfig> {
    connection.query_row(
        "SELECT screen_observation, auto_location, proactive_interaction,
                file_search_desktop, file_search_documents, file_search_downloads
         FROM privacy_config WHERE id = 1",
        [],
        |row| {
            Ok(PrivacyConfig {
                screen_observation: row.get::<_, i64>(0)? != 0,
                auto_location: row.get::<_, i64>(1)? != 0,
                proactive_interaction: row.get::<_, i64>(2)? != 0,
                file_search_desktop: row.get::<_, i64>(3)? != 0,
                file_search_documents: row.get::<_, i64>(4)? != 0,
                file_search_downloads: row.get::<_, i64>(5)? != 0,
            })
        },
    )
}

fn persist_privacy_config(connection: &Connection, config: &PrivacyConfig) -> rusqlite::Result<()> {
    connection.execute(
        "UPDATE privacy_config
         SET screen_observation = ?1, auto_location = ?2, proactive_interaction = ?3,
             file_search_desktop = ?4, file_search_documents = ?5, file_search_downloads = ?6
         WHERE id = 1",
        params![
            if config.screen_observation { 1 } else { 0 },
            if config.auto_location { 1 } else { 0 },
            if config.proactive_interaction { 1 } else { 0 },
            if config.file_search_desktop { 1 } else { 0 },
            if config.file_search_documents { 1 } else { 0 },
            if config.file_search_downloads { 1 } else { 0 },
        ],
    )?;
    Ok(())
}

fn is_loopback_model_url(base_url: &str) -> bool {
    let authority = base_url
        .split_once("://")
        .map(|(_, remainder)| remainder)
        .unwrap_or(base_url)
        .split('/')
        .next()
        .unwrap_or_default();
    let host = if authority.starts_with('[') {
        authority
            .split(']')
            .next()
            .unwrap_or_default()
            .trim_start_matches('[')
    } else {
        authority.split(':').next().unwrap_or_default()
    };
    matches!(
        host.to_ascii_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "::1"
    )
}

async fn extract_memories_with_model(
    provider: &ModelProviderSettings,
    api_key: &str,
    messages: &[Message],
) -> Result<Vec<MemoryCandidate>, String> {
    let transcript = messages
        .iter()
        .map(|message| format!("{}：{}", message.role, message.content))
        .collect::<Vec<_>>()
        .join("\n");
    let seed = vec![Message {
        role: "user".to_string(),
        content: transcript.clone(),
        created_at: 0,
        source: "chat".to_string(),
    }];
    let mut request = build_model_request(provider, &seed)?;
    match provider.protocol.as_str() {
        "responses" => {
            request.body["instructions"] = serde_json::json!(MEMORY_EXTRACTION_PROMPT);
            request.body["input"] = serde_json::json!([{"role": "user", "content": transcript}]);
            request.body["max_output_tokens"] = serde_json::json!(500);
        }
        "chat_completions" => {
            request.body["messages"] = serde_json::json!([
                {"role": "system", "content": MEMORY_EXTRACTION_PROMPT},
                {"role": "user", "content": transcript}
            ]);
            request.body["max_tokens"] = serde_json::json!(500);
        }
        _ => return Err("不支持当前记忆抽取协议".to_string()),
    }
    request.body["stream"] = serde_json::json!(false);
    let client = shared_http_client(90);
    let (status, body) = send_model_request(&client, &request.url, api_key, &request.body).await?;
    if !status.is_success() {
        return Err(format!("记忆抽取请求失败（HTTP {status}）"));
    }
    let response: serde_json::Value =
        serde_json::from_str(&body).map_err(|error| format!("记忆抽取返回了无效数据：{error}"))?;
    parse_memory_candidates(&extract_model_reply(&provider.protocol, &response)?)
}

fn load_pet_state(connection: &Connection) -> rusqlite::Result<PetState> {
    connection.query_row(
        "SELECT locked, x, y, last_active_at FROM pet_state WHERE id = 1",
        [],
        |row| {
            let x: Option<i32> = row.get(1)?;
            let y: Option<i32> = row.get(2)?;
            Ok(PetState {
                locked: row.get::<_, i32>(0)? != 0,
                position: x.zip(y),
                last_active_at: row.get(3)?,
            })
        },
    )
}

fn save_pet_locked(connection: &Connection, locked: bool) -> rusqlite::Result<()> {
    connection.execute(
        "UPDATE pet_state SET locked = ?1 WHERE id = 1",
        params![locked],
    )?;
    Ok(())
}

fn save_pet_position(connection: &Connection, x: i32, y: i32) -> rusqlite::Result<()> {
    connection.execute(
        "UPDATE pet_state SET x = ?1, y = ?2 WHERE id = 1",
        params![x, y],
    )?;
    Ok(())
}

fn mark_active(connection: &Connection) -> rusqlite::Result<()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    connection.execute(
        "UPDATE pet_state SET last_active_at = ?1 WHERE id = 1",
        params![now],
    )?;
    Ok(())
}

#[tauri::command]
fn get_pet_state(database: State<'_, Database>) -> Result<PetState, String> {
    let connection = database.0.lock().map_err(|error| error.to_string())?;
    load_pet_state(&connection).map_err(|error| error.to_string())
}

#[tauri::command]
fn set_pet_locked(
    locked: bool,
    database: State<'_, Database>,
    motion: State<'_, PetMotionState>,
) -> Result<(), String> {
    let connection = database.0.lock().map_err(|error| error.to_string())?;
    save_pet_locked(&connection, locked).map_err(|error| error.to_string())?;
    drop(connection);
    if let Ok(mut state) = motion.0.lock() {
        state.locked = locked;
        // 锁定即钉住：立即取消自动移动与收起中的过渡。
        if locked && state.state != MotionState::NormalIdle {
            state.state = MotionState::NormalIdle;
            state.auto_moving = false;
        }
    }
    Ok(())
}

#[tauri::command]
fn hide_pet(
    app: AppHandle,
    database: State<'_, Database>,
    motion: State<'_, PetMotionState>,
) -> Result<(), String> {
    let connection = database.0.lock().map_err(|error| error.to_string())?;
    mark_active(&connection).map_err(|error| error.to_string())?;
    drop(connection);
    mark_pet_interaction(&motion);

    app.get_webview_window("pet")
        .ok_or_else(|| "pet window is missing".to_string())?
        .hide()
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn open_composer(app: AppHandle) -> Result<(), String> {
    if let Some(motion) = app.try_state::<PetMotionState>() {
        mark_pet_interaction(&motion);
    }
    let composer = app
        .get_webview_window("composer")
        .ok_or_else(|| "composer window is missing".to_string())?;
    let monitor = composer
        .primary_monitor()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "primary monitor is missing".to_string())?;
    let monitor_position = monitor.position();
    let monitor_size = monitor.size();
    let composer_size = composer.outer_size().map_err(|error| error.to_string())?;
    let x = monitor_position.x + monitor_size.width.saturating_sub(composer_size.width) as i32 / 2;
    let y = monitor_position.y
        + monitor_size
            .height
            .saturating_sub(composer_size.height + 48) as i32;
    composer
        .set_position(PhysicalPosition::new(x, y))
        .map_err(|error| error.to_string())?;
    composer.show().map_err(|error| error.to_string())?;
    composer.set_focus().map_err(|error| error.to_string())
}

#[tauri::command]
fn hide_composer(app: AppHandle) -> Result<(), String> {
    app.get_webview_window("composer")
        .ok_or_else(|| "composer window is missing".to_string())?
        .hide()
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn get_companion_state(session: State<'_, CompanionSession>) -> Result<bool, String> {
    session
        .0
        .lock()
        .map(|state| state.active)
        .map_err(|error| error.to_string())
}

/// 功能模式状态：普通 = 非陪伴 且 非工作；陪伴与工作可同时开启。
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModeState {
    companion: bool,
    // 工作模式（Agent/Skills）为后续计划，当前恒为 false，保留状态位便于将来扩展。
    work: bool,
    next_proactive_at: Option<i64>,
    proactive_retry_count: u8,
    last_proactive_result: Option<String>,
}

#[tauri::command]
fn get_mode_state(session: State<'_, CompanionSession>) -> Result<ModeState, String> {
    let state = session.0.lock().map_err(|error| error.to_string())?;
    Ok(ModeState {
        companion: state.active,
        work: false,
        next_proactive_at: state.next_proactive_at,
        proactive_retry_count: state.proactive_retry_count,
        last_proactive_result: state.last_proactive_result.clone(),
    })
}

/// 双击角色或托盘停止：与 `/停止陪伴` 一样写会话状态并同步面板。
#[tauri::command]
fn stop_companion_observation(
    app: AppHandle,
    session: State<'_, CompanionSession>,
) -> Result<(), String> {
    let mut state = session.0.lock().map_err(|error| error.to_string())?;
    halt_companion(&mut state);
    drop(state);
    let _ = app.emit_to("panel", "mode-changed", ());
    let _ = notify_mode_change(&app, "停止陪伴");
    Ok(())
}

/// 面板切换陪伴模式。开启需屏幕观察授权（与 /陪伴模式 命令一致）；关闭即停。
#[tauri::command]
fn set_companion_mode(
    enabled: bool,
    app: AppHandle,
    database: State<'_, Database>,
    session: State<'_, CompanionSession>,
) -> Result<(), String> {
    let was_enabled = session.0.lock().map_err(|error| error.to_string())?.active;
    if enabled == was_enabled {
        return Ok(()); // 状态未变化，不重复处理。
    }
    if enabled {
        let connection = database.0.lock().map_err(|error| error.to_string())?;
        let privacy = load_privacy_config(&connection)
            .map_err(|error| format_error_chain("读取隐私配置失败", &error))?;
        if !privacy.screen_observation {
            return Err(
                "开启陪伴模式需要屏幕观察授权。请在“隐私设置”中开启“允许观察屏幕内容”".to_string(),
            );
        }
    }
    {
        let mut state = session.0.lock().map_err(|error| error.to_string())?;
        if enabled {
            activate_companion(&mut state);
        } else {
            halt_companion(&mut state);
        }
    }
    let _ = app.emit_to("panel", "mode-changed", ());
    // 让模型感知模式切换。
    let _ = notify_mode_change(
        &app,
        if enabled {
            "陪伴模式"
        } else {
            "停止陪伴"
        },
    );
    // 模式真的切换了才向宠物气泡提示（状态未变化时上面的早退分支已拦截）。
    let bubble = if enabled {
        "陪伴模式已开启 ✨ 我会在合适的时候陪你聊聊天～想停止的话随时双击我或输入 /停止陪伴"
    } else {
        "陪伴模式已停止，需要我的时候随时叫我 💙"
    };
    let _ = app.emit_to("pet", "assistant-message", bubble);
    if enabled {
        ensure_pet_visible(&app);
        crate::activity::spawn_companion_start_feedback(&app);
    }
    Ok(())
}

#[tauri::command]
fn get_privacy_config(database: State<'_, Database>) -> Result<PrivacyConfig, String> {
    let connection = database.0.lock().map_err(|error| error.to_string())?;
    load_privacy_config(&connection).map_err(|error| format_error_chain("读取隐私配置失败", &error))
}

#[tauri::command]
fn save_privacy_config(
    app: AppHandle,
    database: State<'_, Database>,
    session: State<'_, CompanionSession>,
    config: PrivacyConfig,
) -> Result<PrivacyConfig, String> {
    let connection = database.0.lock().map_err(|error| error.to_string())?;
    let before = load_privacy_config(&connection)
        .map_err(|error| format_error_chain("读取隐私配置失败", &error))?;
    persist_privacy_config(&connection, &config)
        .map_err(|error| format_error_chain("保存隐私配置失败", &error))?;
    let after = load_privacy_config(&connection)
        .map_err(|error| format_error_chain("校验隐私配置失败", &error))?;
    drop(connection);

    // 撤销屏幕观察授权 = 立即停止陪伴采集，保证"未授权不得截图"。
    if companion_must_stop_on_privacy_revoke(&before, &after) {
        let was_active = {
            let mut guard = session.0.lock().map_err(|error| error.to_string())?;
            let was_active = guard.active;
            halt_companion(&mut guard);
            was_active
        };
        let _ = app.emit_to("panel", "mode-changed", ());
        let _ = notify_mode_change(&app, "停止陪伴");
        if was_active {
            let _ = app.emit_to(
                "pet",
                "assistant-message",
                "屏幕观察授权已关闭，我已停止陪伴模式，不再读取屏幕内容。需要时可在隐私设置重新开启授权后输入 /陪伴模式。",
            );
        } else {
            let _ = notify_privacy_change(&app, "屏幕观察", false);
        }
    } else if before.screen_observation != after.screen_observation {
        let _ = notify_privacy_change(&app, "屏幕观察", after.screen_observation);
    }
    if before.auto_location != after.auto_location {
        let _ = notify_privacy_change(&app, "地理位置", after.auto_location);
    }
    Ok(after)
}

/// 开机自启注册表项：HKCU\...\CurrentVersion\Run 下的 Cyral 值，无需管理员权限。
const AUTOSTART_RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const AUTOSTART_VALUE_NAME: &str = "Cyral";

fn set_autostart_registry(enabled: bool) -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::WIN32_ERROR;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyW, RegDeleteValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
        REG_SZ,
    };

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    unsafe {
        let mut key: HKEY = HKEY(std::ptr::null_mut());
        let path = wide(AUTOSTART_RUN_KEY);
        // 键不存在时自动创建（部分精简系统缺少 Run 键）。
        let error = RegCreateKeyW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr()), &mut key);
        if !error.is_ok() {
            return Err(format!("无法创建/打开开机自启注册表项：{error:?}"));
        }
        let name = wide(AUTOSTART_VALUE_NAME);
        let result: Result<(), String> = if enabled {
            let exe = std::env::current_exe().map_err(|error| error.to_string())?;
            let value = format!("\"{}\"", exe.display());
            let value_utf16 = wide(&value);
            let bytes = std::slice::from_raw_parts(
                value_utf16.as_ptr() as *const u8,
                value_utf16.len() * 2,
            );
            let error = RegSetValueExW(key, PCWSTR(name.as_ptr()), None, REG_SZ, Some(bytes));
            if error.is_ok() {
                Ok(())
            } else {
                Err(format!("写入开机自启失败：{error:?}"))
            }
        } else {
            let error = RegDeleteValueW(key, PCWSTR(name.as_ptr()));
            if error.is_ok() || error == WIN32_ERROR(2) {
                Ok(())
            } else {
                Err(format!("移除开机自启失败：{error:?}"))
            }
        };
        let _ = RegCloseKey(key);
        result
    }
}

#[tauri::command]
fn get_app_settings(database: State<'_, Database>) -> Result<crate::storage::AppSettings, String> {
    let connection = database.0.lock().map_err(|error| error.to_string())?;
    crate::storage::load_app_settings(&connection).map_err(|error| error.to_string())
}

#[tauri::command]
fn save_app_settings(
    settings: crate::storage::AppSettings,
    database: State<'_, Database>,
) -> Result<crate::storage::AppSettings, String> {
    let connection = database.0.lock().map_err(|error| error.to_string())?;
    let before =
        crate::storage::load_app_settings(&connection).map_err(|error| error.to_string())?;
    crate::storage::save_app_settings(&connection, &settings).map_err(|error| error.to_string())?;
    drop(connection);
    if before.autostart != settings.autostart {
        set_autostart_registry(settings.autostart)?;
    }
    Ok(settings)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EmailAccountView {
    #[serde(flatten)]
    account: storage::EmailAccount,
    credential_configured: bool,
}

fn email_credential_entry(account_id: i64) -> Result<keyring::Entry, String> {
    keyring::Entry::new("app.cyral.mail", &format!("account-{account_id}"))
        .map_err(|error| format!("无法访问 Windows 凭据管理器：{error}"))
}

fn mail_hash_salt() -> Result<String, String> {
    let entry = keyring::Entry::new("app.cyral.mail", "notification-hash-salt")
        .map_err(|error| format!("无法访问 Windows 凭据管理器：{error}"))?;
    if let Ok(value) = entry.get_password() {
        if !value.is_empty() {
            return Ok(value);
        }
    }
    let salt = hex::encode(rand::random::<[u8; 32]>());
    entry
        .set_password(&salt)
        .map_err(|error| format!("无法保存邮件通知去重密钥：{error}"))?;
    Ok(salt)
}

fn hash_mail_message_id(salt: &str, account_id: i64, provider_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(salt.as_bytes());
    digest.update(account_id.to_le_bytes());
    digest.update(provider_id.as_bytes());
    hex::encode(digest.finalize())
}

fn google_oauth_client_id() -> Result<String, String> {
    option_env!("CYRAL_GOOGLE_CLIENT_ID")
        .map(str::to_string)
        .or_else(|| std::env::var("CYRAL_GOOGLE_CLIENT_ID").ok())
        .filter(|value| value.ends_with(".apps.googleusercontent.com"))
        .ok_or_else(|| "此构建未配置 CYRAL_GOOGLE_CLIENT_ID，暂时不能连接 Gmail".to_string())
}

fn google_oauth_client_secret() -> Option<String> {
    option_env!("CYRAL_GOOGLE_CLIENT_SECRET")
        .map(str::to_string)
        .or_else(|| std::env::var("CYRAL_GOOGLE_CLIENT_SECRET").ok())
        .filter(|value| !value.trim().is_empty())
}

#[cfg(windows)]
fn open_oauth_url(url: &str) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    if !url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?") {
        return Err("拒绝打开非 Google OAuth 地址".to_string());
    }
    let wide_url: Vec<u16> = std::ffi::OsStr::new(url)
        .encode_wide()
        .chain(Some(0))
        .collect();
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_FLAG_NO_UI,
        lpFile: PCWSTR(wide_url.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info) }
        .map_err(|error| format!("Windows 未能打开 Google 授权页：{error}"))
}

#[cfg(not(windows))]
fn open_oauth_url(_url: &str) -> Result<(), String> {
    Err("当前系统不支持打开 Google 授权页".to_string())
}

fn email_account_has_credential(account: &storage::EmailAccount) -> bool {
    let Ok(secret) = email_credential_entry(account.id)
        .and_then(|entry| entry.get_password().map_err(|error| error.to_string()))
    else {
        return false;
    };
    if account.provider == "gmail" {
        serde_json::from_str::<email::GmailCredential>(&secret)
            .map(|credential| {
                !credential.refresh_token.is_empty() && !credential.access_token.is_empty()
            })
            .unwrap_or(false)
    } else {
        !secret.trim().is_empty()
    }
}

fn email_account_view(account: storage::EmailAccount) -> EmailAccountView {
    let credential_configured = email_account_has_credential(&account);
    EmailAccountView {
        account,
        credential_configured,
    }
}

fn validate_email_account(account: &mut storage::EmailAccountInput) -> Result<(), String> {
    account.provider = account.provider.trim().to_lowercase();
    if !matches!(
        account.provider.as_str(),
        "gmail" | "qq" | "netease" | "custom"
    ) {
        return Err("邮箱服务商必须是 Gmail、QQ、网易或自定义".to_string());
    }
    account.account_address = account.account_address.trim().to_string();
    if !email::valid_email_address(&account.account_address) {
        return Err("发件账户必须是完整邮箱地址".to_string());
    }
    if !matches!(account.inbox_interval_minutes, 5 | 10 | 30) {
        return Err("收件检查周期只能是 5、10 或 30 分钟".to_string());
    }
    if !matches!(account.inbox_detail_level.as_str(), "count" | "metadata") {
        return Err("新邮件提醒级别无效".to_string());
    }
    if account.provider == "qq" {
        account.smtp_host = "smtp.qq.com".to_string();
        account.smtp_port = 465;
        account.smtp_tls = "implicit".to_string();
        account.imap_host = "imap.qq.com".to_string();
        account.imap_port = 993;
        account.imap_tls = "implicit".to_string();
    } else if account.provider == "netease" {
        account.smtp_host = "smtp.163.com".to_string();
        account.smtp_port = 465;
        account.smtp_tls = "implicit".to_string();
        account.imap_host = "imap.163.com".to_string();
        account.imap_port = 993;
        account.imap_tls = "implicit".to_string();
    }
    if account.provider != "gmail"
        && (account.smtp_host.trim().is_empty() || account.imap_host.trim().is_empty())
    {
        return Err("SMTP 与 IMAP 服务器不能为空".to_string());
    }
    if !(1..=65_535).contains(&account.smtp_port) || !(1..=65_535).contains(&account.imap_port) {
        return Err("邮件服务器端口必须在 1 到 65535 之间".to_string());
    }
    if !matches!(account.smtp_tls.as_str(), "implicit" | "starttls")
        || !matches!(account.imap_tls.as_str(), "implicit" | "starttls")
    {
        return Err("TLS 模式必须是 implicit 或 starttls".to_string());
    }
    Ok(())
}

#[tauri::command]
fn get_email_accounts(database: State<'_, Database>) -> Result<Vec<EmailAccountView>, String> {
    let connection = database.0.lock().map_err(|error| error.to_string())?;
    storage::load_email_accounts(&connection)
        .map_err(|error| error.to_string())
        .map(|accounts| accounts.into_iter().map(email_account_view).collect())
}

#[tauri::command]
fn save_email_account(
    mut account: storage::EmailAccountInput,
    credential: Option<String>,
    database: State<'_, Database>,
) -> Result<EmailAccountView, String> {
    validate_email_account(&mut account)?;
    let saved = {
        let mut connection = database.0.lock().map_err(|error| error.to_string())?;
        if let Some(existing_id) =
            storage::find_email_account_id(&connection, &account.provider, &account.account_address)
                .map_err(|error| error.to_string())?
        {
            if account.id != Some(existing_id) {
                return Err("该邮箱账户已经存在，请从上方账户列表选择它后再修改设置".to_string());
            }
        }
        storage::save_email_account(&mut connection, &account, now_unix())
            .map_err(|error| error.to_string())?
    };
    if let Some(secret) = credential.filter(|value| !value.trim().is_empty()) {
        email_credential_entry(saved.id)?
            .set_password(secret.trim())
            .map_err(|error| format!("账户配置已保存，但凭据写入失败：{error}"))?;
    }
    Ok(email_account_view(saved))
}

#[tauri::command]
fn delete_email_account(id: i64, database: State<'_, Database>) -> Result<(), String> {
    let removed = {
        let mut connection = database.0.lock().map_err(|error| error.to_string())?;
        storage::delete_email_account(&mut connection, id).map_err(|error| error.to_string())?
    };
    if !removed {
        return Err("邮箱账户不存在".to_string());
    }
    if let Ok(entry) = email_credential_entry(id) {
        let _ = entry.delete_credential();
    }
    Ok(())
}

#[tauri::command]
async fn connect_gmail_account(
    id: i64,
    database: State<'_, Database>,
) -> Result<EmailAccountView, String> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use sha2::{Digest, Sha256};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let client_id = google_oauth_client_id()?;
    let account = {
        let connection = database.0.lock().map_err(|error| error.to_string())?;
        storage::load_email_account(&connection, id)
            .map_err(|_| "Gmail 账户不存在，请先保存账户".to_string())?
    };
    if account.provider != "gmail" {
        return Err("只有 Gmail 账户使用 OAuth 连接".to_string());
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| "无法启动本机 OAuth 回调端口".to_string())?;
    let port = listener
        .local_addr()
        .map_err(|_| "无法读取本机 OAuth 回调端口".to_string())?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}/oauth/callback");
    let state = hex::encode(rand::random::<[u8; 24]>());
    let verifier = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 48]>());
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut authorization = reqwest::Url::parse("https://accounts.google.com/o/oauth2/v2/auth")
        .map_err(|_| "无法构造 Google OAuth 地址".to_string())?;
    authorization.query_pairs_mut().extend_pairs([
        ("client_id", client_id.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("response_type", "code"),
        (
            "scope",
            "https://www.googleapis.com/auth/gmail.send https://www.googleapis.com/auth/gmail.metadata",
        ),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("state", state.as_str()),
        ("access_type", "offline"),
        ("prompt", "consent"),
        ("login_hint", account.account_address.as_str()),
    ]);
    open_oauth_url(authorization.as_str())?;

    let callback = tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            let (mut stream, _) = listener
                .accept()
                .await
                .map_err(|_| "无法接收 Google OAuth 回调".to_string())?;
            let mut buffer = vec![0_u8; 16_384];
            let length = stream
                .read(&mut buffer)
                .await
                .map_err(|_| "无法读取 Google OAuth 回调".to_string())?;
            let request = String::from_utf8_lossy(&buffer[..length]);
            let target = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("/");
            let parsed = reqwest::Url::parse(&format!("http://127.0.0.1{target}"))
                .map_err(|_| "Google OAuth 回调地址无效".to_string())?;
            if parsed.path() != "/oauth/callback" {
                let _ = stream
                    .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                    .await;
                continue;
            }
            let parameters = parsed.query_pairs().collect::<HashMap<_, _>>();
            let returned_state = parameters.get("state").map(|value| value.as_ref());
            let code = parameters.get("code").map(|value| value.to_string());
            let success = returned_state == Some(state.as_str()) && code.is_some();
            let body = if success {
                "Cyral 已收到授权码，正在完成令牌交换、账户校验与安全保存。请回到 Cyral 查看最终结果。"
            } else {
                "Gmail 授权已取消或校验失败，可以关闭此页面。"
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes()).await;
            if returned_state != Some(state.as_str()) {
                continue;
            }
            return code.ok_or_else(|| "Google OAuth 授权已取消".to_string());
        }
    })
    .await
    .map_err(|_| "等待 Gmail 授权超时".to_string())??;
    let credential = email::exchange_gmail_code(
        &shared_http_client(30),
        &client_id,
        google_oauth_client_secret(),
        &callback,
        &verifier,
        &redirect_uri,
        now_unix(),
    )
    .await?;
    let client = shared_http_client(30);
    let profile = email::get_gmail_profile(&client, &credential).await?;
    if !profile
        .email_address
        .eq_ignore_ascii_case(account.account_address.trim())
    {
        return Err(format!(
            "Google 授权的是 {}，但当前配置的是 {}。请修改邮箱地址后重新连接",
            profile.email_address, account.account_address
        ));
    }
    let secret =
        serde_json::to_string(&credential).map_err(|_| "无法序列化 Gmail 授权".to_string())?;
    let entry = email_credential_entry(account.id)?;
    entry
        .set_password(&secret)
        .map_err(|error| format!("Gmail 已授权，但刷新令牌写入失败：{error}"))?;
    let verified = entry
        .get_password()
        .map_err(|error| format!("Gmail 凭据写入后无法读回：{error}"))?;
    let verified: email::GmailCredential = serde_json::from_str(&verified)
        .map_err(|_| "Gmail 凭据写入后校验失败，请重新连接".to_string())?;
    if verified.refresh_token.is_empty() || verified.access_token.is_empty() {
        let _ = entry.delete_credential();
        return Err("Gmail 凭据写入后内容不完整，请重新连接".to_string());
    }
    if account.inbox_enabled {
        let connection = database.0.lock().map_err(|error| error.to_string())?;
        if let Err(error) = storage::save_mailbox_cursor(
            &connection,
            account.id,
            "gmail_history",
            &profile.history_id,
            now_unix(),
        ) {
            let _ = entry.delete_credential();
            return Err(format!("Gmail 已授权，但无法建立收件箱基线：{error}"));
        }
    }
    Ok(email_account_view(account))
}

#[tauri::command]
fn disconnect_gmail_account(
    id: i64,
    database: State<'_, Database>,
) -> Result<EmailAccountView, String> {
    let account = {
        let connection = database.0.lock().map_err(|error| error.to_string())?;
        storage::load_email_account(&connection, id).map_err(|_| "Gmail 账户不存在".to_string())?
    };
    if account.provider != "gmail" {
        return Err("只有 Gmail 账户使用 OAuth 连接".to_string());
    }
    email_credential_entry(id)?
        .delete_credential()
        .map_err(|error| format!("无法删除 Gmail 授权：{error}"))?;
    Ok(email_account_view(account))
}

#[tauri::command]
async fn test_email_account_connection(
    id: i64,
    database: State<'_, Database>,
) -> Result<String, String> {
    let account = {
        let connection = database.0.lock().map_err(|error| error.to_string())?;
        storage::load_email_account(&connection, id).map_err(|_| "邮箱账户不存在".to_string())?
    };
    let entry = email_credential_entry(id)?;
    let secret = entry
        .get_password()
        .map_err(|_| "邮箱凭据尚未保存".to_string())?;
    if account.provider == "gmail" {
        let mut credential: email::GmailCredential = serde_json::from_str(&secret)
            .map_err(|_| "Gmail 授权格式无效，请重新连接".to_string())?;
        let client = shared_http_client(30);
        if email::refresh_gmail_token(
            &client,
            &google_oauth_client_id()?,
            google_oauth_client_secret(),
            &mut credential,
            now_unix(),
        )
        .await?
        {
            entry
                .set_password(
                    &serde_json::to_string(&credential)
                        .map_err(|_| "无法保存 Gmail 授权".to_string())?,
                )
                .map_err(|_| "无法更新 Gmail 授权".to_string())?;
        }
        email::check_gmail(&client, &credential, None).await?;
    } else {
        tokio::time::timeout(
            Duration::from_secs(45),
            email::check_imap(&account, &secret, None),
        )
        .await
        .map_err(|_| "IMAP 连接测试超时".to_string())??;
    }
    Ok("账户连接与收件权限验证成功；未发送邮件。".to_string())
}

#[tauri::command]
fn get_email_draft(
    id: String,
    tasks: State<'_, EmailTasks>,
) -> Result<email::EmailDraftReview, String> {
    tasks.0.lock().map_err(|error| error.to_string())?.get(&id)
}

#[tauri::command]
fn update_email_draft(
    id: String,
    account_id: i64,
    recipients: Vec<String>,
    subject: String,
    body: String,
    tasks: State<'_, EmailTasks>,
) -> Result<email::EmailDraftReview, String> {
    let mut store = tasks.0.lock().map_err(|error| error.to_string())?;
    if store.get(&id)?.account_id != account_id {
        return Err("审核期间不能替换发件账户，请重新创建邮件".to_string());
    }
    store.update_content(&id, recipients, subject, body)
}

#[tauri::command]
fn cancel_email_draft(id: String, tasks: State<'_, EmailTasks>) -> Result<(), String> {
    tasks
        .0
        .lock()
        .map_err(|error| error.to_string())?
        .cancel(&id)
}

#[tauri::command]
async fn send_email_draft(
    id: String,
    revision: u64,
    app: AppHandle,
    database: State<'_, Database>,
    tasks: State<'_, EmailTasks>,
) -> Result<email::EmailDraftReview, String> {
    let pending = tasks
        .0
        .lock()
        .map_err(|error| error.to_string())?
        .get(&id)?;
    let account = {
        let connection = database.0.lock().map_err(|error| error.to_string())?;
        storage::load_email_account(&connection, pending.account_id)
            .map_err(|_| "发件账户不存在，请重新创建邮件".to_string())?
    };
    if !account.enabled {
        return Err("发件账户已停用，请先在邮件设置中启用".to_string());
    }
    let credential_entry = email_credential_entry(account.id)?;
    let credential = credential_entry
        .get_password()
        .map_err(|_| "邮箱凭据尚未保存，请先连接账户".to_string())?;
    let mut gmail_credential = if account.provider == "gmail" {
        let mut parsed: email::GmailCredential = serde_json::from_str(&credential)
            .map_err(|_| "Gmail 授权格式无效，请断开后重新连接".to_string())?;
        if email::refresh_gmail_token(
            &shared_http_client(30),
            &google_oauth_client_id()?,
            google_oauth_client_secret(),
            &mut parsed,
            now_unix(),
        )
        .await?
        {
            credential_entry
                .set_password(
                    &serde_json::to_string(&parsed)
                        .map_err(|_| "无法保存更新后的 Gmail 授权".to_string())?,
                )
                .map_err(|_| "无法更新 Windows 凭据管理器中的 Gmail 授权".to_string())?;
        }
        Some(parsed)
    } else {
        None
    };
    let job = tasks
        .0
        .lock()
        .map_err(|error| error.to_string())?
        .begin_send(&id, revision)?;
    let run_id = {
        let connection = database.0.lock().map_err(|error| error.to_string())?;
        storage::record_execution_started(
            &connection,
            "send_email",
            &format!(
                "{} · {} 位收件人",
                account.provider,
                job.review.recipients.len()
            ),
            now_unix(),
        )
        .map_err(|error| error.to_string())?
    };
    let outcome = if let Some(gmail) = gmail_credential.take() {
        email::send_gmail(
            &shared_http_client(30),
            &account,
            &gmail,
            &job.review,
            &job.attachments,
        )
        .await
    } else {
        let send_account = account.clone();
        let send_review = job.review.clone();
        let send_attachments = job.attachments.clone();
        tauri::async_runtime::spawn_blocking(move || {
            email::send_smtp(&send_account, &credential, &send_review, &send_attachments)
        })
        .await
        .unwrap_or_else(|_| {
            email::SendOutcome::Unknown("发送线程意外中断；不会自动重试".to_string())
        })
    };
    let (status, audit_status, error_code, status_message) = match outcome {
        email::SendOutcome::Sent => (
            email::EmailTaskStatus::Sent,
            "completed",
            None,
            Some("邮箱服务已确认接收这封邮件。".to_string()),
        ),
        email::SendOutcome::Failed(message) => (
            email::EmailTaskStatus::Failed,
            "failed",
            Some("MAIL_SEND_FAILED"),
            Some(message),
        ),
        email::SendOutcome::Unknown(message) => (
            email::EmailTaskStatus::Unknown,
            "failed",
            Some("MAIL_STATUS_UNKNOWN"),
            Some(message),
        ),
    };
    {
        let mut store = tasks.0.lock().map_err(|error| error.to_string())?;
        store.finish(&id, status, status_message)?;
    }
    {
        let connection = database.0.lock().map_err(|error| error.to_string())?;
        let _ = storage::finish_execution_run(
            &connection,
            run_id,
            audit_status,
            error_code,
            now_unix(),
        );
    }
    let finished = tasks
        .0
        .lock()
        .map_err(|error| error.to_string())?
        .get(&id)?;
    let _ = app.emit_to("panel", "email-send-result", &finished);
    let notification = match finished.status {
        email::EmailTaskStatus::Sent => "邮件已发送，邮箱服务已经确认接收。",
        email::EmailTaskStatus::Failed => "邮件发送失败，审核卡里已经显示具体原因。",
        email::EmailTaskStatus::Unknown => {
            "邮件发送结果暂时无法确认；为避免重复发送，Cyral 不会自动重试。"
        }
        _ => "邮件状态已更新。",
    };
    let _ = app.emit_to("pet", "assistant-message", notification);
    Ok(finished)
}

async fn check_gmail_inbox_status(database: &Database) -> Result<InboxStatus, String> {
    let mut accounts = {
        let connection = database.0.lock().map_err(|error| error.to_string())?;
        storage::load_email_accounts(&connection).map_err(|error| error.to_string())?
    }
    .into_iter()
    .filter(|account| {
        account.enabled
            && account.inbox_enabled
            && account.provider == "gmail"
            && email_account_has_credential(account)
    })
    .collect::<Vec<_>>();
    accounts.sort_by_key(|account| !account.is_default);
    let account = accounts
        .into_iter()
        .next()
        .ok_or("没有已连接且开启新邮件提醒的 Gmail 账户，请先在邮箱设置中完成连接")?;
    let entry = email_credential_entry(account.id)?;
    let secret = entry
        .get_password()
        .map_err(|_| "Gmail 凭据尚未保存，请重新连接账户".to_string())?;
    let mut credential: email::GmailCredential =
        serde_json::from_str(&secret).map_err(|_| "Gmail 授权格式无效，请重新连接".to_string())?;
    let client = shared_http_client(30);
    if email::refresh_gmail_token(
        &client,
        &google_oauth_client_id()?,
        google_oauth_client_secret(),
        &mut credential,
        now_unix(),
    )
    .await?
    {
        entry
            .set_password(
                &serde_json::to_string(&credential)
                    .map_err(|_| "无法保存 Gmail 授权".to_string())?,
            )
            .map_err(|error| format!("无法更新 Gmail 授权：{error}"))?;
    }
    let previous_cursor = {
        let connection = database.0.lock().map_err(|error| error.to_string())?;
        storage::load_mailbox_cursor(&connection, account.id)
            .map_err(|error| error.to_string())?
            .filter(|(kind, _)| kind == "gmail_history")
            .map(|(_, value)| value)
    };
    let delta = email::check_gmail(&client, &credential, previous_cursor.as_deref()).await?;
    let baseline_established_now = previous_cursor.is_none();
    let checked_at = now_unix();
    let new_since_last_check = {
        let salt = mail_hash_salt()?;
        let connection = database.0.lock().map_err(|error| error.to_string())?;
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| error.to_string())?;
        let mut inserted = 0_u64;
        for message in delta.messages {
            let message_hash =
                hash_mail_message_id(&salt, account.id, &message.provider_message_id);
            if storage::record_mail_notification_once(
                &transaction,
                account.id,
                &message_hash,
                checked_at,
            )
            .map_err(|error| error.to_string())?
            {
                inserted = inserted.saturating_add(1);
            }
        }
        storage::save_mailbox_cursor(
            &transaction,
            account.id,
            "gmail_history",
            &delta.cursor,
            checked_at,
        )
        .map_err(|error| error.to_string())?;
        storage::delete_expired_mail_receipts(&transaction, checked_at)
            .map_err(|error| error.to_string())?;
        transaction.commit().map_err(|error| error.to_string())?;
        inserted
    };
    let new_today_observed_by_cyral = {
        let connection = database.0.lock().map_err(|error| error.to_string())?;
        storage::count_mail_notifications_since(&connection, account.id, local_day_start_unix())
            .map_err(|error| error.to_string())?
    };
    let inbox_unread_count = email::get_gmail_inbox_unread_count(&client, &credential).await?;
    Ok(InboxStatus {
        account_address: account.account_address,
        inbox_unread_count,
        new_since_last_check,
        new_today_observed_by_cyral,
        baseline_established_now,
        checked_at,
        message_content_read: false,
    })
}

async fn recent_gmail_metadata_for_today(
    database: &Database,
) -> Result<Vec<email::InboxMessageMetadata>, String> {
    let status = check_gmail_inbox_status(database).await?;
    let account = {
        let connection = database.0.lock().map_err(|error| error.to_string())?;
        storage::load_email_accounts(&connection)
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|account| {
                account.enabled
                    && account.provider == "gmail"
                    && account
                        .account_address
                        .eq_ignore_ascii_case(&status.account_address)
            })
            .ok_or("已连接的 Gmail 账户不存在")?
    };
    let secret = email_credential_entry(account.id)?
        .get_password()
        .map_err(|_| "Gmail 凭据尚未保存，请重新连接账户".to_string())?;
    let credential: email::GmailCredential =
        serde_json::from_str(&secret).map_err(|_| "Gmail 授权格式无效，请重新连接".to_string())?;
    email::get_recent_gmail_metadata(
        &shared_http_client(30),
        &credential,
        local_day_start_unix(),
        5,
    )
    .await
}

/// 启动问候：确定性模板（日期/时间/休眠时长/可选天气），不依赖模型调用。
async fn build_startup_greeting(app: &AppHandle) -> String {
    use chrono::Timelike;
    let now = chrono::Local::now();
    let period = match now.hour() {
        5..=11 => "早上好",
        12..=17 => "下午好",
        _ => "晚上好",
    };
    let weekdays = [
        "星期一",
        "星期二",
        "星期三",
        "星期四",
        "星期五",
        "星期六",
        "星期日",
    ];
    let weekday = weekdays[now.weekday().num_days_from_monday() as usize];
    let mut parts = vec![format!(
        "{period}，今天是 {}年{}月{}日，{weekday}，现在是 {}。",
        now.year(),
        now.month(),
        now.day(),
        now.format("%H:%M")
    )];
    if app
        .try_state::<RuntimeLifecycle>()
        .is_some_and(|lifecycle| lifecycle.recovered_from_unclean_exit)
    {
        parts.push("检测到上次未正常退出，本地数据已通过检查，窗口状态也已恢复。".to_string());
    }
    if let Some(sleep_text) = startup_sleep_text(app) {
        parts.push(sleep_text);
    }
    if let Some(weather_text) = startup_weather_text(app).await {
        parts.push(weather_text);
    }
    parts.join(" ")
}

async fn build_startup_status(app: &AppHandle) -> String {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let todo_count = app
        .try_state::<Database>()
        .and_then(|database| {
            let connection = database.0.lock().ok()?;
            load_todos(&connection).ok().map(|todos| {
                todos
                    .iter()
                    .filter(|todo| {
                        todo.status == "active" && todo.deadline_local.starts_with(&today)
                    })
                    .count()
            })
        })
        .unwrap_or(0);
    let todo_text = if todo_count == 0 {
        "今天没有待办。".to_string()
    } else {
        format!("今天还有 {todo_count} 项待办。")
    };
    let Some(database) = app.try_state::<Database>() else {
        return todo_text;
    };
    match check_gmail_inbox_status(&database).await {
        Ok(status) if status.baseline_established_now => format!(
            "邮箱检查基线已经建立，目前有 {} 封未读邮件；{todo_text}",
            status.inbox_unread_count
        ),
        Ok(status) => format!(
            "今天新收到 {} 封邮件，目前有 {} 封未读；{todo_text}",
            status.new_today_observed_by_cyral, status.inbox_unread_count
        ),
        Err(_) => todo_text,
    }
}

const MAX_DROPPED_FILES: usize = 10;
const MAX_ATTACHMENT_BYTES: u64 = 20 * 1024 * 1024;
const MAX_TEXT_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DroppedFileView {
    candidate_id: String,
    name: String,
    size_bytes: u64,
    text_readable: bool,
}

fn text_file_is_readable(path: &std::path::Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "txt"
            | "md"
            | "csv"
            | "json"
            | "rs"
            | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "py"
            | "java"
            | "c"
            | "h"
            | "cpp"
            | "cs"
            | "go"
            | "html"
            | "css"
            | "toml"
            | "yaml"
            | "yml"
            | "xml"
            | "sql"
            | "sh"
            | "ps1"
    )
}

#[tauri::command]
fn register_dropped_files(
    paths: Vec<String>,
    candidates: State<'_, ExecutionCandidates>,
) -> Result<Vec<DroppedFileView>, String> {
    if paths.is_empty() || paths.len() > MAX_DROPPED_FILES {
        return Err("一次最多拖入 10 个文件".to_string());
    }
    let mut vault = candidates.0.lock().map_err(|error| error.to_string())?;
    let mut registered: Vec<DroppedFileView> = Vec::with_capacity(paths.len());
    let mut total_bytes = 0_u64;
    for raw_path in paths {
        let (candidate_id, file) =
            match vault.register_dropped_file(std::path::Path::new(&raw_path)) {
                Ok(value) => value,
                Err(error) => {
                    for item in &registered {
                        vault.discard_file(&item.candidate_id);
                    }
                    return Err(error);
                }
            };
        let size_bytes = match fs::metadata(&file.path) {
            Ok(metadata) => metadata.len(),
            Err(_) => {
                vault.discard_file(&candidate_id);
                for item in &registered {
                    vault.discard_file(&item.candidate_id);
                }
                return Err("无法读取拖入文件的大小".to_string());
            }
        };
        total_bytes = total_bytes.saturating_add(size_bytes);
        if total_bytes > MAX_ATTACHMENT_BYTES {
            vault.discard_file(&candidate_id);
            for item in &registered {
                vault.discard_file(&item.candidate_id);
            }
            return Err("附件总大小不能超过 20 MiB".to_string());
        }
        registered.push(DroppedFileView {
            candidate_id,
            name: file.display_name,
            size_bytes,
            text_readable: text_file_is_readable(&file.path),
        });
    }
    Ok(registered)
}

#[tauri::command]
fn discard_dropped_file(
    candidate_id: String,
    candidates: State<'_, ExecutionCandidates>,
) -> Result<(), String> {
    candidates
        .0
        .lock()
        .map_err(|error| error.to_string())?
        .discard_file(&candidate_id);
    Ok(())
}

fn dropped_file_context(
    candidate_ids: &[String],
    candidates: &Mutex<windows_actions::CandidateVault>,
) -> Result<Option<Message>, String> {
    if candidate_ids.is_empty() {
        return Ok(None);
    }
    if candidate_ids.len() > MAX_DROPPED_FILES {
        return Err("一次最多处理 10 个拖入文件".to_string());
    }
    let vault = candidates.lock().map_err(|error| error.to_string())?;
    let mut total_bytes = 0_u64;
    let mut items = Vec::with_capacity(candidate_ids.len());
    for candidate_id in candidate_ids {
        let file = vault.snapshot_authorized_file(candidate_id)?;
        let metadata =
            fs::metadata(&file.path).map_err(|_| "拖入文件已失效，请重新拖入".to_string())?;
        total_bytes = total_bytes.saturating_add(metadata.len());
        if total_bytes > MAX_ATTACHMENT_BYTES {
            return Err("附件总大小不能超过 20 MiB".to_string());
        }
        let mut item = format!(
            "文件名：{}；大小：{} 字节；attachmentCandidateId：{}",
            file.display_name,
            metadata.len(),
            candidate_id
        );
        if text_file_is_readable(&file.path) && metadata.len() <= MAX_TEXT_FILE_BYTES {
            item.push_str("；可在用户明确要求读取时调用 read_dropped_text_file");
        } else {
            item.push_str("；此文件不读取内容，只可作为邮件附件");
        }
        items.push(item);
    }
    Ok(Some(Message {
        role: "system".to_string(),
        content: format!(
            "用户已在本轮明确拖入以下文件。拖入只授权本轮使用且不等于授权读取；仅当用户明确要求阅读、总结或分析文件内容时，才可调用 read_dropped_text_file。若用户要求发送邮件，必须把对应 attachmentCandidateId 原样传给 prepare_email_review。不得修改文件内容，不得声称已经发送；若用户没有提出读取或发送要求，只确认文件已就绪。\n{}",
            items.join("\n---\n")
        ),
        created_at: 0,
        source: "runtime".to_string(),
    }))
}

fn discard_file_candidates(
    candidate_ids: &[String],
    candidates: &Mutex<windows_actions::CandidateVault>,
) {
    if let Ok(mut vault) = candidates.lock() {
        for candidate_id in candidate_ids {
            vault.discard_file(candidate_id);
        }
    }
}

fn startup_sleep_text(app: &AppHandle) -> Option<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let last_active_at = {
        let database = app.try_state::<Database>()?;
        let connection = database.0.lock().ok()?;
        load_pet_state(&connection).ok()?.last_active_at
    };
    if last_active_at <= 0 || last_active_at >= now {
        return None;
    }
    let elapsed = now - last_active_at;
    if elapsed < 86_400 {
        let hours = std::cmp::max(1, ((elapsed as f64) / 3600.0).round() as i64);
        Some(format!("你休息了大约 {hours} 小时。"))
    } else {
        let days = std::cmp::max(1, ((elapsed as f64) / 86_400.0).round() as i64);
        Some(format!("你有 {days} 天没有打开 Cyral 了。"))
    }
}

/// 问候天气：位置授权优先，失败回退默认城市，两者都不可用则省略。
async fn startup_weather_text(app: &AppHandle) -> Option<String> {
    let (auto_location, city) = {
        let database = app.try_state::<Database>()?;
        let connection = database.0.lock().ok()?;
        let privacy = load_privacy_config(&connection).ok()?;
        let settings = crate::storage::load_app_settings(&connection).ok()?;
        (privacy.auto_location, settings.city)
    };
    let location_intent = WeatherIntent {
        city: None,
        time: WeatherTime::Current,
        metric: WeatherMetric::Summary,
    };
    if auto_location {
        if let Ok(location) = current_location().await {
            if let Ok(summary) = fetch_weather_at_location(&location, &location_intent).await {
                return Some(format!("当前天气：{summary}。"));
            }
        }
    }
    let city = city.trim();
    if !city.is_empty() {
        let city_intent = WeatherIntent {
            city: Some(city.to_string()),
            time: WeatherTime::Current,
            metric: WeatherMetric::Summary,
        };
        if let Ok(summary) = fetch_weather(&city_intent).await {
            return Some(format!("{city}当前天气：{summary}。"));
        }
    }
    None
}

#[tauri::command]
#[allow(clippy::too_many_arguments)] // Tauri 注入的独立 State 参数，不是业务参数对象。
async fn send_message(
    message: String,
    file_candidate_ids: Option<Vec<String>>,
    app: AppHandle,
    database: State<'_, Database>,
    companion: State<'_, CompanionSession>,
    motion: State<'_, PetMotionState>,
    email_tasks: State<'_, EmailTasks>,
    execution_candidates: State<'_, ExecutionCandidates>,
) -> Result<String, String> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return Err("message cannot be empty".to_string());
    }
    mark_pet_interaction(&motion);

    // 处理斜杠命令
    if trimmed == "/陪伴模式" {
        let connection = database.0.lock().map_err(|error| error.to_string())?;
        let privacy = load_privacy_config(&connection)
            .map_err(|error| format_error_chain("读取隐私配置失败", &error))?;

        if !privacy.screen_observation {
            let response = "我需要你的授权才能观察屏幕。请在右侧面板的“隐私设置”中开启“允许观察屏幕内容”，然后再输入 /陪伴模式 开启哦 💙";
            app.emit_to("pet", "assistant-message", response)
                .map_err(|error| error.to_string())?;
            return Ok(response.to_string());
        }

        // 已开启时无需重复执行。
        if companion
            .0
            .lock()
            .map_err(|error| error.to_string())?
            .active
        {
            let response = "陪伴模式已开启，无需重复打开 ✨ 想停止的话随时双击我或输入 /停止陪伴";
            app.emit_to("pet", "assistant-message", response)
                .map_err(|error| error.to_string())?;
            return Ok(response.to_string());
        }

        let mut state = companion.0.lock().map_err(|error| error.to_string())?;
        activate_companion(&mut state);
        let response =
            "陪伴模式已开启 ✨ 我会在合适的时候陪你聊聊天～想停止的话随时双击我或输入 /停止陪伴";
        ensure_pet_visible(&app);
        app.emit_to("pet", "assistant-message", response)
            .map_err(|error| error.to_string())?;
        crate::activity::spawn_companion_start_feedback(&app);
        let _ = app.emit_to("panel", "mode-changed", ());
        let _ = notify_mode_change(&app, "陪伴模式");
        return Ok(response.to_string());
    }

    if trimmed == "/停止陪伴" {
        let mut state = companion.0.lock().map_err(|error| error.to_string())?;
        halt_companion(&mut state);
        drop(state);
        let response = "好的，已停止陪伴模式。需要我的时候随时叫我 💙";
        app.emit_to("pet", "assistant-message", response)
            .map_err(|error| error.to_string())?;
        let _ = app.emit_to("panel", "mode-changed", ());
        let _ = notify_mode_change(&app, "停止陪伴");
        return Ok(response.to_string());
    }

    let (mut provider, mut messages, privacy) = {
        let connection = database.0.lock().map_err(|error| error.to_string())?;
        let provider = load_provider_settings(&connection)
            .optional()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "请先在功能区保存模型配置和 API Key".to_string())?;
        let mut messages = model_conversation_context(
            load_recent_messages(&connection, 24).map_err(|error| error.to_string())?,
        );
        let memories = load_memory_atoms(&connection).map_err(|error| error.to_string())?;
        if let Some(context) = memory_context(&memories) {
            messages.insert(0, context);
        }
        let privacy = load_privacy_config(&connection)
            .map_err(|error| format_error_chain("读取隐私配置失败", &error))?;
        (provider, messages, privacy)
    };
    // 注入运行时状态：让模型感知陪伴模式与隐私授权，从而对"能否看到屏幕"等基于事实回应。
    let movement_allowed = motion
        .0
        .lock()
        .map(|state| state.movement_allowed)
        .unwrap_or(true);
    if let Some(runtime) = build_runtime_context(&companion, &privacy, movement_allowed) {
        messages.insert(0, runtime);
    }
    let file_candidate_ids = file_candidate_ids.unwrap_or_default();
    if let Some(context) = dropped_file_context(&file_candidate_ids, &execution_candidates.0)? {
        messages.insert(0, context);
    }
    let api_key = match keyring::Entry::new("app.cyral.desktop", "active-provider-api-key")
        .map_err(|error| format!("无法访问安全凭据库：{error}"))?
        .get_password()
    {
        Ok(key) => key,
        Err(_) if is_loopback_model_url(&provider.base_url) => String::new(),
        Err(_) => return Err("没有找到已保存的 API Key，请重新保存模型配置".to_string()),
    };
    messages.push(Message {
        role: "user".to_string(),
        content: trimmed.to_string(),
        created_at: 0,
        source: "chat".to_string(),
    });
    if let Some(context) = file_recall_context(&messages) {
        messages.insert(0, context);
    }
    if let Some(context) = email_clarification_context(&messages) {
        messages.insert(0, context);
    }
    // 预算硬限：达到 100% 拦截云端请求并明确告知（本地待办/动画不受影响）。
    if cloud_budget_blocked(&database) {
        let connection = database.0.lock().map_err(|error| error.to_string())?;
        let (used, budget) = monthly_budget_status(&connection, now_unix());
        let response = budget_warning(used, budget, 1.0)
            .unwrap_or_else(|| "本月 API 预算已用完，云端请求已暂停。".to_string());
        if let Ok(mut ratio) = LAST_WARNED_BUDGET_RATIO.lock() {
            *ratio = 1.0;
        }
        app.emit_to("pet", "assistant-message", &response)
            .map_err(|error| error.to_string())?;
        return Ok(response);
    }
    let tool_runtime = ToolRuntime {
        app: &app,
        companion: &companion,
        privacy: &privacy,
        database: &database,
        pet_motion: &motion,
        execution_origin: execution::ExecutionOrigin::UserChat,
        execution_candidates: &execution_candidates.0,
        email_tasks: &email_tasks,
        user_message: trimmed,
    };
    let reply = match run_agent_loop(&provider, &api_key, &messages, &tool_runtime).await {
        Ok(reply) => reply,
        Err(initial_error) if is_endpoint_compatibility_error(&initial_error) => {
            let candidates = build_provider_candidates(
                &provider.provider_name,
                &provider.base_url,
                "auto",
                &provider.model,
                provider.monthly_budget_usd,
            );
            let mut recovered = None;
            let mut last_error = initial_error;
            for candidate in candidates
                .into_iter()
                .filter(|candidate| candidate != &provider)
            {
                match run_agent_loop(&candidate, &api_key, &messages, &tool_runtime).await {
                    Ok(reply) => {
                        recovered = Some((candidate, reply));
                        break;
                    }
                    Err(error) => last_error = error,
                }
            }
            match recovered {
                Some((matched_provider, reply)) => {
                    provider = matched_provider;
                    reply
                }
                None => {
                    let _ = app.emit_to("pet", "assistant-stream-error", &last_error);
                    return Err(last_error);
                }
            }
        }
        Err(error) => {
            let _ = app.emit_to("pet", "assistant-stream-error", &error);
            return Err(error);
        }
    };
    discard_file_candidates(&file_candidate_ids, &execution_candidates.0);
    let (extract_due, extraction_batch) = {
        let connection = database.0.lock().map_err(|error| error.to_string())?;
        save_provider_settings(&connection, &provider).map_err(|error| error.to_string())?;
        save_message(&connection, "user", trimmed).map_err(|error| error.to_string())?;
        save_message(&connection, "assistant", &reply).map_err(|error| error.to_string())?;
        let user_turn_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE role = 'user'",
                [],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        let due = should_extract_memories(user_turn_count);
        let batch = if due {
            load_recent_messages(&connection, 10).map_err(|error| error.to_string())?
        } else {
            Vec::new()
        };
        (due, batch)
    };
    app.emit_to("pet", "assistant-message", &reply)
        .map_err(|error| error.to_string())?;
    app.emit_to("pet", "assistant-stream-end", &reply)
        .map_err(|error| error.to_string())?;
    // 80% 预算提醒：跨阈值只提醒一次。
    {
        let connection = database.0.lock().map_err(|error| error.to_string())?;
        flush_usage_records(&connection);
        let (used, budget) = monthly_budget_status(&connection, now_unix());
        let last = LAST_WARNED_BUDGET_RATIO
            .lock()
            .map(|ratio| *ratio)
            .unwrap_or(0.0);
        let ratio = if budget > 0.0 { used / budget } else { 0.0 };
        if let Some(warning) = budget_warning(used, budget, last) {
            if ratio < 1.0 {
                if let Ok(mut guard) = LAST_WARNED_BUDGET_RATIO.lock() {
                    *guard = ratio;
                }
                let _ = app.emit_to("pet", "assistant-message", &warning);
            }
        }
    }
    if extract_due {
        let extraction_app = app.clone();
        let extraction_provider = provider.clone();
        let extraction_key = api_key.clone();
        tauri::async_runtime::spawn(async move {
            let Ok(candidates) = extract_memories_with_model(
                &extraction_provider,
                &extraction_key,
                &extraction_batch,
            )
            .await
            else {
                return;
            };
            let Some(database) = extraction_app.try_state::<Database>() else {
                return;
            };
            let Ok(connection) = database.0.lock() else {
                return;
            };
            for memory in candidates {
                let _ = save_memory_atom(
                    &connection,
                    &memory.kind,
                    &memory.content,
                    &memory.source_excerpt,
                    memory.confidence,
                );
            }
        });
    }
    Ok(reply)
}

#[tauri::command]
fn begin_chat_request(app: AppHandle) -> Result<(), String> {
    app.emit_to("pet", "assistant-stream-start", ())
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn fail_chat_request(app: AppHandle) -> Result<(), String> {
    app.emit_to("pet", "assistant-stream-error", ())
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn get_recent_messages(
    limit: Option<usize>,
    database: State<'_, Database>,
) -> Result<Vec<Message>, String> {
    let connection = database.0.lock().map_err(|error| error.to_string())?;
    load_recent_messages(&connection, limit.unwrap_or(50).min(200))
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn get_memory_atoms(database: State<'_, Database>) -> Result<Vec<MemoryAtom>, String> {
    let connection = database.0.lock().map_err(|error| error.to_string())?;
    load_memory_atoms(&connection).map_err(|error| error.to_string())
}

#[tauri::command]
fn get_cyral_activity_events(
    database: State<'_, Database>,
) -> Result<Vec<storage::CyralActivityEvent>, String> {
    let connection = database.0.lock().map_err(|error| error.to_string())?;
    storage::load_cyral_activity_events(&connection, 200).map_err(|error| error.to_string())
}

#[tauri::command]
fn get_activity_themes(
    database: State<'_, Database>,
) -> Result<Vec<storage::ActivityTheme>, String> {
    let connection = database.0.lock().map_err(|error| error.to_string())?;
    storage::load_activity_themes(&connection, 30).map_err(|error| error.to_string())
}

#[tauri::command]
fn mute_activity_theme(identity_key: String, database: State<'_, Database>) -> Result<(), String> {
    let until = now_unix().saturating_add(7 * 86_400);
    let connection = database.0.lock().map_err(|error| error.to_string())?;
    storage::mute_activity_theme(&connection, &identity_key, until)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn delete_activity_theme(
    identity_key: String,
    database: State<'_, Database>,
) -> Result<(), String> {
    let connection = database.0.lock().map_err(|error| error.to_string())?;
    storage::delete_activity_theme(&connection, &identity_key).map_err(|error| error.to_string())
}

/// 本月用量摘要：自然月（本地时区）token 与估算费用。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MonthlyUsage {
    month: String,
    used_usd: f64,
    budget_usd: f64,
    total_tokens: i64,
}

/// 折线图数据点：某本地日期的 token 消耗。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsagePoint {
    day: String,
    total_tokens: i64,
}

#[tauri::command]
fn get_monthly_usage(database: State<'_, Database>) -> Result<MonthlyUsage, String> {
    let connection = database.0.lock().map_err(|error| error.to_string())?;
    flush_usage_records(&connection);
    let now = now_unix();
    let month = local_month_key(now);
    let (total_tokens, used_usd) =
        crate::storage::monthly_usage(&connection, &month).map_err(|error| error.to_string())?;
    let budget_usd = load_monthly_budget(&connection);
    Ok(MonthlyUsage {
        month,
        used_usd,
        budget_usd,
        total_tokens,
    })
}

#[tauri::command]
fn get_usage_history(
    database: State<'_, Database>,
    days: Option<i64>,
) -> Result<Vec<UsagePoint>, String> {
    let connection = database.0.lock().map_err(|error| error.to_string())?;
    flush_usage_records(&connection);
    let days = days.unwrap_or(30).clamp(1, 90);
    let rows = crate::storage::daily_token_usage(&connection, days, now_unix())
        .map_err(|error| error.to_string())?;
    Ok(rows
        .into_iter()
        .map(|(day, total_tokens)| UsagePoint { day, total_tokens })
        .collect())
}

#[tauri::command]
fn update_memory(id: i64, content: String, database: State<'_, Database>) -> Result<(), String> {
    if content.trim().is_empty() {
        return Err("记忆内容不能为空".to_string());
    }
    let connection = database.0.lock().map_err(|error| error.to_string())?;
    update_memory_atom(&connection, id, &content).map_err(|error| error.to_string())
}

#[tauri::command]
fn delete_memory(id: i64, database: State<'_, Database>) -> Result<(), String> {
    let connection = database.0.lock().map_err(|error| error.to_string())?;
    delete_memory_atom(&connection, id).map_err(|error| error.to_string())
}

#[tauri::command]
fn get_todos(database: State<'_, Database>) -> Result<Vec<TodoItem>, String> {
    let connection = database.0.lock().map_err(|error| error.to_string())?;
    load_todos(&connection).map_err(|error| error.to_string())
}

/// 功能面板直接新建待办：不经过草稿确认，立即进入 active。
#[tauri::command]
fn create_todo(
    title: String,
    deadline_local: String,
    database: State<'_, Database>,
) -> Result<(), String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("待办事项不能为空".to_string());
    }
    let Some(deadline) = deadline_timestamp(&deadline_local) else {
        return Err("截止时间格式无效，请使用 YYYY-MM-DDTHH:mm".to_string());
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    if deadline <= now {
        return Err("截止时间必须晚于当前时间".to_string());
    }
    let connection = database.0.lock().map_err(|error| error.to_string())?;
    connection
        .execute(
            "INSERT INTO todos (title, deadline_local, remind_before_minutes, source_text, status, created_at)
             VALUES (?1, ?2, 0, ?3, 'active', ?4)",
            params![
                title,
                deadline_local.trim(),
                format!("面板创建：{title}"),
                now
            ],
        )
        .map_err(|error| error.to_string())?;
    let id = connection.last_insert_rowid();
    initialize_reminder_points(&connection, id, &deadline_local, now)
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command]
fn confirm_todo_draft(
    id: i64,
    title: String,
    deadline_local: String,
    remind_before_minutes: i64,
    database: State<'_, Database>,
) -> Result<(), String> {
    if title.trim().is_empty() || deadline_local.trim().is_empty() {
        return Err("事项和截止时间必须填写后才能创建".to_string());
    }
    let connection = database.0.lock().map_err(|error| error.to_string())?;
    confirm_todo(
        &connection,
        id,
        &title,
        &deadline_local,
        remind_before_minutes,
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
fn discard_todo_draft(id: i64, database: State<'_, Database>) -> Result<(), String> {
    let connection = database.0.lock().map_err(|error| error.to_string())?;
    connection
        .execute(
            "DELETE FROM todos WHERE id = ?1 AND status = 'draft'",
            params![id],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command]
fn complete_todo_reminder(id: i64, database: State<'_, Database>) -> Result<(), String> {
    let connection = database.0.lock().map_err(|error| error.to_string())?;
    complete_todo(&connection, id).map_err(|error| error.to_string())
}

#[tauri::command]
fn snooze_todo_reminder(id: i64, database: State<'_, Database>) -> Result<(), String> {
    let until = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
        + 10 * 60;
    let connection = database.0.lock().map_err(|error| error.to_string())?;
    snooze_todo(&connection, id, until).map_err(|error| error.to_string())
}

#[tauri::command]
async fn save_model_provider(
    settings: ModelProviderInput,
    database: State<'_, Database>,
) -> Result<ModelProviderSettings, String> {
    if settings.base_url.trim().is_empty() || settings.model.trim().is_empty() {
        return Err("Base URL 与模型名称为必填项".to_string());
    }
    if !matches!(
        settings.protocol.trim(),
        "auto" | "responses" | "chat_completions"
    ) {
        return Err("unsupported provider protocol".to_string());
    }
    if !(0.0 < settings.monthly_budget_usd && settings.monthly_budget_usd <= 10.0) {
        return Err("monthly budget must be between 0 and 10 USD".to_string());
    }

    let key_entry = keyring::Entry::new("app.cyral.desktop", "active-provider-api-key")
        .map_err(|error| format!("无法访问安全凭据库：{error}"))?;
    let submitted_api_key = settings.api_key.trim();
    let effective_api_key = if submitted_api_key.is_empty() {
        if is_loopback_model_url(&settings.base_url) {
            String::new()
        } else {
            key_entry
                .get_password()
                .map_err(|_| "没有找到已保存的 API Key，请填写后再保存模型配置".to_string())?
        }
    } else {
        submitted_api_key.to_string()
    };

    let candidates = build_provider_candidates(
        &settings.provider_name,
        &settings.base_url,
        &settings.protocol,
        &settings.model,
        settings.monthly_budget_usd,
    );
    let client = shared_http_client(20);
    let mut retryable_errors = Vec::new();
    let mut matched = None;
    for candidate in candidates {
        match probe_provider(&client, &candidate, &effective_api_key).await {
            Ok(()) => {
                matched = Some(candidate);
                break;
            }
            Err(ProbeFailure::Fatal(error)) => return Err(error),
            Err(ProbeFailure::Retryable(error)) => retryable_errors.push(error),
        }
    }
    let provider = matched.ok_or_else(|| {
        format!(
            "无法自动匹配可用接口：{}",
            retryable_errors.last().cloned().unwrap_or_default()
        )
    })?;

    if !submitted_api_key.is_empty() {
        key_entry
            .set_password(submitted_api_key)
            .map_err(|error| error.to_string())?;
    }
    let connection = database.0.lock().map_err(|error| error.to_string())?;
    save_provider_settings(&connection, &provider).map_err(|error| error.to_string())?;
    Ok(provider)
}

#[tauri::command]
fn get_model_provider(
    database: State<'_, Database>,
) -> Result<Option<ModelProviderSettings>, String> {
    let connection = database.0.lock().map_err(|error| error.to_string())?;
    load_provider_settings(&connection)
        .optional()
        .map_err(|error| error.to_string())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VisionProviderInput {
    base_url: String,
    protocol: String,
    model: String,
    api_key: String,
    enabled: bool,
}

/// 可选视觉分析模型：与对话模型独立。未配置或失败时截图分析降级为纯文本摘要。
#[tauri::command]
fn save_vision_provider(
    settings: VisionProviderInput,
    database: State<'_, Database>,
) -> Result<storage::VisionProvider, String> {
    if settings.enabled {
        if settings.base_url.trim().is_empty() || settings.model.trim().is_empty() {
            return Err("启用视觉分析需要填写 Base URL 与模型名称".to_string());
        }
        if settings.api_key.trim().is_empty() {
            return Err("视觉模型必须填写 API Key".to_string());
        }
        if !matches!(settings.protocol.trim(), "responses" | "chat_completions") {
            return Err("视觉模型协议仅支持 responses 或 chat_completions".to_string());
        }
    }
    let key_entry = keyring::Entry::new("app.cyral.desktop", "vision-provider-api-key")
        .map_err(|error| error.to_string())?;
    if settings.enabled && !settings.api_key.trim().is_empty() {
        key_entry
            .set_password(settings.api_key.trim())
            .map_err(|error| error.to_string())?;
    } else {
        let _ = key_entry.delete_credential();
    }
    let provider = storage::VisionProvider {
        base_url: settings.base_url.trim().to_string(),
        protocol: settings.protocol.trim().to_string(),
        model: settings.model.trim().to_string(),
        enabled: settings.enabled,
    };
    let connection = database.0.lock().map_err(|error| error.to_string())?;
    storage::save_vision_provider(&connection, &provider)
        .map_err(|error| format!("保存视觉模型配置失败：{error}"))?;
    Ok(provider)
}

#[tauri::command]
fn get_vision_provider(
    database: State<'_, Database>,
) -> Result<Option<storage::VisionProvider>, String> {
    let connection = database.0.lock().map_err(|error| error.to_string())?;
    storage::load_vision_provider(&connection)
        .optional()
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn open_panel(app: AppHandle) -> Result<(), String> {
    if let Some(motion) = app.try_state::<PetMotionState>() {
        mark_pet_interaction(&motion);
    }
    let panel = app
        .get_webview_window("panel")
        .ok_or_else(|| "panel window is missing".to_string())?;
    position_panel(&app)?;
    panel.show().map_err(|error| error.to_string())?;
    panel.set_focus().map_err(|error| error.to_string())?;
    Ok(())
}

fn related_panel_position(
    monitor_position: (i32, i32),
    monitor_size: (u32, u32),
    pet_position: (i32, i32),
    pet_size: (u32, u32),
    panel_size: (u32, u32),
) -> (i32, i32) {
    const GAP: i32 = 12;
    const MARGIN: i32 = 16;

    let monitor_right = monitor_position.0 + monitor_size.0 as i32;
    let monitor_bottom = monitor_position.1 + monitor_size.1 as i32;
    let pet_center_x = pet_position.0 + pet_size.0 as i32 / 2;
    let monitor_center_x = monitor_position.0 + monitor_size.0 as i32 / 2;

    let preferred_x = if pet_center_x >= monitor_center_x {
        pet_position.0 - panel_size.0 as i32 - GAP
    } else {
        pet_position.0 + pet_size.0 as i32 + GAP
    };
    let preferred_y = pet_position.1 + pet_size.1 as i32 / 2 - panel_size.1 as i32 / 2;
    let maximum_x = (monitor_right - panel_size.0 as i32 - MARGIN).max(monitor_position.0 + MARGIN);
    let maximum_y =
        (monitor_bottom - panel_size.1 as i32 - MARGIN).max(monitor_position.1 + MARGIN);

    (
        preferred_x.clamp(monitor_position.0 + MARGIN, maximum_x),
        preferred_y.clamp(monitor_position.1 + MARGIN, maximum_y),
    )
}

fn position_panel(app: &AppHandle) -> Result<(), String> {
    let pet = app
        .get_webview_window("pet")
        .ok_or_else(|| "pet window is missing".to_string())?;
    let panel = app
        .get_webview_window("panel")
        .ok_or_else(|| "panel window is missing".to_string())?;
    let monitor = pet
        .current_monitor()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "pet monitor is missing".to_string())?;
    let monitor_position = monitor.position();
    let monitor_size = monitor.size();
    let pet_position = pet.outer_position().map_err(|error| error.to_string())?;
    let pet_size = pet.outer_size().map_err(|error| error.to_string())?;
    let panel_size = panel.outer_size().map_err(|error| error.to_string())?;
    let (x, y) = related_panel_position(
        (monitor_position.x, monitor_position.y),
        (monitor_size.width, monitor_size.height),
        (pet_position.x, pet_position.y),
        (pet_size.width, pet_size.height),
        (panel_size.width, panel_size.height),
    );

    panel
        .set_position(PhysicalPosition::new(x, y))
        .map_err(|error| error.to_string())
}

fn bottom_right_position(
    screen_width: u32,
    screen_height: u32,
    window_width: u32,
    window_height: u32,
    margin: u32,
) -> (i32, i32) {
    (
        screen_width.saturating_sub(window_width + margin) as i32,
        screen_height.saturating_sub(window_height + margin) as i32,
    )
}

/// 保留仍在当前屏幕工作区内的位置；副屏断开或分辨率变化后，改放到右下角。
fn restore_position_in_visible_work_area(
    saved_position: (i32, i32),
    window_size: (u32, u32),
    work_area_position: (i32, i32),
    work_area_size: (u32, u32),
    margin: u32,
) -> (i32, i32) {
    let (work_x, work_y) = work_area_position;
    let (work_width, work_height) = work_area_size;
    let (window_width, window_height) = window_size;
    let saved_right = saved_position.0.saturating_add(window_width as i32);
    let saved_bottom = saved_position.1.saturating_add(window_height as i32);
    let work_right = work_x.saturating_add(work_width as i32);
    let work_bottom = work_y.saturating_add(work_height as i32);

    if saved_right > work_x
        && saved_position.0 < work_right
        && saved_bottom > work_y
        && saved_position.1 < work_bottom
    {
        return constrain_pet_position(
            saved_position,
            window_size,
            work_area_position,
            work_area_size,
        );
    }

    let (x, y) =
        bottom_right_position(work_width, work_height, window_width, window_height, margin);
    (work_x.saturating_add(x), work_y.saturating_add(y))
}

fn constrain_pet_position(
    position: (i32, i32),
    window_size: (u32, u32),
    monitor_position: (i32, i32),
    monitor_size: (u32, u32),
) -> (i32, i32) {
    let maximum_x = monitor_position.0 + monitor_size.0.saturating_sub(window_size.0) as i32;
    let maximum_y = monitor_position.1 + monitor_size.1.saturating_sub(window_size.1) as i32;

    (
        position.0.clamp(monitor_position.0, maximum_x),
        position.1.clamp(monitor_position.1, maximum_y),
    )
}

const TRANSIENT_CLICK_MAX_MS: u128 = 350;
const TRANSIENT_CLICK_POLL_MS: u64 = 20;

fn should_track_outside_click(
    window_label: &str,
    pet_is_focused: bool,
    left_button_pressed: bool,
) -> bool {
    left_button_pressed
        && (window_label == "composer" || (window_label == "panel" && !pet_is_focused))
}

fn should_hide_after_outside_pointer(
    held_ms: u128,
    window_is_focused: bool,
    window_is_visible: bool,
) -> bool {
    held_ms < TRANSIENT_CLICK_MAX_MS && !window_is_focused && window_is_visible
}

fn track_outside_click(window: tauri::Window) {
    tauri::async_runtime::spawn(async move {
        let started = std::time::Instant::now();
        while left_mouse_button_pressed() {
            if started.elapsed().as_millis() >= TRANSIENT_CLICK_MAX_MS {
                // 长按或原生文件拖拽：保持窗口可见，等待用户把内容拖回来。
                return;
            }
            tokio::time::sleep(Duration::from_millis(TRANSIENT_CLICK_POLL_MS)).await;
        }
        let focused = window.is_focused().unwrap_or(false);
        let visible = window.is_visible().unwrap_or(false);
        if should_hide_after_outside_pointer(started.elapsed().as_millis(), focused, visible) {
            let _ = window.hide();
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // 必须保持为首个插件：重复启动只唤回现有 Cyral，不创建第二套后台调度器。
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            rescue_pet_window(app);
            let _ = open_composer(app.clone());
        }))
        .invoke_handler(tauri::generate_handler![
            open_panel,
            get_pet_state,
            set_pet_locked,
            hide_pet,
            open_composer,
            hide_composer,
            begin_chat_request,
            fail_chat_request,
            register_dropped_files,
            discard_dropped_file,
            send_message,
            get_recent_messages,
            get_memory_atoms,
            update_memory,
            delete_memory,
            get_todos,
            create_todo,
            confirm_todo_draft,
            discard_todo_draft,
            complete_todo_reminder,
            snooze_todo_reminder,
            save_model_provider,
            get_model_provider,
            get_companion_state,
            get_mode_state,
            stop_companion_observation,
            set_companion_mode,
            get_privacy_config,
            save_privacy_config,
            get_app_settings,
            save_app_settings,
            get_email_accounts,
            save_email_account,
            delete_email_account,
            connect_gmail_account,
            disconnect_gmail_account,
            test_email_account_connection,
            get_email_draft,
            update_email_draft,
            cancel_email_draft,
            send_email_draft,
            get_monthly_usage,
            get_usage_history,
            get_cyral_activity_events,
            get_activity_themes,
            mute_activity_theme,
            delete_activity_theme,
            save_vision_provider,
            get_vision_provider,
            get_pet_motion,
            mark_pet_frontend_ready,
            set_animation_available,
            pet_pointer_active,
            pet_drag_started,
            restore_from_compact
        ])
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::Moved(_) if window.label() == "pet" => {
                let app = window.app_handle();
                // 自动移动/形态切换期间不持久化、不联动面板（settle 时统一写一次）；
                // 用户拖动期间不逐帧写库（拖动结束统一写一次）。
                let (auto_moving, dragging) = app
                    .try_state::<PetMotionState>()
                    .map(|motion| {
                        motion
                            .0
                            .lock()
                            .ok()
                            .map(|state| {
                                (
                                    state.auto_moving || state.state != MotionState::NormalIdle,
                                    state.dragging,
                                )
                            })
                            .unwrap_or((false, false))
                    })
                    .unwrap_or((false, false));
                if dragging {
                    if !auto_moving {
                        if let Some(panel) = app.get_webview_window("panel") {
                            if panel.is_visible().unwrap_or(false) {
                                let _ = position_panel(app);
                            }
                        }
                    }
                } else if !auto_moving {
                    if let (Ok(position), Some(database)) =
                        (window.outer_position(), app.try_state::<Database>())
                    {
                        if let Ok(connection) = database.0.lock() {
                            let _ = save_pet_position(&connection, position.x, position.y);
                        }
                    }
                    if let Some(panel) = app.get_webview_window("panel") {
                        if panel.is_visible().unwrap_or(false) {
                            let _ = position_panel(app);
                        }
                    }
                }
            }
            tauri::WindowEvent::Focused(false)
                if window.label() == "panel" || window.label() == "composer" =>
            {
                let pet_is_focused = window
                    .app_handle()
                    .get_webview_window("pet")
                    .and_then(|pet| pet.is_focused().ok())
                    .unwrap_or(false);
                if should_track_outside_click(
                    window.label(),
                    pet_is_focused,
                    left_mouse_button_pressed(),
                ) {
                    track_outside_click(window.clone());
                }
            }
            _ => {}
        })
        .setup(|app| {
            let data_directory = app.path().app_data_dir()?;
            fs::create_dir_all(&data_directory)?;
            let connection = open_database_with_recovery(&data_directory.join("cyral.db"))?;
            // 开机会话：记录本次启动时刻，并按 3 天保留策略清理过期活动记录。
            let boot_now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            let marker_path = data_directory.join("runtime-active");
            let recovered_from_unclean_exit = begin_runtime_session(&marker_path, boot_now)?;
            let _ = storage::set_boot_started_at(&connection, boot_now);
            let _ = storage::delete_expired_activity_sessions(
                &connection,
                boot_now,
                storage::ACTIVITY_RETENTION_SECS,
            );
            let _ = storage::delete_expired_execution_data(&connection, boot_now);
            let saved_state = load_pet_state(&connection)?;
            app.manage(Database(Mutex::new(connection)));
            app.manage(EmailTasks(Mutex::new(email::EmailTaskStore::default())));
            app.manage(ExecutionCandidates(Mutex::new(
                windows_actions::CandidateVault::default(),
            )));
            app.manage(RuntimeReadiness(AtomicBool::new(false)));
            app.manage(RuntimeLifecycle {
                marker_path,
                recovered_from_unclean_exit,
            });
            app.manage(CompanionSession(Mutex::new(CompanionState::default())));
            app.manage(PetMotionState(Mutex::new(PetMotion {
                locked: saved_state.locked,
                ..PetMotion::default()
            })));
            start_reminder_scheduler(app.handle().clone());
            start_mailbox_scheduler(app.handle().clone());

            let window = app
                .get_webview_window("pet")
                .expect("pet window should exist");

            if let Some(monitor) = window.primary_monitor()? {
                let monitor_size = monitor.size();
                let window_size = window.outer_size()?;
                let monitor_position = monitor.position();
                let (x, y) = saved_state.position.map_or_else(
                    || {
                        let (x, y) = bottom_right_position(
                            monitor_size.width,
                            monitor_size.height,
                            window_size.width,
                            window_size.height,
                            48,
                        );
                        (monitor_position.x + x, monitor_position.y + y)
                    },
                    |position| {
                        constrain_pet_position(
                            position,
                            (window_size.width, window_size.height),
                            (monitor_position.x, monitor_position.y),
                            (monitor_size.width, monitor_size.height),
                        )
                    },
                );

                window.set_position(PhysicalPosition::new(x, y))?;
                if let Some(motion) = app.try_state::<PetMotionState>() {
                    if let Ok(mut state) = motion.0.lock() {
                        state.last_normal_position = (x, y);
                    }
                }
            }

            let tracker_app = app.handle().clone();
            std::thread::spawn(move || crate::activity::run_activity_tracker(tracker_app, 5));

            // 自由活动：决策循环（收起/出发）与移动执行循环（位置插值）。
            let motion_app = app.handle().clone();
            tauri::async_runtime::spawn(async move { pet_motion_ticker(motion_app).await });
            let mover_app = app.handle().clone();
            tauri::async_runtime::spawn(async move { pet_motion_mover(mover_app).await });

            // 启动简报：首条日期/时间/天气，第二条邮箱/今日待办（异步，不阻塞启动）。
            let greeting_app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_millis(2500)).await;
                // 最多等待 30 秒；前端未就绪时宁可跳过，也不能投递一条用户看不到的问候。
                let mut ready = pet_frontend_ready(&greeting_app);
                for _ in 0..110 {
                    if ready {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    ready = pet_frontend_ready(&greeting_app);
                }
                if !ready {
                    return;
                }
                let (greeting, status) = futures_util::future::join(
                    build_startup_greeting(&greeting_app),
                    build_startup_status(&greeting_app),
                )
                .await;
                ensure_pet_visible(&greeting_app);
                if let Some(window) = greeting_app.get_webview_window("pet") {
                    let _ = window.show();
                }
                let _ = greeting_app.emit_to("pet", "startup-briefing", vec![greeting, status]);
            });

            let mut tray = TrayIconBuilder::new().tooltip("希莉尔 · Cyral");
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.on_tray_icon_event(|tray, event| {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                {
                    let app = tray.app_handle();
                    rescue_pet_window(app);
                }
            })
            .build(app)?;

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build Cyral")
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                if let Some(lifecycle) = app.try_state::<RuntimeLifecycle>() {
                    let _ = finish_runtime_session(&lifecycle.marker_path);
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::{
        apply_movement_permission, begin_runtime_session, bottom_right_position, budget_blocked,
        budget_warning, compact_anchor_position, constrain_pet_position, distance,
        finish_runtime_session, initialize_database, interpolate_position, load_pet_state,
        local_month_key, near_screen_edges, pet_window_logical_size, pick_walk_target,
        point_in_rect, rebase_walk_after_drag, record_usage_from_response, related_panel_position,
        save_pet_locked, save_pet_position, should_compact, should_restore_from_compact,
        should_walk, walk_distance_range, MotionState, PetMotion, RuntimeReadiness, XorShift,
        AGENT_TOOL_POLICY, COMPACT_AFTER_SECS, USAGE_BUFFER,
    };
    use std::sync::atomic::Ordering;

    #[cfg(windows)]
    #[test]
    fn windows_proxy_parser_accepts_single_endpoint() {
        assert_eq!(
            super::parse_windows_proxy_server("127.0.0.1:7897"),
            Some("http://127.0.0.1:7897".to_string())
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_proxy_parser_prefers_https_endpoint() {
        assert_eq!(
            super::parse_windows_proxy_server(
                "http=127.0.0.1:8080;https=127.0.0.1:7897;socks=127.0.0.1:1080"
            ),
            Some("http://127.0.0.1:7897".to_string())
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_proxy_parser_rejects_empty_or_socks_only_values() {
        assert_eq!(super::parse_windows_proxy_server("  "), None);
        assert_eq!(
            super::parse_windows_proxy_server("socks=127.0.0.1:1080"),
            None
        );
    }

    #[test]
    fn release_manifest_defaults_to_embedded_frontend_protocol() {
        let manifest = include_str!("../Cargo.toml");
        assert!(
            manifest.contains("default = [\"custom-protocol\"]"),
            "release 必须默认启用 custom-protocol，避免依赖 127.0.0.1 开发服务器"
        );
    }

    #[test]
    fn single_instance_plugin_is_part_of_the_runtime() {
        let manifest = include_str!("../Cargo.toml");
        let source = include_str!("lib.rs");
        assert!(manifest.contains("tauri-plugin-single-instance"));
        assert!(source.contains(".plugin(tauri_plugin_single_instance::init"));
        assert!(source.contains("rescue_pet_window(app)"));
    }

    #[test]
    fn frontend_readiness_is_closed_until_the_pet_registers_listeners() {
        let readiness = RuntimeReadiness(std::sync::atomic::AtomicBool::new(false));
        assert!(!readiness.0.load(Ordering::Acquire));
        readiness.0.store(true, Ordering::Release);
        assert!(readiness.0.load(Ordering::Acquire));
    }

    #[test]
    fn runtime_marker_distinguishes_clean_and_unclean_shutdowns() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let marker = std::env::temp_dir().join(format!(
            "cyral-runtime-marker-{}-{unique}",
            std::process::id()
        ));

        assert!(!begin_runtime_session(&marker, 100).unwrap());
        assert!(begin_runtime_session(&marker, 200).unwrap());
        finish_runtime_session(&marker).unwrap();
        assert!(!marker.exists());
    }

    #[test]
    fn budget_blocks_only_at_or_above_the_limit() {
        assert!(!budget_blocked(9.99, 10.0));
        assert!(budget_blocked(10.0, 10.0));
        assert!(budget_blocked(12.0, 10.0));
        assert!(!budget_blocked(5.0, 0.0), "未配置预算时不拦截");
    }

    #[test]
    fn dropped_file_context_exposes_metadata_without_reading_content() {
        let root = std::env::temp_dir().join(format!(
            "cyral-drop-context-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("private.txt");
        std::fs::write(&path, "不能在拖入时读取的内容").unwrap();
        let mut vault = crate::windows_actions::CandidateVault::default();
        let (candidate_id, _) = vault.register_dropped_file(&path).unwrap();
        let candidates = std::sync::Mutex::new(vault);

        let context = super::dropped_file_context(&[candidate_id], &candidates)
            .unwrap()
            .unwrap();
        assert!(context.content.contains("private.txt"));
        assert!(context.content.contains("read_dropped_text_file"));
        assert!(!context.content.contains("不能在拖入时读取的内容"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn budget_warning_fires_once_across_each_threshold() {
        let warning = budget_warning(8.0, 10.0, 0.0).expect("80% 应提醒");
        assert!(warning.contains("80%"));
        assert_eq!(budget_warning(8.5, 10.0, 0.8), None, "跨过 80% 后不再重复");
        let blocked = budget_warning(10.0, 10.0, 0.8).expect("100% 应提示已暂停");
        assert!(blocked.contains("已用完"));
        assert!(
            budget_warning(10.0, 10.0, 1.0).is_some(),
            "达到 100% 时始终提示已暂停，去重由调用方的 LAST_WARNED_BUDGET_RATIO 维护"
        );
        assert_eq!(budget_warning(1.0, 0.0, 0.0), None);
    }

    #[test]
    fn local_month_key_uses_local_timezone() {
        // 2026-08-01T00:00:00+08:00（上海）。
        assert_eq!(local_month_key(1_785_513_600), "2026-08");
        assert_eq!(local_month_key(0), "1970-01");
    }

    #[test]
    fn records_usage_from_responses_protocol() {
        let _ = USAGE_BUFFER
            .lock()
            .map(|mut buffer| std::mem::take(&mut *buffer));
        let body = serde_json::json!({ "model": "deepseek-v4-flash" });
        let response = serde_json::json!({
            "usage": { "input_tokens": 12, "output_tokens": 34, "total_tokens": 46 }
        });

        record_usage_from_response(
            &body,
            "https://api.deepseek.com/v1/responses",
            &response.to_string(),
        );

        let buffer = USAGE_BUFFER.lock().unwrap();
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer[0].protocol, "responses");
        assert_eq!(buffer[0].prompt_tokens, 12);
        assert_eq!(buffer[0].completion_tokens, 34);
        assert_eq!(buffer[0].total_tokens, 46);
        assert_eq!(buffer[0].model, "deepseek-v4-flash");
        assert!((buffer[0].est_cost_usd - 46.0 / 1_000_000.0).abs() < 1e-12);
    }

    #[test]
    fn ignores_responses_without_usage() {
        let _ = USAGE_BUFFER
            .lock()
            .map(|mut buffer| std::mem::take(&mut *buffer));
        let body = serde_json::json!({ "model": "deepseek-v4-flash" });

        record_usage_from_response(
            &body,
            "https://api.deepseek.com/v1/responses",
            r#"{"id":"x"}"#,
        );

        assert!(USAGE_BUFFER.lock().unwrap().is_empty());
    }

    #[test]
    fn places_the_pet_inside_the_bottom_right_work_area() {
        assert_eq!(bottom_right_position(1920, 1040, 220, 260, 24), (1676, 756));
    }

    #[test]
    fn keeps_a_panel_related_to_a_bottom_right_pet_inside_the_monitor() {
        assert_eq!(
            related_panel_position((0, 0), (1920, 1080), (1700, 800), (220, 260), (480, 680)),
            (1208, 384)
        );
    }

    #[test]
    fn places_the_panel_to_the_right_of_a_top_left_pet() {
        assert_eq!(
            related_panel_position((0, 0), (1920, 1080), (0, 0), (220, 260), (480, 680)),
            (232, 16)
        );
    }

    #[test]
    fn initializes_a_default_pet_state() {
        let connection = Connection::open_in_memory().expect("in-memory database should open");

        initialize_database(&connection).expect("schema should initialize");
        let state = load_pet_state(&connection).expect("default state should load");

        assert!(!state.locked);
        assert_eq!(state.position, None);
        assert_eq!(state.last_active_at, 0);
    }

    #[test]
    fn persists_the_pet_lock_and_position() {
        let connection = Connection::open_in_memory().expect("in-memory database should open");
        initialize_database(&connection).expect("schema should initialize");

        save_pet_locked(&connection, true).expect("lock should save");
        save_pet_position(&connection, 321, 654).expect("position should save");
        let state = load_pet_state(&connection).expect("saved state should load");

        assert!(state.locked);
        assert_eq!(state.position, Some((321, 654)));
    }

    #[test]
    fn records_the_last_active_time() {
        let connection = Connection::open_in_memory().expect("in-memory database should open");
        initialize_database(&connection).expect("schema should initialize");

        super::mark_active(&connection).expect("active time should save");

        assert!(load_pet_state(&connection).unwrap().last_active_at > 0);
    }

    #[test]
    fn constrains_a_restored_pet_position_to_the_visible_monitor() {
        assert_eq!(
            constrain_pet_position((2392, 1232), (220, 260), (0, 0), (2560, 1440)),
            (2340, 1180)
        );
        assert_eq!(
            constrain_pet_position((-400, -300), (220, 260), (0, 0), (1920, 1080)),
            (0, 0)
        );
    }

    #[test]
    fn proactive_interval_is_random_and_stays_within_ten_to_fifteen_minutes() {
        let mut rng = XorShift::new(42);
        let first = super::draw_proactive_interval_secs(&mut rng);
        let second = super::draw_proactive_interval_secs(&mut rng);

        assert!((600..=900).contains(&first));
        assert!((600..=900).contains(&second));
        assert_ne!(first, second, "同一随机源的相邻周期必须重新抽取");
    }

    #[test]
    fn retryable_proactive_failures_retry_twice_then_return_to_a_random_cycle() {
        let mut state = super::CompanionState {
            active: true,
            next_proactive_at: Some(0),
            ..super::CompanionState::default()
        };

        for expected_retry in 1..=2 {
            let generation = super::claim_due_proactive(&mut state, super::now_unix())
                .expect("到期任务必须能被领取");
            super::finish_proactive(
                &mut state,
                generation,
                super::ProactiveOutcome::Retryable("网络暂时不可用"),
            );
            assert_eq!(state.proactive_retry_count, expected_retry);
            assert!(state
                .last_proactive_result
                .as_deref()
                .unwrap()
                .contains("重试"));
            state.next_proactive_at = Some(super::now_unix());
        }

        let generation = super::claim_due_proactive(&mut state, super::now_unix()).unwrap();
        super::finish_proactive(
            &mut state,
            generation,
            super::ProactiveOutcome::Retryable("网络暂时不可用"),
        );
        assert_eq!(state.proactive_retry_count, 0);
        assert!(state.next_proactive_at.unwrap() >= super::now_unix() + 600);
        assert!(state
            .last_proactive_result
            .as_deref()
            .unwrap()
            .contains("本轮失败"));
    }

    #[test]
    fn movement_permission_defaults_to_allowed() {
        let motion = PetMotion::default();
        assert!(motion.movement_allowed);
        assert_eq!(motion.state, MotionState::NormalIdle);
    }

    #[test]
    fn companion_mode_blocks_compacting_after_idle_timeout() {
        let now = 1_800_000_000i64;
        let motion = PetMotion {
            last_user_interaction_at: now - COMPACT_AFTER_SECS,
            ..PetMotion::default()
        };

        assert!(!should_compact(&motion, true, now));
        assert!(should_compact(&motion, false, now));
    }

    #[test]
    fn normal_mode_compact_restores_after_ten_minutes() {
        let now = 1_800_000_000i64;
        let motion = PetMotion {
            state: MotionState::Compact,
            compact_started_at: Some(now - 600),
            ..PetMotion::default()
        };

        assert!(should_restore_from_compact(&motion, false, now));
        assert!(should_restore_from_compact(&motion, true, now - 599));
    }
    #[test]
    fn compact_fires_only_when_idle_unlocked_and_overdue() {
        let now = 1_800_000_000i64;
        let mut motion = PetMotion {
            last_user_interaction_at: now - 10,
            ..PetMotion::default()
        };
        assert!(!should_compact(&motion, false, now));

        motion.last_user_interaction_at = now - COMPACT_AFTER_SECS;
        assert!(should_compact(&motion, false, now));

        motion.locked = true;
        assert!(!should_compact(&motion, false, now), "锁定后不收起");

        motion.locked = false;
        motion.dragging = true;
        assert!(!should_compact(&motion, false, now), "拖动中不收起");
    }

    #[test]
    fn compacting_transition_cannot_start_another_motion() {
        let now = 1_800_000_000i64;
        let motion = PetMotion {
            state: MotionState::Compacting,
            last_user_interaction_at: 0,
            stay_until: 0,
            walk_available: true,
            ..PetMotion::default()
        };

        assert!(!should_compact(&motion, false, now));
        assert!(!should_walk(&motion, now));
    }

    #[test]
    fn walk_fires_only_when_permission_and_walk_asset_are_ready() {
        let now = 1_800_000_000i64;
        let mut motion = PetMotion {
            stay_until: now - 1,
            walk_available: true,
            ..PetMotion::default()
        };
        assert!(should_walk(&motion, now));

        motion.walk_available = false;
        assert!(!should_walk(&motion, now), "walk 资源缺失不游走");

        motion.walk_available = true;
        motion.movement_allowed = false;
        assert!(!should_walk(&motion, now), "移动许可=false 不游走");

        motion.movement_allowed = true;
        motion.locked = true;
        assert!(!should_walk(&motion, now), "锁定不游走");

        motion.locked = false;
        motion.dragging = true;
        assert!(!should_walk(&motion, now), "拖动中不发起新行走");
    }

    #[test]
    fn walk_target_stays_inside_work_area_and_respects_min_distance() {
        let mut rng = XorShift::new(42);
        let current = (960, 500);
        let blocked = [(100, 100, 300, 200)];
        for _ in 0..50 {
            let target = pick_walk_target(
                &mut rng,
                current,
                (300, 260),
                (0, 0),
                (1920, 1080),
                &blocked,
                None,
            );
            let d = distance(target, current);
            let (min_d, max_d) = walk_distance_range(1920 - 300 - 48, 1080 - 260 - 48);
            assert!(d >= min_d as f64 - 0.001, "单段距离不得过短：{d}");
            assert!(
                d <= max_d as f64 + 0.001,
                "单段距离不得超出工作区缩放上限：{d}"
            );
            assert!(target.0 >= 24 && target.0 <= 1920 - 300 - 24);
            assert!(target.1 >= 24 && target.1 <= 1080 - 260 - 24);
            assert!(!point_in_rect(target, blocked[0], 12), "不得落入可见窗口");
        }
    }

    #[test]
    fn walk_distance_scales_with_work_area() {
        // 1920×1080 工作区：短边 772×0.2=154.4 被下限 200 顶起；长边 1572×0.7≈1100。
        assert_eq!(walk_distance_range(1572, 772), (200, 1100));
        // 大工作区：短边 1500×0.2=300 超过下限，按比例放大。
        assert_eq!(walk_distance_range(3000, 1500), (300, 2100));
        // 极小工作区：下限兜底。
        assert_eq!(walk_distance_range(400, 300), (200, 280));
    }

    #[test]
    fn near_edges_detects_window_close_to_monitor_border() {
        assert_eq!(
            near_screen_edges((20, 500), (300, 260), (0, 0), (1920, 1080), 80),
            vec!["left"]
        );
        assert_eq!(
            near_screen_edges((1600, 500), (300, 260), (0, 0), (1920, 1080), 80),
            vec!["right"]
        );
        assert_eq!(
            near_screen_edges((960, 10), (300, 260), (0, 0), (1920, 1080), 80),
            vec!["top"]
        );
        assert_eq!(
            near_screen_edges((960, 790), (300, 260), (0, 0), (1920, 1080), 80),
            vec!["bottom"]
        );
        assert_eq!(
            near_screen_edges((960, 500), (300, 260), (0, 0), (1920, 1080), 80),
            Vec::<&str>::new()
        );
        // 角落：左+下同时贴近。
        assert_eq!(
            near_screen_edges((20, 790), (300, 260), (0, 0), (1920, 1080), 80),
            vec!["left", "bottom"]
        );
    }

    #[test]
    fn walk_target_avoids_ping_ponging_to_the_previous_spot() {
        let mut rng = XorShift::new(7);
        let current = (500, 500);
        let previous = (400, 500);
        for _ in 0..30 {
            let target = pick_walk_target(
                &mut rng,
                current,
                (300, 260),
                (0, 0),
                (1920, 1080),
                &[],
                Some(previous),
            );
            assert!(
                distance(target, previous) >= 120.0,
                "不得紧邻折返到上一目标"
            );
        }
    }

    #[test]
    fn compact_anchor_sits_fully_visible_at_the_right_edge_middle() {
        assert_eq!(
            compact_anchor_position((0, 0), (1920, 1080), 64, 24),
            (1832, 508)
        );
        assert_eq!(
            compact_anchor_position((1920, 0), (1920, 1080), 64, 24),
            (3752, 508)
        );
    }

    #[test]
    fn pet_window_sizes_are_defined_in_logical_pixels_for_dpi_safety() {
        assert_eq!(pet_window_logical_size(false), (300.0, 260.0));
        assert_eq!(pet_window_logical_size(true), (77.0, 77.0));
    }

    #[test]
    fn movement_tool_policy_covers_the_users_explicit_free_movement_phrase() {
        assert!(AGENT_TOOL_POLICY.contains("自由移动"));
    }

    #[test]
    fn enabling_movement_makes_the_next_walk_immediately_eligible() {
        let mut motion = PetMotion {
            movement_allowed: false,
            stay_until: i64::MAX,
            ..PetMotion::default()
        };

        apply_movement_permission(&mut motion, true);

        assert!(motion.movement_allowed);
        assert_eq!(motion.stay_until, 0);
    }

    #[test]
    fn interpolate_position_is_linear_between_endpoints() {
        assert_eq!(interpolate_position((0, 0), (200, 100), 0.0), (0, 0));
        assert_eq!(interpolate_position((0, 0), (200, 100), 0.5), (100, 50));
        assert_eq!(interpolate_position((0, 0), (200, 100), 1.0), (200, 100));
        assert_eq!(
            interpolate_position((0, 0), (200, 100), 2.0),
            (200, 100),
            "超界收敛到终点"
        );
    }

    #[test]
    fn drag_rebase_continues_the_same_walk_from_the_drop_point() {
        let rebased = rebase_walk_after_drag((900, 300), (1200, 500), 96.0, 42_000)
            .expect("落点远离目标时应从落点继续原行走");
        assert_eq!(
            rebased,
            MotionState::Walking {
                from: (900, 300),
                to: (1200, 500),
                speed_px_s: 96.0,
                started_at_ms: 42_000,
            },
            "目标与速度不变，仅起点改为落点并重新计时"
        );
    }

    #[test]
    fn drag_rebase_arrives_when_dropped_on_the_target() {
        assert!(
            rebase_walk_after_drag((1210, 490), (1200, 500), 96.0, 42_000).is_none(),
            "落点已到达目标附近时结束本段行走"
        );
    }

    #[test]
    fn persists_a_user_message_in_the_local_conversation() {
        let connection = Connection::open_in_memory().expect("in-memory database should open");
        initialize_database(&connection).expect("schema should initialize");

        super::save_message(&connection, "user", "今天过得怎么样？").expect("message should save");
        let messages = super::load_recent_messages(&connection, 10).expect("messages should load");

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "今天过得怎么样？");
    }

    #[test]
    fn persists_a_proactive_reply_with_its_visible_source() {
        let connection = Connection::open_in_memory().expect("in-memory database should open");
        initialize_database(&connection).expect("schema should initialize");

        super::save_message_with_source(
            &connection,
            "assistant",
            "这份 PPT 这两天都在陪你忙呀。",
            "proactive",
        )
        .expect("proactive reply should save");

        let messages = super::load_recent_messages(&connection, 10).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "assistant");
        assert_eq!(messages[0].source, "proactive");
        assert_eq!(messages[0].content, "这份 PPT 这两天都在陪你忙呀。");
    }
    #[test]
    fn extracts_memory_after_every_fifth_user_turn() {
        assert!(!super::should_extract_memories(4));
        assert!(super::should_extract_memories(5));
        assert!(super::should_extract_memories(10));
    }

    #[test]
    fn recognizes_only_loopback_urls_as_private_local_models() {
        assert!(super::is_loopback_model_url("http://localhost:1234/v1"));
        assert!(super::is_loopback_model_url("http://127.0.0.1:11434/v1"));
        assert!(super::is_loopback_model_url("http://[::1]:1234/v1"));
        assert!(!super::is_loopback_model_url("http://192.168.1.8:1234/v1"));
        assert!(!super::is_loopback_model_url("https://api.example.com/v1"));
    }

    #[test]
    fn only_marks_messages_with_both_task_and_time_clues_as_todo_candidates() {
        assert!(super::looks_like_todo_candidate("提醒我明天下午四点交 PPT"));
        assert!(super::looks_like_todo_candidate("下周一要开组会"));
        assert!(!super::looks_like_todo_candidate("我今天心情不错"));
        assert!(!super::looks_like_todo_candidate("记得把论文写完"));
    }

    #[test]
    fn todo_drafts_must_be_confirmed_before_becoming_active() {
        let connection = Connection::open_in_memory().expect("in-memory database should open");
        initialize_database(&connection).expect("schema should initialize");
        let id = super::save_todo_draft(
            &connection,
            "提交 PPT",
            "2026-08-12T16:00",
            120,
            "今天下午四点要交 PPT",
        )
        .unwrap();
        assert_eq!(super::load_todos(&connection).unwrap()[0].status, "draft");
        super::confirm_todo(&connection, id, "提交答辩 PPT", "2026-08-12T16:00", 120).unwrap();
        assert_eq!(super::load_todos(&connection).unwrap()[0].status, "active");
    }

    fn insert_active_todo(
        connection: &Connection,
        title: &str,
        deadline_local: &str,
        now: i64,
    ) -> i64 {
        connection
            .execute(
                "INSERT INTO todos (title, deadline_local, remind_before_minutes, source_text, status, created_at)
                 VALUES (?1, ?2, 0, ?3, 'active', ?4)",
                rusqlite::params![title, deadline_local, title, now],
            )
            .unwrap();
        connection.last_insert_rowid()
    }

    fn local_deadline(timestamp: i64) -> String {
        use chrono::TimeZone;
        chrono::Local
            .timestamp_opt(timestamp, 0)
            .single()
            .unwrap()
            .format("%Y-%m-%dT%H:%M")
            .to_string()
    }

    #[test]
    fn reminders_fire_at_thirty_minutes_then_five_minutes_before() {
        let connection = Connection::open_in_memory().unwrap();
        initialize_database(&connection).unwrap();
        let now = 1_800_000_000i64;
        let deadline = local_deadline(now + 5_400);
        let id = insert_active_todo(&connection, "交周报", &deadline, now);
        super::initialize_reminder_points(&connection, id, &deadline, now).unwrap();

        assert!(super::load_due_todos(&connection, now).unwrap().is_empty());
        let thirty_due = super::load_due_todos(&connection, now + 3_600).unwrap();
        assert_eq!(thirty_due.len(), 1);
        assert_eq!(thirty_due[0].point, super::ReminderPoint::ThirtyMinutes);

        let five_due = super::load_due_todos(&connection, now + 5_100).unwrap();
        assert_eq!(five_due.len(), 1);
        assert_eq!(five_due[0].point, super::ReminderPoint::FiveMinutes);
    }

    #[test]
    fn reminders_begin_two_hours_before_the_deadline() {
        let connection = Connection::open_in_memory().unwrap();
        initialize_database(&connection).unwrap();
        let now = 1_800_000_000i64;
        let deadline = local_deadline(now + 10_800);
        let id = insert_active_todo(&connection, "提交设计稿", &deadline, now);
        super::initialize_reminder_points(&connection, id, &deadline, now).unwrap();

        assert!(super::load_due_todos(&connection, now + 3_599)
            .unwrap()
            .is_empty());
        let due = super::load_due_todos(&connection, now + 3_600).unwrap();
        assert_eq!(due.len(), 1);
        assert!(
            super::todo_reminder_message(&due[0].todo, due[0].point, now + 3_600)
                .contains("2 小时")
        );
    }
    #[test]
    fn creation_skips_past_reminder_points() {
        let connection = Connection::open_in_memory().unwrap();
        initialize_database(&connection).unwrap();
        let now = 1_800_000_000i64;
        // 截止时间在 30 分钟后：1 小时点已过，应被跳过，只保留 10 分钟点。
        let deadline = local_deadline(now + 1800);
        let id = insert_active_todo(&connection, "交材料", &deadline, now);
        super::initialize_reminder_points(&connection, id, &deadline, now).unwrap();

        let reminded_2h: Option<i64> = connection
            .query_row(
                "SELECT reminded_2h_at FROM todos WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(reminded_2h.is_some());
        // 现在不触发（10 分钟点未到）。
        assert!(super::load_due_todos(&connection, now).unwrap().is_empty());
        // 到 10 分钟点后触发 TenMinutesBefore。
        let due = super::load_due_todos(&connection, now + 1500).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].point, super::ReminderPoint::FiveMinutes);
    }

    #[test]
    fn todo_completion_matches_exact_then_fuzzy_titles() {
        let todos = vec![
            super::TodoItem {
                id: 1,
                title: "提交周报".to_string(),
                deadline_local: "2026-08-20T18:00".to_string(),
                remind_before_minutes: 0,
                source_text: "测试".to_string(),
                status: "active".to_string(),
                created_at: 0,
            },
            super::TodoItem {
                id: 2,
                title: "提交周报 v2".to_string(),
                deadline_local: "2026-08-21T18:00".to_string(),
                remind_before_minutes: 0,
                source_text: "测试".to_string(),
                status: "active".to_string(),
                created_at: 0,
            },
            super::TodoItem {
                id: 3,
                title: "买咖啡".to_string(),
                deadline_local: "2026-08-22T09:00".to_string(),
                remind_before_minutes: 0,
                source_text: "测试".to_string(),
                status: "active".to_string(),
                created_at: 0,
            },
        ];

        let exact = super::find_todo_candidates(&todos, "提交周报 v2");
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].id, 2);

        let fuzzy = super::find_todo_candidates(&todos, "周报");
        assert_eq!(fuzzy.len(), 2);

        let none = super::find_todo_candidates(&todos, "不存在");
        assert!(none.is_empty());
        assert!(super::find_todo_candidates(&todos, "").is_empty());
    }

    #[test]
    fn parses_a_structured_todo_candidate_from_a_fenced_model_reply() {
        let candidate = super::parse_todo_candidate(
            "```json\n{\"todo\":{\"title\":\"提交 PPT\",\"deadlineLocal\":\"2026-08-12T16:00\",\"remindBeforeMinutes\":120}}\n```",
        )
        .expect("todo extraction should parse")
        .expect("a todo should be present");

        assert_eq!(candidate.title, "提交 PPT");
        assert_eq!(candidate.deadline_local, "2026-08-12T16:00");
        assert_eq!(candidate.remind_before_minutes, 120);
    }

    #[test]
    fn accepts_an_explicit_empty_todo_extraction() {
        assert_eq!(
            super::parse_todo_candidate("{\"todo\":null}").unwrap(),
            None
        );
    }

    #[test]
    fn finds_only_active_todos_whose_reminder_point_has_arrived() {
        let connection = Connection::open_in_memory().expect("in-memory database should open");
        initialize_database(&connection).expect("schema should initialize");
        let now = 1_800_000_000i64;
        // 90 分钟后截止：1 小时点已到。
        let due_deadline = local_deadline(now + 5400);
        let due_id = insert_active_todo(&connection, "提交 PPT", &due_deadline, now);
        super::initialize_reminder_points(&connection, due_id, &due_deadline, now).unwrap();
        // 3 小时后截止：两个点都未到。
        let future_deadline = local_deadline(now + 14_400);
        let future_id = insert_active_todo(&connection, "晚间复盘", &future_deadline, now);
        super::initialize_reminder_points(&connection, future_id, &future_deadline, now).unwrap();

        let due = super::load_due_todos(&connection, now + 3600).unwrap();

        assert_eq!(due.len(), 1);
        assert_eq!(due[0].todo.id, due_id);
    }

    #[test]
    fn marking_a_reminder_point_prevents_duplicate_delivery() {
        let connection = Connection::open_in_memory().expect("in-memory database should open");
        initialize_database(&connection).expect("schema should initialize");
        let now = 1_800_000_000i64;
        let deadline = local_deadline(now + 5400);
        let id = insert_active_todo(&connection, "提交 PPT", &deadline, now);
        super::initialize_reminder_points(&connection, id, &deadline, now).unwrap();

        let hour_due = super::load_due_todos(&connection, now + 3600).unwrap();
        assert_eq!(hour_due.len(), 1);
        super::mark_reminder_point(
            &connection,
            id,
            super::ReminderPoint::ThirtyMinutes,
            now + 3600,
        )
        .unwrap();

        assert!(super::load_due_todos(&connection, now + 3601)
            .unwrap()
            .is_empty());
        // 10 分钟点仍可单独触发。
        let ten_due = super::load_due_todos(&connection, now + 10500).unwrap();
        assert_eq!(ten_due.len(), 1);
        assert_eq!(ten_due[0].point, super::ReminderPoint::FiveMinutes);
    }

    #[test]
    fn waking_near_deadline_consumes_older_overdue_reminders() {
        let connection = Connection::open_in_memory().expect("in-memory database should open");
        initialize_database(&connection).expect("schema should initialize");
        let now = 1_800_000_000i64;
        let deadline = local_deadline(now + 10_800);
        let id = insert_active_todo(&connection, "提交答辩材料", &deadline, now);
        super::initialize_reminder_points(&connection, id, &deadline, now).unwrap();

        let resumed_at = now + 10_500;
        let due = super::load_due_todos(&connection, resumed_at).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].point, super::ReminderPoint::FiveMinutes);
        super::mark_delivered_reminder(&connection, &due[0], resumed_at).unwrap();

        assert!(super::load_due_todos(&connection, resumed_at + 1)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn completing_a_todo_removes_it_from_active_reminders() {
        let connection = Connection::open_in_memory().expect("in-memory database should open");
        initialize_database(&connection).expect("schema should initialize");
        let id = super::save_todo_draft(
            &connection,
            "提交 PPT",
            "2026-08-12T16:00",
            120,
            "今天下午四点交 PPT",
        )
        .unwrap();
        super::confirm_todo(&connection, id, "提交 PPT", "2026-08-12T16:00", 120).unwrap();

        super::complete_todo(&connection, id).unwrap();

        assert_eq!(
            super::load_todos(&connection).unwrap()[0].status,
            "completed"
        );
    }

    #[test]
    fn snoozing_a_todo_makes_it_due_again_at_the_new_time() {
        let connection = Connection::open_in_memory().expect("in-memory database should open");
        initialize_database(&connection).expect("schema should initialize");
        let now = 1_800_000_000i64;
        let deadline = local_deadline(now + 5400);
        let id = insert_active_todo(&connection, "提交 PPT", &deadline, now);
        super::initialize_reminder_points(&connection, id, &deadline, now).unwrap();
        super::mark_reminder_point(&connection, id, super::ReminderPoint::TwoHours, now).unwrap();

        super::snooze_todo(&connection, id, now + 600).unwrap();

        // 延后重置两个提醒点；未到 10 分钟点前不触发。
        assert_eq!(
            super::load_due_todos(&connection, now + 601).unwrap()[0]
                .todo
                .id,
            id
        );
        assert_eq!(
            super::load_due_todos(&connection, now + 10500).unwrap()[0]
                .todo
                .id,
            id
        );
    }

    #[test]
    fn formats_a_short_companion_reminder() {
        let todo = super::TodoItem {
            id: 1,
            title: "提交 PPT".to_string(),
            deadline_local: "2026-08-12T16:00".to_string(),
            remind_before_minutes: 120,
            source_text: "今天下午四点交 PPT".to_string(),
            status: "active".to_string(),
            created_at: 0,
        };

        assert_eq!(
            super::todo_reminder_message(
                &todo,
                super::ReminderPoint::TwoHours,
                super::deadline_timestamp(&todo.deadline_local).unwrap() - 7_200,
            ),
            "提醒你一下：距离“提交 PPT”到期还有 2 小时（16:00）。"
        );
        assert_eq!(
            super::todo_reminder_message(
                &todo,
                super::ReminderPoint::FiveMinutes,
                super::deadline_timestamp(&todo.deadline_local).unwrap() - 300,
            ),
            "提醒你一下：距离“提交 PPT”到期还有 5 分钟（16:00）。"
        );
        assert_eq!(
            super::todo_reminder_message(
                &todo,
                super::ReminderPoint::FiveMinutes,
                super::deadline_timestamp(&todo.deadline_local).unwrap(),
            ),
            "提醒你一下：“提交 PPT”已到截止时间（16:00）。"
        );
    }

    #[test]
    fn persists_updates_and_deletes_traceable_memory_atoms() {
        let connection = Connection::open_in_memory().expect("in-memory database should open");
        initialize_database(&connection).expect("schema should initialize");
        super::save_memory_atom(
            &connection,
            "preference",
            "用户喜欢安静地写论文",
            "我喜欢安静地写论文",
            0.95,
        )
        .unwrap();

        let memories = super::load_memory_atoms(&connection).unwrap();
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].content, "用户喜欢安静地写论文");
        super::update_memory_atom(&connection, memories[0].id, "用户喜欢安静的写作环境").unwrap();
        assert_eq!(
            super::load_memory_atoms(&connection).unwrap()[0].content,
            "用户喜欢安静的写作环境"
        );
        super::delete_memory_atom(&connection, memories[0].id).unwrap();
        assert!(super::load_memory_atoms(&connection).unwrap().is_empty());
    }

    #[test]
    fn parses_memory_json_even_when_wrapped_in_a_markdown_fence() {
        let parsed = super::parse_memory_candidates(
            "```json\n{\"memories\":[{\"kind\":\"preference\",\"content\":\"用户喜欢咖啡\",\"sourceExcerpt\":\"我喜欢咖啡\",\"confidence\":0.98}]}\n```",
        )
        .unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].content, "用户喜欢咖啡");
    }

    #[test]
    fn serializes_local_messages_for_the_typescript_history_view() {
        let message = super::Message {
            role: "assistant".to_string(),
            content: "我在这里。".to_string(),
            created_at: 1_800_000_000,
            source: "chat".to_string(),
        };

        let json = serde_json::to_value(message).unwrap();
        assert_eq!(json["createdAt"], 1_800_000_000);
        assert!(json.get("created_at").is_none());
    }

    #[test]
    fn transient_windows_close_only_after_a_short_outside_click() {
        assert!(super::should_track_outside_click("composer", false, true));
        assert!(super::should_track_outside_click("panel", false, true));
        assert!(!super::should_track_outside_click("panel", true, true));
        assert!(!super::should_track_outside_click("composer", false, false));

        assert!(super::should_hide_after_outside_pointer(120, false, true));
        assert!(!super::should_hide_after_outside_pointer(350, false, true));
        assert!(!super::should_hide_after_outside_pointer(120, true, true));
        assert!(!super::should_hide_after_outside_pointer(120, false, false));
    }

    #[test]
    fn persists_provider_configuration_without_an_api_key_column() {
        let connection = Connection::open_in_memory().expect("in-memory database should open");
        initialize_database(&connection).expect("schema should initialize");
        let settings = super::ModelProviderSettings {
            provider_name: "OpenAI".into(),
            base_url: "https://api.openai.com/v1".into(),
            protocol: "responses".into(),
            model: "gpt-5.6-luna".into(),
            monthly_budget_usd: 10.0,
        };

        super::save_provider_settings(&connection, &settings).expect("settings should save");

        assert_eq!(
            super::load_provider_settings(&connection).unwrap(),
            settings
        );
        let columns: Vec<String> = connection
            .prepare("PRAGMA table_info(model_provider)")
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(!columns.iter().any(|column| column.contains("key")));
    }

    #[test]
    fn privacy_config_round_trips_companion_and_file_search_permissions() {
        let connection = Connection::open_in_memory().unwrap();
        initialize_database(&connection).unwrap();
        let expected = serde_json::json!({
            "screenObservation": true,
            "autoLocation": false,
            "proactiveInteraction": true,
            "fileSearchDesktop": true,
            "fileSearchDocuments": false,
            "fileSearchDownloads": true,
        });
        let config = serde_json::from_value(expected.clone()).unwrap();

        super::persist_privacy_config(&connection, &config).unwrap();

        assert_eq!(
            serde_json::to_value(super::load_privacy_config(&connection).unwrap()).unwrap(),
            expected
        );
    }
    #[test]
    fn migrates_a_legacy_database_to_the_current_schema_version() {
        let connection = Connection::open_in_memory().expect("in-memory database should open");
        connection
            .execute_batch(
                "CREATE TABLE todos (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    title TEXT NOT NULL,
                    deadline_local TEXT NOT NULL,
                    remind_before_minutes INTEGER NOT NULL,
                    source_text TEXT NOT NULL,
                    status TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    UNIQUE(source_text, status)
                );
                INSERT INTO todos
                    (title, deadline_local, remind_before_minutes, source_text, status, created_at)
                VALUES ('交论文', '2026-08-13T16:00', 120, '今天下午四点交论文', 'active', 1);",
            )
            .expect("legacy schema should initialize");

        initialize_database(&connection).expect("legacy schema should migrate");

        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            crate::storage::CURRENT_SCHEMA_VERSION
        );
        let columns: Vec<String> = connection
            .prepare("PRAGMA table_info(todos)")
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(columns.contains(&"reminded_at".to_string()));
        assert!(columns.contains(&"snoozed_until".to_string()));
        assert_eq!(
            connection
                .query_row("SELECT title FROM todos", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "交论文"
        );
    }

    #[test]
    fn database_migrations_are_idempotent() {
        let connection = Connection::open_in_memory().expect("in-memory database should open");

        initialize_database(&connection).expect("first initialization should succeed");
        initialize_database(&connection).expect("second initialization should succeed");

        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            crate::storage::CURRENT_SCHEMA_VERSION
        );
    }

    #[test]
    fn builds_deduplicated_auto_detection_candidates_for_an_unversioned_url() {
        let candidates = super::build_provider_candidates(
            "",
            "https://api3.gemai.cc/",
            "auto",
            "claude-sonnet-5",
            10.0,
        );

        assert_eq!(candidates.len(), 4);
        assert_eq!(candidates[0].provider_name, "自定义供应商");
        assert_eq!(candidates[0].base_url, "https://api3.gemai.cc");
        assert_eq!(candidates[0].protocol, "chat_completions");
        assert_eq!(candidates[1].base_url, "https://api3.gemai.cc/v1");
        assert_eq!(candidates[1].protocol, "chat_completions");
        assert_eq!(candidates[2].protocol, "responses");
        assert_eq!(candidates[3].base_url, "https://api3.gemai.cc/v1");
    }

    #[test]
    fn core_behavior_contract_separates_boundaries_from_user_persona() {
        assert!(super::CYRAL_CORE_BEHAVIOR.contains("固定行为边界"));
        assert!(super::CYRAL_CORE_BEHAVIOR.contains("用户人格设定"));
        assert!(super::CYRAL_CORE_BEHAVIOR.contains("不得声称已经完成"));
        assert!(super::CYRAL_CORE_BEHAVIOR.contains("立即停止观察"));
    }

    #[test]
    fn builds_a_responses_api_request_with_core_behavior_and_history() {
        let provider = super::ModelProviderSettings {
            provider_name: "OpenAI".into(),
            base_url: "https://api.openai.com/v1".into(),
            protocol: "responses".into(),
            model: "gpt-5.6-luna".into(),
            monthly_budget_usd: 10.0,
        };
        let history = vec![super::Message {
            role: "user".into(),
            content: "你好".into(),
            created_at: 0,
            source: "chat".to_string(),
        }];

        let request = super::build_model_request(&provider, &history).unwrap();

        assert_eq!(request.url, "https://api.openai.com/v1/responses");
        assert_eq!(request.fallback_url, None);
        assert_eq!(request.body["model"], "gpt-5.6-luna");
        assert_eq!(request.body["instructions"], super::CYRAL_CORE_BEHAVIOR);
        assert_eq!(request.body["max_output_tokens"], 1024);
        assert_eq!(request.body["input"][0]["content"], "你好");
    }

    #[test]
    fn formats_runtime_context_with_date_weekday_and_weather() {
        use chrono::TimeZone;

        let time = chrono::FixedOffset::east_opt(8 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 8, 12, 14, 30, 0)
            .unwrap();
        let context = super::runtime_environment_context(time, Some("上海：多云，31°C，体感 35°C"));

        assert!(context.content.contains("2026年8月12日"));
        assert!(context.content.contains("星期三"));
        assert!(context.content.contains("14:30"));
        assert!(context.content.contains("上海：多云，31°C，体感 35°C"));
    }

    #[test]
    fn omits_weather_when_the_user_has_not_configured_a_city() {
        use chrono::TimeZone;

        let time = chrono::FixedOffset::east_opt(8 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 8, 12, 14, 30, 0)
            .unwrap();
        let context = super::runtime_environment_context(time, None);

        assert!(context.content.contains("天气：未配置城市"));
    }

    #[test]
    fn answers_explicit_time_questions_from_the_local_clock() {
        use chrono::TimeZone;

        let time = chrono::FixedOffset::east_opt(8 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 8, 13, 14, 30, 0)
            .unwrap();

        assert_eq!(
            super::local_time_answer("现在几点了？", time),
            Some("现在是 2026年8月13日 星期四 14:30。".to_string())
        );
        assert_eq!(super::local_time_answer("帮我写论文", time), None);
    }

    #[test]
    fn extracts_a_city_only_from_explicit_weather_questions() {
        assert_eq!(
            super::weather_city_from_message("杭州今天天气怎么样？"),
            Some("杭州".to_string())
        );
        assert_eq!(
            super::weather_city_from_message("帮我看看上海的天气"),
            Some("上海".to_string())
        );
        assert_eq!(super::weather_city_from_message("现在几点？"), None);
        assert_eq!(super::weather_city_from_message("我准备去杭州旅游"), None);
    }

    #[test]
    fn treats_local_weather_phrases_as_automatic_location_requests() {
        for message in [
            "我这里天气怎么样？",
            "当地天气如何？",
            "当前位置的天气",
            "今天天气怎么样？",
        ] {
            assert_eq!(super::weather_city_from_message(message), None, "{message}");
            assert!(super::needs_automatic_location(message), "{message}");
        }
    }

    #[test]
    fn parses_weather_location_time_and_metric_from_natural_chinese() {
        use super::{WeatherMetric, WeatherTime};

        let cases = [
            (
                "当前的天气",
                None,
                WeatherTime::Current,
                WeatherMetric::Summary,
            ),
            (
                "我这里明天会下雨吗？",
                None,
                WeatherTime::DayOffset(1),
                WeatherMetric::Precipitation,
            ),
            (
                "上海明天的天气",
                Some("上海"),
                WeatherTime::DayOffset(1),
                WeatherMetric::Summary,
            ),
            (
                "北京未来三天气温怎么样？",
                Some("北京"),
                WeatherTime::NextDays(3),
                WeatherMetric::Temperature,
            ),
            (
                "明天广州会不会下雨",
                Some("广州"),
                WeatherTime::DayOffset(1),
                WeatherMetric::Precipitation,
            ),
            (
                "8月20日上海气温是多少",
                Some("上海"),
                WeatherTime::MonthDay { month: 8, day: 20 },
                WeatherMetric::Temperature,
            ),
        ];

        for (message, city, time, metric) in cases {
            let intent = super::parse_weather_intent(message)
                .unwrap_or_else(|| panic!("应识别为天气请求：{message}"));
            assert_eq!(intent.city.as_deref(), city, "{message}");
            assert_eq!(intent.time, time, "{message}");
            assert_eq!(intent.metric, metric, "{message}");
        }
    }

    #[test]
    fn does_not_treat_relative_place_or_time_words_as_city_names() {
        for message in [
            "当前的天气",
            "现在天气如何",
            "这里明天冷不冷",
            "我这儿未来三天会下雨吗",
            "本地气温多少",
        ] {
            let intent = super::parse_weather_intent(message)
                .unwrap_or_else(|| panic!("应识别为天气请求：{message}"));
            assert_eq!(intent.city, None, "{message}");
            assert!(super::needs_automatic_location(message), "{message}");
        }
    }

    #[test]
    fn recognizes_weather_questions_without_the_word_weather() {
        for message in ["明天上海会下雨吗？", "北京现在多少度？", "广州后天冷不冷？"]
        {
            assert!(super::asks_for_weather(message), "{message}");
        }
    }

    #[test]
    fn understands_additional_colloquial_weather_phrases() {
        use super::{WeatherMetric, WeatherTime};

        for message in ["我这边天气如何", "附近明天会下雨吗", "今天外面冷吗"] {
            let intent = super::parse_weather_intent(message)
                .unwrap_or_else(|| panic!("应识别为天气请求：{message}"));
            assert_eq!(intent.city, None, "{message}");
        }

        let future = super::parse_weather_intent("上海未来几天的天气").unwrap();
        assert_eq!(future.city.as_deref(), Some("上海"));
        assert_eq!(future.time, WeatherTime::NextDays(3));

        let place = super::parse_weather_intent("广州地区未来5天会不会下雨").unwrap();
        assert_eq!(place.city.as_deref(), Some("广州"));
        assert_eq!(place.time, WeatherTime::NextDays(5));
        assert_eq!(place.metric, WeatherMetric::Precipitation);

        let explicit_current = super::parse_weather_intent("当前北京气温多少").unwrap();
        assert_eq!(explicit_current.city.as_deref(), Some("北京"));
        assert_eq!(explicit_current.time, WeatherTime::Current);
    }

    #[test]
    fn does_not_route_unrelated_temperature_words_to_weather() {
        for message in ["CPU 温度是多少", "把空调温度调低", "这杯水有多少度"] {
            assert_eq!(super::parse_weather_intent(message), None, "{message}");
        }
    }

    #[test]
    fn extracts_human_city_from_reverse_geocode_address_variants() {
        let county_address = serde_json::json!({
            "address": {
                "county": "郾城区",
                "city": "漯河市",
                "state": "河南省",
                "country": "中国"
            }
        });
        let district_address = serde_json::json!({
            "address": {
                "city_district": "浦东新区",
                "municipality": "上海市",
                "country": "中国"
            }
        });

        let county = super::parse_reverse_geocode(&county_address).unwrap();
        assert_eq!(county.city.as_deref(), Some("漯河市"));
        assert_eq!(county.district.as_deref(), Some("郾城区"));
        assert_eq!(county.region.as_deref(), Some("河南省"));

        let district = super::parse_reverse_geocode(&district_address).unwrap();
        assert_eq!(district.city.as_deref(), Some("上海市"));
        assert_eq!(district.district.as_deref(), Some("浦东新区"));
    }

    #[test]
    fn recognizes_natural_first_person_location_questions() {
        for message in [
            "我现在在哪？",
            "我在哪里？",
            "你知道我在哪儿吗？",
            "能看到我的定位吗？",
        ] {
            assert!(super::asks_for_location(message), "{message}");
        }
        assert!(!super::asks_for_location("上海哪里有好吃的？"));
    }

    #[test]
    fn keeps_location_answers_in_model_history_for_truthful_follow_ups() {
        let history = vec![
            super::Message {
                role: "user".into(),
                content: "你好".into(),
                created_at: 1,
                source: "chat".to_string(),
            },
            super::Message {
                role: "assistant".into(),
                content: "你好呀".into(),
                created_at: 2,
                source: "chat".to_string(),
            },
            super::Message {
                role: "user".into(),
                content: "我现在在哪？".into(),
                created_at: 3,
                source: "chat".to_string(),
            },
            super::Message {
                role: "assistant".into(),
                content: "定位显示仍然是 SG".into(),
                created_at: 4,
                source: "chat".to_string(),
            },
        ];
        assert_eq!(super::model_conversation_context(history.clone()), history);
    }

    #[test]
    fn keeps_weather_tool_turns_in_model_history_for_truthful_follow_ups() {
        let history = vec![
            super::Message {
                role: "user".into(),
                content: "上海明天的天气怎么样？".into(),
                created_at: 1,
                source: "chat".to_string(),
            },
            super::Message {
                role: "assistant".into(),
                content: "上海明天：多云，最高 26°C，降雨概率 20%。".into(),
                created_at: 2,
                source: "chat".to_string(),
            },
            super::Message {
                role: "user".into(),
                content: "你刚才说上海明天天气怎样？".into(),
                created_at: 3,
                source: "chat".to_string(),
            },
        ];

        assert_eq!(super::model_conversation_context(history.clone()), history);
    }

    #[test]
    fn keeps_an_explicit_city_weather_request_out_of_automatic_location() {
        assert_eq!(
            super::weather_city_from_message("杭州今天天气怎么样？"),
            Some("杭州".to_string())
        );
        assert!(!super::needs_automatic_location("杭州今天天气怎么样？"));
    }

    #[test]
    fn extracts_a_city_from_natural_or_test_prefixed_weather_questions() {
        for (message, city) in [
            ("请问上海今天的天气怎么样？", "上海"),
            ("测试：上海今天的天气怎么样", "上海"),
            ("帮我查一下北京市明天天气", "北京市"),
        ] {
            assert_eq!(
                super::weather_city_from_message(message),
                Some(city.to_string()),
                "{message}"
            );
        }
    }

    #[test]
    fn never_displays_machine_location_codes_as_city_names() {
        let location = super::LocationSnapshot {
            latitude: 31.2304,
            longitude: 121.4737,
            accuracy_meters: 143.0,
            city: Some("SG0".to_string()),
            region: None,
            country: None,
        };
        assert_eq!(
            super::location_label(&location),
            "纬度 31.2304、经度 121.4737"
        );

        let nul_terminated = super::LocationSnapshot {
            city: Some("SG\0".to_string()),
            ..location
        };
        assert_eq!(
            super::location_label(&nul_terminated),
            "纬度 31.2304、经度 121.4737"
        );
    }

    #[test]
    fn removes_duplicate_region_and_city_from_location_labels() {
        let location = super::LocationSnapshot {
            latitude: 31.2304,
            longitude: 121.4737,
            accuracy_meters: 143.0,
            city: Some("上海市".to_string()),
            region: Some("上海市".to_string()),
            country: Some("中国".to_string()),
        };
        assert_eq!(super::location_label(&location), "中国 上海市");
    }

    #[test]
    fn offers_a_same_origin_v1_fallback_for_an_unversioned_base_url() {
        let provider = super::ModelProviderSettings {
            provider_name: "OpenAI-compatible".into(),
            base_url: "https://api3.gemai.cc".into(),
            protocol: "responses".into(),
            model: "gpt-5.6-luna".into(),
            monthly_budget_usd: 10.0,
        };

        let history = vec![super::Message {
            role: "user".into(),
            content: "你好".into(),
            created_at: 0,
            source: "chat".to_string(),
        }];
        let request = super::build_model_request(&provider, &history).unwrap();

        assert_eq!(request.url, "https://api3.gemai.cc/responses");
        assert_eq!(
            request.fallback_url.as_deref(),
            Some("https://api3.gemai.cc/v1/responses")
        );
    }

    #[test]
    fn builds_a_chat_completions_request_for_compatible_providers() {
        let tools = super::agent_tool_definitions("chat_completions");
        let names = tools
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["function"]["name"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "get_current_datetime",
                "get_current_location",
                "get_weather",
                "get_screen_observation",
                "prepare_todo_review",
                "mark_todo_completed",
                "recall_file_activity",
                "find_local_files",
                "find_installed_apps",
                "open_app_candidate",
                "open_file_candidate",
                "read_dropped_text_file",
                "get_inbox_status",
                "get_recent_mail_senders",
                "get_latest_email_status",
                "prepare_email_review",
                "set_movement_allowed"
            ]
        );

        let provider = super::ModelProviderSettings {
            provider_name: "兼容供应商".into(),
            base_url: "https://gateway.example/v1/".into(),
            protocol: "chat_completions".into(),
            model: "compatible-model".into(),
            monthly_budget_usd: 10.0,
        };

        let history = vec![super::Message {
            role: "user".into(),
            content: "你好".into(),
            created_at: 0,
            source: "chat".to_string(),
        }];
        let request = super::build_model_request(&provider, &history).unwrap();

        assert_eq!(request.url, "https://gateway.example/v1/chat/completions");
        assert_eq!(request.body["messages"][0]["role"], "system");
        assert_eq!(
            request.body["messages"][0]["content"],
            super::CYRAL_CORE_BEHAVIOR
        );
        assert_eq!(request.body["messages"][1]["content"], "你好");
        assert_eq!(request.body["max_tokens"], 1024);
    }

    #[test]
    fn file_search_permissions_survive_database_reopen() {
        let path = std::env::temp_dir().join(format!(
            "cyral-privacy-reopen-test-{}-{}.db",
            std::process::id(),
            super::now_unix()
        ));
        {
            let connection = Connection::open(&path).unwrap();
            initialize_database(&connection).unwrap();
            super::persist_privacy_config(
                &connection,
                &super::PrivacyConfig {
                    screen_observation: false,
                    auto_location: false,
                    proactive_interaction: true,
                    file_search_desktop: false,
                    file_search_documents: true,
                    file_search_downloads: false,
                },
            )
            .unwrap();
        }
        {
            let connection = Connection::open(&path).unwrap();
            initialize_database(&connection).unwrap();
            let persisted = super::load_privacy_config(&connection).unwrap();
            assert!(persisted.file_search_documents);
            assert!(!persisted.file_search_desktop);
            assert!(!persisted.file_search_downloads);
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn recalls_observed_files_by_name_type_and_time_window() {
        let connection = Connection::open_in_memory().unwrap();
        initialize_database(&connection).unwrap();
        crate::storage::record_cyral_activity_event(
            &connection,
            "screen_observation",
            "companion",
            "WINWORD.EXE",
            Some("个人简历.docx - Word"),
            "正在编辑个人简历",
            1_800_000_100,
        )
        .unwrap();
        crate::storage::record_cyral_activity_event(
            &connection,
            "screen_observation",
            "companion",
            "POWERPNT.EXE",
            Some("课程作业.pptx - PowerPoint"),
            "正在编辑课程作业",
            1_800_000_300,
        )
        .unwrap();

        let recalled = super::recall_file_activity(
            &connection,
            Some("简历"),
            Some("docx"),
            Some(1_800_000_000),
            Some(1_800_000_200),
        )
        .unwrap();

        assert_eq!(recalled.len(), 1);
        assert_eq!(recalled[0].name, "个人简历.docx");
        assert_eq!(recalled[0].application, "WINWORD.EXE");
        assert_eq!(recalled[0].last_observed_at, 1_800_000_100);
    }

    #[test]
    fn file_recall_context_forces_search_after_two_clarification_replies() {
        let messages = vec![
            super::Message {
                role: "user".into(),
                content: "我前几天编辑的课程作业文件在哪里？".into(),
                created_at: 1,
                source: "chat".into(),
            },
            super::Message {
                role: "assistant".into(),
                content: "你记得是什么文件类型吗？".into(),
                created_at: 2,
                source: "chat".into(),
            },
            super::Message {
                role: "user".into(),
                content: "大概是演示文稿。".into(),
                created_at: 3,
                source: "chat".into(),
            },
            super::Message {
                role: "assistant".into(),
                content: "还能再给我一点线索吗？".into(),
                created_at: 4,
                source: "chat".into(),
            },
            super::Message {
                role: "user".into(),
                content: "上周吧，记不清了。".into(),
                created_at: 5,
                source: "chat".into(),
            },
        ];

        assert!(super::file_recall_requires_tool(&messages));
        let guidance = super::file_recall_context(&messages).expect("文件找回流程应持续");
        assert!(guidance.content.contains("已追问 2 轮"));
        assert!(guidance.content.contains("不得继续追问"));
        assert!(guidance.content.contains("recall_file_activity"));
        assert!(guidance.content.contains("find_local_files"));
    }

    #[test]
    fn email_clarification_stops_after_three_combined_questions() {
        let mut messages = vec![super::Message {
            role: "user".into(),
            content: "帮我发一封邮件给老师".into(),
            created_at: 1,
            source: "chat".into(),
        }];
        for index in 0..3 {
            messages.push(super::Message {
                role: "assistant".into(),
                content: "请补充收件邮箱、主题和正文？".into(),
                created_at: index * 2 + 2,
                source: "chat".into(),
            });
            messages.push(super::Message {
                role: "user".into(),
                content: "我还没想好".into(),
                created_at: index * 2 + 3,
                source: "chat".into(),
            });
        }
        let context = super::email_clarification_context(&messages).expect("邮件流程应持续");
        assert!(context.content.contains("已追问 3 轮"));
        assert!(context.content.contains("本次准备已停止"));
        assert!(context.content.contains("不得继续追问"));
    }

    #[test]
    fn enriches_file_matches_with_observation_evidence_before_ranking() {
        let connection = Connection::open_in_memory().unwrap();
        initialize_database(&connection).unwrap();
        crate::storage::record_cyral_activity_event(
            &connection,
            "screen_observation",
            "companion",
            "POWERPNT.EXE",
            Some("课程作业.pptx - PowerPoint"),
            "正在编辑课程作业",
            1_800_000_300,
        )
        .unwrap();
        let mut matches = vec![
            super::LocalFileMatch {
                root: "文档".into(),
                relative_path: "旧课程作业.pptx".into(),
                name: "旧课程作业.pptx".into(),
                extension: "pptx".into(),
                size_bytes: 10,
                modified_at: 1_800_000_400,
                observation_count: 0,
                last_observed_at: None,
            },
            super::LocalFileMatch {
                root: "文档".into(),
                relative_path: "课程作业.pptx".into(),
                name: "课程作业.pptx".into(),
                extension: "pptx".into(),
                size_bytes: 20,
                modified_at: 1_700_000_000,
                observation_count: 0,
                last_observed_at: None,
            },
        ];

        super::enrich_and_rank_file_matches(
            &connection,
            &mut matches,
            Some(1_800_000_000),
            Some(1_800_000_500),
        )
        .unwrap();

        assert_eq!(matches[0].name, "课程作业.pptx");
        assert_eq!(matches[0].observation_count, 1);
        assert_eq!(matches[0].last_observed_at, Some(1_800_000_300));
    }

    #[test]
    fn local_file_search_returns_only_matching_files_below_an_authorized_root() {
        let root = std::env::temp_dir().join(format!(
            "cyral-file-search-test-{}-{}",
            std::process::id(),
            super::now_unix()
        ));
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::write(root.join("项目报告.pptx"), b"do not read this content").unwrap();
        std::fs::write(root.join("nested").join("项目报告.pdf"), b"also private").unwrap();
        std::fs::write(root.join("无关文件.txt"), b"unrelated").unwrap();

        let result = super::search_local_files(
            &[super::LocalFileSearchRoot {
                label: "测试目录",
                path: root.clone(),
            }],
            "报告",
            Some("pptx"),
        )
        .unwrap();

        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].root, "测试目录");
        assert_eq!(result.matches[0].relative_path, "项目报告.pptx");
        assert_eq!(result.matches[0].extension, "pptx");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_file_search_treats_ppt_as_a_presentation_family() {
        let root = std::env::temp_dir().join(format!(
            "cyral-file-search-family-test-{}-{}",
            std::process::id(),
            super::now_unix()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("课程作业.pptx"), b"private").unwrap();

        let result = super::search_local_files(
            &[super::LocalFileSearchRoot {
                label: "测试目录",
                path: root.clone(),
            }],
            "课程作业",
            Some("ppt"),
        )
        .unwrap();

        std::fs::remove_dir_all(root).unwrap();
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].name, "课程作业.pptx");
    }

    #[test]
    fn file_open_candidate_is_issued_only_for_one_complete_search_result() {
        let root = std::env::temp_dir().join(format!(
            "cyral-file-open-candidate-test-{}-{}",
            std::process::id(),
            super::now_unix()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("课程作业.pptx"), b"private").unwrap();
        std::fs::write(root.join("课程作业.pdf"), b"private").unwrap();
        let roots = vec![super::LocalFileSearchRoot {
            label: "测试目录",
            path: root.clone(),
        }];
        let candidates = std::sync::Mutex::new(crate::windows_actions::CandidateVault::default());

        let mut ambiguous = super::search_local_files(&roots, "课程作业", None).unwrap();
        super::register_unique_file_open_candidate(&roots, &mut ambiguous, &candidates).unwrap();
        assert!(ambiguous.requires_user_selection);
        assert_eq!(ambiguous.open_candidate_id, None);

        let mut unique = super::search_local_files(&roots, "课程作业", Some("pptx")).unwrap();
        super::register_unique_file_open_candidate(&roots, &mut unique, &candidates).unwrap();
        assert!(!unique.requires_user_selection);
        assert!(unique
            .open_candidate_id
            .as_deref()
            .is_some_and(|id| id.starts_with("file-")));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_file_search_rejects_path_like_keywords() {
        let root = std::env::temp_dir().join(format!(
            "cyral-file-search-keyword-test-{}-{}",
            std::process::id(),
            super::now_unix()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let error = super::search_local_files(
            &[super::LocalFileSearchRoot {
                label: "测试目录",
                path: root.clone(),
            }],
            "C:\\Users\\someone\\report",
            None,
        )
        .expect_err("关键词不得承载路径");
        std::fs::remove_dir_all(root).unwrap();
        assert!(error.contains("文件名关键词"));
    }
    #[test]
    fn agent_tools_expose_local_file_search_without_a_path_parameter() {
        let tools = super::agent_tool_definitions("chat_completions");
        let file_search = tools
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .find(|tool| tool["function"]["name"] == "find_local_files")
            })
            .expect("对话模型必须能看到只读本地文件搜索工具");
        let parameters = &file_search["function"]["parameters"];
        assert_eq!(parameters["properties"]["query"]["type"], "string");
        assert!(parameters["properties"].get("path").is_none());
        assert_eq!(parameters["additionalProperties"], false);
    }

    #[test]
    fn desktop_execution_tools_use_candidate_ids_and_expose_no_decisive_action() {
        let tools = super::agent_tool_definitions("chat_completions");
        let functions = tools.as_array().unwrap();
        let names = functions
            .iter()
            .filter_map(|tool| tool["function"]["name"].as_str())
            .collect::<Vec<_>>();

        for required in [
            "find_installed_apps",
            "open_app_candidate",
            "open_file_candidate",
        ] {
            assert!(names.contains(&required), "缺少 {required}");
        }
        for forbidden in [
            "send_email",
            "start_playback",
            "overwrite_file",
            "delete_data",
            "run_command",
        ] {
            assert!(!names.contains(&forbidden), "不得暴露 {forbidden}");
        }
        for name in ["open_app_candidate", "open_file_candidate"] {
            let parameters = functions
                .iter()
                .find(|tool| tool["function"]["name"] == name)
                .map(|tool| &tool["function"]["parameters"])
                .unwrap();
            assert_eq!(parameters["required"], serde_json::json!(["candidateId"]));
            assert!(parameters["properties"].get("path").is_none());
            assert_eq!(parameters["additionalProperties"], false);
        }
    }
    #[test]
    fn extracts_native_chat_completion_tool_calls() {
        let response = serde_json::json!({
            "choices": [{"message": {"tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {"name": "get_current_location", "arguments": "{}"}
            }]}}]
        });
        assert_eq!(
            super::extract_native_tool_calls("chat_completions", &response).unwrap(),
            vec![super::AgentToolCall {
                id: "call_1".into(),
                name: "get_current_location".into(),
                arguments: serde_json::json!({}),
            }]
        );
    }

    #[test]
    fn extracts_native_responses_api_tool_calls() {
        let response = serde_json::json!({
            "output": [{
                "type": "function_call",
                "call_id": "call_2",
                "name": "get_weather",
                "arguments": "{\"city\":\"上海\",\"date\":\"2026-08-16\"}"
            }]
        });
        let calls = super::extract_native_tool_calls("responses", &response).unwrap();
        assert_eq!(calls[0].name, "get_weather");
        assert_eq!(calls[0].arguments["city"], "上海");
        assert_eq!(calls[0].arguments["date"], "2026-08-16");
    }

    #[test]
    fn parses_provider_agnostic_json_tool_plans() {
        let raw = r#"分析略。{"toolCalls":[{"id":"fallback_1","name":"get_current_datetime","arguments":{}}]}"#;
        assert_eq!(
            super::parse_json_tool_plan(raw).unwrap(),
            vec![super::AgentToolCall {
                id: "fallback_1".into(),
                name: "get_current_datetime".into(),
                arguments: serde_json::json!({}),
            }]
        );
    }

    #[test]
    fn agent_tools_expose_todo_review_creation() {
        let tools = super::agent_tool_definitions("chat_completions");
        let todo_tool = tools
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .find(|tool| tool["function"]["name"] == "prepare_todo_review")
            })
            .expect("对话模型必须能看到待办审核工具");
        assert!(todo_tool["function"]["parameters"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field == "title"));
        assert!(todo_tool["function"]["parameters"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field == "deadlineLocal"));
    }
    #[test]
    fn builds_agent_requests_with_native_tools_without_keyword_routing() {
        use super::{WeatherMetric, WeatherTime};

        let tomorrow = super::weather_intent_from_tool_arguments(
            &serde_json::json!({"city":"上海","date":"tomorrow","metric":"precipitation"}),
        )
        .unwrap();
        assert_eq!(tomorrow.city.as_deref(), Some("上海"));
        assert_eq!(tomorrow.time, WeatherTime::DayOffset(1));
        assert_eq!(tomorrow.metric, WeatherMetric::Precipitation);

        let future = super::weather_intent_from_tool_arguments(
            &serde_json::json!({"latitude":31.2,"longitude":121.5,"days":3}),
        )
        .unwrap();
        assert_eq!(future.city, None);
        assert_eq!(future.time, WeatherTime::NextDays(3));

        assert!(
            super::weather_intent_from_tool_arguments(&serde_json::json!({"date":"tomorrow"}))
                .is_err()
        );
        assert!(super::weather_intent_from_tool_arguments(
            &serde_json::json!({"city":"上海","days":9})
        )
        .is_err());

        let provider = super::ModelProviderSettings {
            provider_name: "兼容供应商".into(),
            base_url: "https://gateway.example/v1".into(),
            protocol: "chat_completions".into(),
            model: "deepseek-v4-flash".into(),
            monthly_budget_usd: 10.0,
        };
        let context = vec![super::AgentContextItem::Plain(super::Message {
            role: "user".into(),
            content: "测试2：今天我这里是什么天气？".into(),
            created_at: 0,
            source: "chat".to_string(),
        })];
        let request = super::build_agent_request(&provider, &context, true).unwrap();
        assert_eq!(request.body["tool_choice"], "auto");
        assert_eq!(request.body["tools"].as_array().unwrap().len(), 17);
        assert_eq!(
            request.body["messages"].as_array().unwrap().last().unwrap()["content"],
            "测试2：今天我这里是什么天气？"
        );
    }

    #[test]
    fn does_not_retry_auth_or_rate_limit_failures_with_the_fallback_protocol() {
        assert!(!super::should_fallback_from_native_tools(
            "模型供应商请求失败（HTTP 401 Unauthorized）"
        ));
        assert!(!super::should_fallback_from_native_tools(
            "模型供应商请求失败（HTTP 403 Forbidden）"
        ));
        assert!(!super::should_fallback_from_native_tools(
            "模型供应商请求失败（HTTP 429 Too Many Requests）"
        ));
        assert!(super::should_fallback_from_native_tools(
            "模型供应商请求失败（HTTP 400 Bad Request）：tools not supported"
        ));
    }

    #[test]
    fn tool_failures_are_returned_as_facts_instead_of_becoming_model_guesses() {
        let call = super::AgentToolCall {
            id: "call_weather".into(),
            name: "get_weather".into(),
            arguments: serde_json::json!({"use_current_location":true}),
        };
        let message =
            super::tool_result_message(&call, serde_json::json!({"ok":false,"error":"定位未授权"}));
        let payload: serde_json::Value = serde_json::from_str(&message.content).unwrap();
        assert_eq!(payload["result"]["ok"], false);
        assert_eq!(payload["result"]["error"], "定位未授权");
        assert_eq!(payload["tool"], "get_weather");
    }

    #[test]
    fn native_chat_tool_round_backfills_calls_and_tool_results() {
        let datetime = super::AgentToolCall {
            id: "call_dt".into(),
            name: "get_current_datetime".into(),
            arguments: serde_json::json!({}),
        };
        let weather = super::AgentToolCall {
            id: "call_wx".into(),
            name: "get_weather".into(),
            arguments: serde_json::json!({"city":"北京","date":"tomorrow"}),
        };
        let mut context = vec![super::AgentContextItem::Plain(super::Message {
            role: "user".into(),
            content: "明天北京天气怎么样？".into(),
            created_at: 0,
            source: "chat".to_string(),
        })];
        super::push_tool_round(
            &mut context,
            vec![datetime.clone(), weather.clone()],
            super::ToolCallMode::Native,
            vec![
                serde_json::json!({"ok":true,"data":{"date":"2026-08-16"}}),
                serde_json::json!({"ok":true,"data":{"summary":"晴"}}),
            ],
        );
        let body = super::serialize_agent_history("chat_completions", &context).unwrap();
        let assistant = &body[1];
        assert_eq!(assistant["role"], "assistant");
        assert_eq!(assistant["tool_calls"].as_array().unwrap().len(), 2);
        assert_eq!(assistant["tool_calls"][0]["id"], "call_dt");
        assert_eq!(
            assistant["tool_calls"][1]["function"]["arguments"],
            "{\"city\":\"北京\",\"date\":\"tomorrow\"}"
        );
        let tool_dt = &body[2];
        assert_eq!(tool_dt["role"], "tool");
        assert_eq!(tool_dt["tool_call_id"], "call_dt");
        let tool_wx = &body[3];
        assert_eq!(tool_wx["role"], "tool");
        assert_eq!(tool_wx["tool_call_id"], "call_wx");
        let payload: serde_json::Value =
            serde_json::from_str(tool_wx["content"].as_str().unwrap()).unwrap();
        assert_eq!(payload["tool"], "get_weather");
        assert_eq!(payload["result"]["data"]["summary"], "晴");
    }

    #[test]
    fn native_responses_tool_round_uses_function_call_output() {
        let call = super::AgentToolCall {
            id: "fc_1".into(),
            name: "get_current_location".into(),
            arguments: serde_json::json!({}),
        };
        let mut context = vec![super::AgentContextItem::Plain(super::Message {
            role: "user".into(),
            content: "我现在在哪里？".into(),
            created_at: 0,
            source: "chat".to_string(),
        })];
        super::push_tool_round(
            &mut context,
            vec![call.clone()],
            super::ToolCallMode::Native,
            vec![serde_json::json!({
                "ok": true,
                "data": {"latitude":31.2,"longitude":121.5}
            })],
        );
        let body = super::serialize_agent_history("responses", &context).unwrap();
        let fn_call = &body[1];
        assert_eq!(fn_call["type"], "function_call");
        assert_eq!(fn_call["call_id"], "fc_1");
        assert_eq!(fn_call["name"], "get_current_location");
        let fn_output = &body[2];
        assert_eq!(fn_output["type"], "function_call_output");
        assert_eq!(fn_output["call_id"], "fc_1");
        let parsed: serde_json::Value =
            serde_json::from_str(fn_output["output"].as_str().unwrap()).unwrap();
        assert_eq!(parsed["result"]["ok"], true);
        assert_eq!(parsed["result"]["data"]["latitude"], 31.2);
    }

    #[test]
    fn json_plan_fallback_shares_the_same_tool_result_payload() {
        let call = super::AgentToolCall {
            id: "f_1".into(),
            name: "get_weather".into(),
            arguments: serde_json::json!({"city":"广州"}),
        };
        let mut context = vec![super::AgentContextItem::Plain(super::Message {
            role: "user".into(),
            content: "广州今天热吗？".into(),
            created_at: 0,
            source: "chat".to_string(),
        })];
        super::push_tool_round(
            &mut context,
            vec![call.clone()],
            super::ToolCallMode::JsonPlan,
            vec![serde_json::json!({"ok":false,"error":"无法连接 Open-Meteo"})],
        );
        let body = super::serialize_agent_history("chat_completions", &context).unwrap();
        let fallback = &body[1];
        assert_eq!(fallback["role"], "system");
        let payload: serde_json::Value =
            serde_json::from_str(fallback["content"].as_str().unwrap()).unwrap();
        assert_eq!(payload["tool"], "get_weather");
        assert_eq!(payload["result"]["ok"], false);

        // 原生与回退必须保持同一语义载荷，不得出现两套工具语义。
        let native = super::tool_result_json(
            &call,
            serde_json::json!({"ok":false,"error":"无法连接 Open-Meteo"}),
        );
        assert_eq!(native["toolCallId"], payload["toolCallId"]);
        assert_eq!(native["tool"], payload["tool"]);
        assert_eq!(native["result"], payload["result"]);
        assert_eq!(native["instruction"], payload["instruction"]);
    }

    #[test]
    fn extracts_stream_deltas_from_both_supported_protocols() {
        let responses = serde_json::json!({
            "type": "response.output_text.delta",
            "delta": "你好"
        });
        let chat = serde_json::json!({
            "choices": [{"delta": {"content": "，朋友"}}]
        });

        assert_eq!(
            super::extract_stream_delta("responses", &responses),
            Some("你好".to_string())
        );
        assert_eq!(
            super::extract_stream_delta("chat_completions", &chat),
            Some("，朋友".to_string())
        );
    }

    #[test]
    fn accepts_gateway_stream_shape_even_when_saved_protocol_is_different() {
        let chat = serde_json::json!({
            "choices": [{"delta": {"content": "你好"}}]
        });
        let responses = serde_json::json!({
            "type": "response.output_text.delta",
            "delta": "朋友"
        });

        assert_eq!(
            super::extract_stream_delta("responses", &chat),
            Some("你好".to_string())
        );
        assert_eq!(
            super::extract_stream_delta("chat_completions", &responses),
            Some("朋友".to_string())
        );
    }

    #[test]
    fn never_exposes_reasoning_stream_as_user_visible_text() {
        let responses_reasoning = serde_json::json!({
            "type": "response.reasoning_text.delta",
            "delta": "用户仍在确认我是否知道位置"
        });
        let chat_reasoning = serde_json::json!({
            "choices": [{"delta": {
                "reasoning_content": "先分析用户意图",
                "content": null
            }}]
        });

        assert_eq!(
            super::extract_stream_delta("responses", &responses_reasoning),
            None
        );
        assert_eq!(
            super::extract_stream_delta("chat_completions", &chat_reasoning),
            None
        );
    }

    #[test]
    fn extracts_only_the_explicit_final_answer_from_mixed_reasoning() {
        let mixed = "我们先分析用户的意图和历史。\n<cyral_final>最近还好吗？我一直在这里。</cyral_final>\n不要输出上面的分析。";

        assert_eq!(
            super::extract_cyral_final(mixed),
            Ok("最近还好吗？我一直在这里。".to_string())
        );
    }

    #[test]
    fn rejects_unmarked_model_output_instead_of_leaking_reasoning() {
        assert_eq!(
            super::extract_cyral_final("我们需要先分析用户消息。"),
            Err("模型没有返回可安全显示的最终正文，请重试".to_string())
        );
    }

    #[test]
    fn safe_visible_reply_rejects_unmarked_reasoning() {
        assert_eq!(
            super::extract_safe_visible_reply(
                "我会先根据当前信息判断时间是否完整，然后再生成答案。"
            ),
            Err("模型没有返回可安全显示的最终正文，请重试".to_string())
        );
    }
    #[test]
    fn extracts_final_answer_from_a_fenced_or_prefixed_json_object() {
        assert_eq!(
            super::extract_final_json("分析省略。\n```json\n{\"final\":\"我在这里。\"}\n```"),
            Ok("我在这里。".to_string())
        );
    }

    #[test]
    fn extracts_the_last_valid_final_object_when_reasoning_contains_braces() {
        let output =
            "先分析 {用户请求}，然后输出。\n{\"draft\":\"忽略\"}\n{\"final\":\"真正的回复。\"}";
        assert_eq!(
            super::extract_final_json(output),
            Ok("真正的回复。".to_string())
        );
    }

    #[test]
    fn accepts_a_tagged_answer_from_the_normalization_pass() {
        assert_eq!(
            super::extract_normalized_final("分析完成。<cyral_final>真正的回复。</cyral_final>"),
            Ok("真正的回复。".to_string())
        );
    }

    #[test]
    fn accepts_json_lines_without_sse_data_prefix() {
        let body = concat!(
            "{\"choices\":[{\"delta\":{\"content\":\"你\"}}]}\n",
            "{\"choices\":[{\"delta\":{\"content\":\"好\"}}]}\n"
        );

        assert_eq!(
            super::extract_sse_reply("chat_completions", body),
            Some("你好".to_string())
        );
    }

    #[test]
    fn ignores_stream_control_events_and_done_marker() {
        let completed = serde_json::json!({"type": "response.completed"});

        assert_eq!(super::extract_stream_delta("responses", &completed), None);
        assert_eq!(super::parse_sse_data("chat_completions", "[DONE]"), None);
    }

    #[test]
    fn takes_complete_sse_events_with_crlf_and_keeps_partial_data() {
        let mut buffer = "data: {\"delta\":\"你\"}\r\n\r\ndata: {\"delta\"".to_string();

        assert_eq!(
            super::take_sse_event(&mut buffer),
            Some("data: {\"delta\":\"你\"}".to_string())
        );
        assert_eq!(buffer, "data: {\"delta\"");
        assert_eq!(super::take_sse_event(&mut buffer), None);
    }

    #[test]
    fn recognizes_sse_content_even_when_the_provider_labels_it_as_json() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"你\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"好\"}}]}\n\n",
            "data: [DONE]\n\n"
        );

        assert_eq!(
            super::extract_sse_reply("chat_completions", body),
            Some("你好".to_string())
        );
    }

    #[test]
    fn recognizes_gateway_streams_that_omit_blank_event_separators() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"你\"}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"好\"}}]}\n",
            "data: [DONE]\n"
        );

        assert_eq!(
            super::extract_sse_reply("chat_completions", body),
            Some("你好".to_string())
        );
    }

    #[test]
    fn only_retries_endpoint_compatibility_errors_at_runtime() {
        assert!(super::is_endpoint_compatibility_error(
            "模型返回了无法识别的数据"
        ));
        assert!(super::is_endpoint_compatibility_error(
            "模型供应商请求失败（HTTP 404 Not Found）"
        ));
        assert!(!super::is_endpoint_compatibility_error(
            "模型供应商请求失败（HTTP 401 Unauthorized）"
        ));
    }

    #[test]
    fn retries_only_transient_stream_decode_failures() {
        assert!(super::is_retryable_stream_error(
            "读取流式响应失败：error decoding response body"
        ));
        assert!(super::is_retryable_stream_error(
            "读取流式响应失败：connection closed before message completed"
        ));
        assert!(!super::is_retryable_stream_error(
            "模型供应商请求失败（HTTP 401 Unauthorized）"
        ));
    }

    #[test]
    fn extracts_replies_from_both_supported_protocols() {
        let responses = serde_json::json!({
            "output": [{"content": [{"type": "output_text", "text": "我在这里。"}]}]
        });
        let chat = serde_json::json!({
            "choices": [{"message": {"content": "我也在这里。"}}]
        });

        assert_eq!(
            super::extract_model_reply("responses", &responses).unwrap(),
            "我在这里。"
        );
        assert_eq!(
            super::extract_model_reply("chat_completions", &chat).unwrap(),
            "我也在这里。"
        );
    }

    #[test]
    fn accepts_chat_protocol_visible_content_after_ignoring_reasoning() {
        let response = serde_json::json!({
            "choices": [{"message": {
                "reasoning_content": "先确认工具已经成功修改移动状态。",
                "content": "我会乖乖待在原地。"
            }}]
        });

        assert_eq!(
            super::extract_agent_decision("chat_completions", &response),
            Ok(super::AgentDecision::Final(
                "我会乖乖待在原地。".to_string()
            ))
        );
    }

    #[test]
    fn responses_protocol_ignores_reasoning_items_and_accepts_output_text() {
        let response = serde_json::json!({
            "output": [
                {"type": "reasoning", "content": [
                    {"type": "reasoning_text", "text": "先分析用户意图。"}
                ]},
                {"type": "message", "content": [
                    {"type": "output_text", "text": "已经停在这里了。"}
                ]}
            ]
        });

        assert_eq!(
            super::extract_agent_decision("responses", &response),
            Ok(super::AgentDecision::Final("已经停在这里了。".to_string()))
        );
    }

    #[test]
    fn does_not_fallback_when_the_final_body_fails_safety_validation() {
        assert!(!super::should_fallback_from_native_tools(
            "模型没有返回可安全显示的最终正文，请重试"
        ));
    }
}

#[cfg(test)]
mod runtime_context_tests {
    use super::*;
    use crate::activity::Observation;

    fn companion(active: bool, observation: Option<Observation>) -> CompanionSession {
        CompanionSession(Mutex::new(CompanionState {
            active,
            last_observation: observation,
            user_talking_to_cyral: false,
            stop_generation: 0,
            ..CompanionState::default()
        }))
    }

    fn privacy(screen_observation: bool, auto_location: bool) -> PrivacyConfig {
        PrivacyConfig {
            screen_observation,
            auto_location,
            proactive_interaction: true,
            file_search_desktop: false,
            file_search_documents: false,
            file_search_downloads: false,
        }
    }

    fn observation() -> Observation {
        Observation {
            app_name: "POWERPNT.EXE".to_string(),
            file_key: Some("报告.pptx".to_string()),
            window_title: Some("报告.pptx - PowerPoint".to_string()),
            summary: "正在编辑演示文稿".to_string(),
            source: "text".to_string(),
            captured_at: 1_752_000_000,
        }
    }

    #[test]
    fn companion_off_injects_honest_state_without_observation() {
        let message = build_runtime_context(&companion(false, None), &privacy(false, false), true)
            .expect("runtime context should build");
        assert!(message.content.contains("陪伴模式：未开启"));
        assert!(message.content.contains("屏幕观察授权：未开启"));
        assert!(message.content.contains("最近观察：暂无"));
        assert!(message.content.contains("移动许可：允许在桌面自动移动"));
        assert!(!message.content.contains("你可以看到用户屏幕"));
        assert!(message
            .content
            .contains("没有观察且陪伴模式未开启时回答“陪伴模式未开启，我看不到你的屏幕”"));
        assert!(message.content.contains("以本条为准"));
        assert!(message.content.contains("不代表观察未开启"));
    }

    #[test]
    fn companion_on_with_observation_injects_structured_facts() {
        let message = build_runtime_context(
            &companion(true, Some(observation())),
            &privacy(true, true),
            false,
        )
        .expect("runtime context should build");
        assert!(message.content.contains("陪伴模式：开启"));
        assert!(message.content.contains("移动许可：原地待机"));
        assert!(message.content.contains("最近观察：应用：POWERPNT.EXE"));
        assert!(message.content.contains("文件：报告.pptx"));
        assert!(message.content.contains("场景摘要：正在编辑演示文稿"));
        assert!(!message.content.contains("你可以看到用户屏幕"));
        assert!(message.content.contains("以本条为准"));
        assert!(message.content.contains("最近观察来自前台窗口"));
    }

    #[test]
    fn companion_on_while_talking_to_cyral_keeps_last_non_cyral_context() {
        let session = CompanionSession(Mutex::new(CompanionState {
            active: true,
            last_observation: Some(observation()),
            user_talking_to_cyral: true,
            stop_generation: 0,
            ..CompanionState::default()
        }));
        let message = build_runtime_context(&session, &privacy(true, true), true)
            .expect("runtime context should build");
        assert!(message.content.contains("用户当前正在与 Cyral 对话"));
        assert!(message
            .content
            .contains("保留的是用户此前使用的非 Cyral 应用"));
        assert!(message.content.contains("最近观察：应用：POWERPNT.EXE"));
    }

    #[test]
    fn companion_on_without_observation_answers_not_observed_yet() {
        let message = build_runtime_context(&companion(true, None), &privacy(true, false), true)
            .expect("runtime context should build");
        assert!(message.content.contains("陪伴模式：开启"));
        assert!(message.content.contains("最近观察：暂无"));
        assert!(message
            .content
            .contains("没有观察且陪伴模式已开启时回答“我还没有观察到屏幕内容”"));
        assert!(message
            .content
            .contains("陪伴模式已开启时，不要说“请开启观察”"));
    }

    #[test]
    fn companion_runtime_state_declares_client_driven_proactive_dialogue() {
        let message = build_runtime_context(&companion(true, None), &privacy(true, false), true)
            .expect("runtime context should build");

        assert!(message.content.contains("客户端会自行定期观察并主动搭话"));
        assert!(message.content.contains("不得声称没有主动发起对话的能力"));
    }

    #[test]
    fn revoking_screen_observation_forces_companion_stop() {
        let before = privacy(true, false);
        let after = privacy(false, false);
        assert!(companion_must_stop_on_privacy_revoke(&before, &after));
        assert!(!companion_must_stop_on_privacy_revoke(&after, &after));
        assert!(!companion_must_stop_on_privacy_revoke(&after, &before));
    }

    #[test]
    fn prompt_placeholders_do_not_echo_final_answer_marker() {
        assert!(!FALLBACK_TOOL_PROTOCOL.contains("最终答案"));
    }

    #[test]
    fn plain_json_requests_force_non_streaming_for_both_protocols() {
        let provider = ModelProviderSettings {
            provider_name: "自定义供应商".to_string(),
            base_url: "https://api.deepseek.com".to_string(),
            protocol: "responses".to_string(),
            model: "deepseek-v4-flash".to_string(),
            monthly_budget_usd: 10.0,
        };
        let messages = vec![Message {
            role: "user".to_string(),
            content: "你好".to_string(),
            created_at: 0,
            source: "chat".to_string(),
        }];

        let responses = build_plain_json_request(&provider, &messages).unwrap();
        assert_eq!(responses.body["stream"], false);
        assert!(responses.url.contains("/responses"));

        let chat_provider = ModelProviderSettings {
            protocol: "chat_completions".to_string(),
            ..provider
        };
        let chat = build_plain_json_request(&chat_provider, &messages).unwrap();
        assert_eq!(chat.body["stream"], false);
        assert!(chat.url.contains("/chat/completions"));
    }

    #[test]
    fn cleaned_normalized_text_strips_wrappers_and_rejects_placeholders() {
        assert_eq!(
            clean_normalized_text("  正文内容  ").as_deref(),
            Some("正文内容")
        );
        assert_eq!(
            clean_normalized_text("```\n\"真正的回复\"\n```").as_deref(),
            Some("真正的回复")
        );
        assert_eq!(clean_normalized_text("<这里填写提取出的正文>"), None);
        assert_eq!(clean_normalized_text(""), None);
        assert_eq!(clean_normalized_text("   "), None);
        assert_eq!(clean_normalized_text("最终答案"), None);
        assert_eq!(
            clean_normalized_text("最终答案：今天天气很好").as_deref(),
            Some("今天天气很好")
        );
    }

    #[test]
    fn cleaned_normalized_text_rejects_instruction_echo() {
        assert_eq!(
            clean_normalized_text(
                "我们需要理解指令。从下面的模型输出中提取真正回复用户的最终正文。"
            ),
            None
        );
        assert_eq!(
            clean_normalized_text("The user is asking me to extract the final body text that actually replies to the user."),
            None
        );
    }

    #[test]
    fn normalized_final_extracts_json_after_reasoning() {
        let mixed = "我们需要理解指令。\n{\"final\":\"观察不了，需要先输入 /陪伴模式\"}";
        assert_eq!(
            extract_normalized_final(mixed).unwrap(),
            "观察不了，需要先输入 /陪伴模式"
        );
    }

    #[test]
    fn normalize_rejects_reasoning_only_reply() {
        let reasoning =
            "我们需要理解指令。用户给出了一段话：\"The tool result says companionship mode is not enabled\"";
        // 纯推理、无 JSON 包裹：不允许作为正文通过。
        assert!(extract_normalized_final(reasoning).is_err());
    }

    #[test]
    fn normalizer_rejects_instruction_echo_instead_of_guessing_the_last_line() {
        // 实测失败样本（id 162 形态）：整理器复述指令 + 分析，正文在最后一行。
        let echo = "The user is asking me to extract the final body text that actually replies to the user. \
            They want me to output only that body text itself — no JSON, no quotes, no code blocks. \
            So the body text is simply: \"我尝试观察了，但屏幕截图或分析失败，暂时无法看到内容，不是权限问题。你可以稍后再让我试一次。\"\n\
            我尝试观察了，但屏幕截图或分析失败，暂时无法看到内容，不是权限问题。你可以稍后再让我试一次。";
        assert_eq!(extract_final_text_from_normalizer(echo), None);
    }

    #[test]
    fn normalizer_rejects_unstructured_reasoning_even_when_it_sounds_like_a_reply() {
        let reasoning =
            "We need to respond to the user after changing movement state. 好的，我会待在原地。";

        assert_eq!(extract_final_text_from_normalizer(reasoning), None);
    }

    #[test]
    fn normalizer_rejects_chinese_reasoning_with_an_unstructured_last_line() {
        // 实测失败样本（id 160 形态）：中文分析在前，正文在末尾。
        let echo = "我们需要理解指令。用户给出了一段话：\"The tool result says companionship mode is not enabled\"。\
            因此输出应为：观察不了，需要先输入 /陪伴模式。\n观察不了，需要先输入 /陪伴模式。";
        assert_eq!(extract_final_text_from_normalizer(echo), None);
    }

    #[test]
    fn normalizer_fallback_rejects_pure_instruction_echo_without_answer() {
        let echo = "The user is asking me to extract the final body text. They want me to output only that body text itself.";
        assert_eq!(extract_final_text_from_normalizer(echo), None);
    }

    #[test]
    fn normalizer_fallback_prefers_structured_json() {
        let reply = "分析略。\n{\"final\":\"明天记得带伞\"}";
        assert_eq!(
            extract_final_text_from_normalizer(reply).as_deref(),
            Some("明天记得带伞")
        );
    }

    #[test]
    fn screen_observation_tool_requires_companion_and_grant() {
        assert!(ensure_screen_observation_allowed(true, true).is_ok());
        assert!(ensure_screen_observation_allowed(false, true).is_err());
        assert!(ensure_screen_observation_allowed(true, false).is_err());
        let message = ensure_screen_observation_allowed(false, true).unwrap_err();
        assert!(message.contains("/陪伴模式"));
    }

    #[test]
    fn halting_companion_deactivates_and_increments_generation() {
        let session = CompanionSession(Mutex::new(CompanionState {
            active: true,
            last_observation: None,
            user_talking_to_cyral: false,
            stop_generation: 0,
            ..CompanionState::default()
        }));
        assert!(companion_chain_alive(&session, 0));

        {
            let mut state = session.0.lock().unwrap();
            halt_companion(&mut state);
        }

        assert!(!session.0.lock().unwrap().active);
        assert_eq!(companion_stop_generation(&session), 1);
        assert!(!companion_chain_alive(&session, 0));
        assert!(!companion_chain_alive(&session, 1));
    }

    #[test]
    fn chain_alive_requires_same_generation_and_active() {
        let session = CompanionSession(Mutex::new(CompanionState {
            active: false,
            last_observation: None,
            user_talking_to_cyral: false,
            stop_generation: 3,
            ..CompanionState::default()
        }));
        assert!(!companion_chain_alive(&session, 3));
    }

    #[test]
    fn agent_tool_registry_includes_screen_observation() {
        let chat = agent_tool_definitions("chat_completions");
        let chat_names: Vec<&str> = chat
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["function"]["name"].as_str())
            .collect();
        assert!(chat_names.contains(&"get_screen_observation"));
        assert!(chat_names.contains(&"get_current_datetime"));
        assert!(chat_names.contains(&"get_weather"));
        assert!(chat_names.contains(&"set_movement_allowed"));

        let responses = agent_tool_definitions("responses");
        let response_names: Vec<&str> = responses
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        assert!(response_names.contains(&"get_screen_observation"));
    }
}
#[cfg(test)]
mod pet_recovery_tests {
    use super::*;

    #[test]
    fn recovery_rehomes_an_offscreen_pet_to_the_current_work_area() {
        assert_eq!(
            restore_position_in_visible_work_area(
                (3_840, 100),
                (300, 260),
                (0, 0),
                (1_920, 1_040),
                24,
            ),
            (1_596, 756),
            "断开副屏后，托盘恢复必须回到主屏可见区域"
        );
    }
}
