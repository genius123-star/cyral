use std::{error::Error, fmt, path::Path};

use rusqlite::{backup::Progress, params, Connection, DatabaseName, OptionalExtension};
use serde::{Deserialize, Serialize};

pub(crate) const CURRENT_SCHEMA_VERSION: i64 = 14;
/// 活动记录保留期：3 天。
pub const ACTIVITY_RETENTION_SECS: i64 = 3 * 86_400;
pub const CYRAL_ACTIVITY_RETENTION_SECS: i64 = 30 * 86_400;
/// 桌面执行审计与旧版浏览器进度数据的保留期：滚动 7 天。
pub const EXECUTION_RETENTION_SECS: i64 = 7 * 86_400;

#[derive(Debug)]
pub(crate) enum DatabaseStartupError {
    Sqlite(rusqlite::Error),
    Integrity(String),
    UnsupportedSchema(i64),
    MigrationRolledBack(String),
    RollbackFailed { migration: String, rollback: String },
}

impl fmt::Display for DatabaseStartupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(error) => write!(formatter, "数据库启动失败：{error}"),
            Self::Integrity(detail) => write!(formatter, "数据库完整性检查失败：{detail}"),
            Self::UnsupportedSchema(version) => write!(
                formatter,
                "数据库版本 {version} 高于当前程序支持的 {CURRENT_SCHEMA_VERSION}"
            ),
            Self::MigrationRolledBack(error) => {
                write!(formatter, "数据库迁移失败，已恢复迁移前备份：{error}")
            }
            Self::RollbackFailed {
                migration,
                rollback,
            } => write!(
                formatter,
                "数据库迁移失败且备份恢复失败；迁移错误：{migration}；恢复错误：{rollback}"
            ),
        }
    }
}

impl Error for DatabaseStartupError {}

impl From<rusqlite::Error> for DatabaseStartupError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

fn verify_database_integrity(connection: &Connection) -> Result<(), DatabaseStartupError> {
    let result =
        connection.query_row("PRAGMA quick_check(1)", [], |row| row.get::<_, String>(0))?;
    if result == "ok" {
        Ok(())
    } else {
        Err(DatabaseStartupError::Integrity(result))
    }
}

fn migration_backup_path(path: &Path, version: i64) -> std::path::PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("cyral.db");
    path.with_file_name(format!("{file_name}.schema-v{version}.bak"))
}

/// 打开真实数据库时先校验完整性；迁移前使用 SQLite 在线备份，迁移失败则原地恢复。
/// 在线备份会合并 WAL，不能用普通文件复制替代。
pub(crate) fn open_database_with_recovery(path: &Path) -> Result<Connection, DatabaseStartupError> {
    let had_database = path
        .metadata()
        .map(|metadata| metadata.len() > 0)
        .unwrap_or(false);
    let mut connection = Connection::open(path)?;
    verify_database_integrity(&connection)?;
    let version =
        connection.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?;
    if version > CURRENT_SCHEMA_VERSION {
        return Err(DatabaseStartupError::UnsupportedSchema(version));
    }

    let backup_path = (had_database && version < CURRENT_SCHEMA_VERSION)
        .then(|| migration_backup_path(path, version));
    if let Some(backup_path) = backup_path.as_ref() {
        connection.backup(DatabaseName::Main, backup_path, None)?;
    }

    if let Err(error) = initialize_database(&connection) {
        let migration = error.to_string();
        if let Some(backup_path) = backup_path {
            return match connection.restore(DatabaseName::Main, backup_path, None::<fn(Progress)>) {
                Ok(()) => Err(DatabaseStartupError::MigrationRolledBack(migration)),
                Err(rollback) => Err(DatabaseStartupError::RollbackFailed {
                    migration,
                    rollback: rollback.to_string(),
                }),
            };
        }
        return Err(DatabaseStartupError::Sqlite(error));
    }
    verify_database_integrity(&connection)?;
    Ok(connection)
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivitySession {
    pub id: i64,
    pub app_name: String,
    pub window_title: Option<String>,
    pub started_at: i64,
    pub ended_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CyralActivityEvent {
    pub id: i64,
    pub event_type: String,
    pub source: String,
    pub app_name: String,
    pub window_title: Option<String>,
    pub summary: String,
    pub observed_at: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionRun {
    pub id: i64,
    pub task_kind: String,
    pub target_label: String,
    pub status: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub error_code: Option<String>,
}

/// 本地聚合出的跨天活动主题。只含窗口标题抽出的稳定标识和真实活动时长。
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityTheme {
    pub identity_key: String,
    pub kind: String,
    pub display_name: String,
    pub active_days: i64,
    pub total_active_secs: i64,
    pub observation_count: i64,
    pub first_seen_at: i64,
    pub last_seen_at: i64,
    pub last_mentioned_at: Option<i64>,
    pub muted_until_at: Option<i64>,
}

/// 可选视觉分析模型供应商。与对话模型独立，未配置时截图分析降级为纯文本。
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VisionProvider {
    pub base_url: String,
    pub protocol: String,
    pub model: String,
    pub enabled: bool,
}

/// 应用设置：开机自启与问候天气的默认城市。
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub autostart: bool,
    pub city: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailAccountInput {
    pub id: Option<i64>,
    pub provider: String,
    pub account_address: String,
    pub display_name: String,
    pub signature: String,
    pub smtp_host: String,
    pub smtp_port: i64,
    pub smtp_tls: String,
    pub imap_host: String,
    pub imap_port: i64,
    pub imap_tls: String,
    pub is_default: bool,
    pub inbox_enabled: bool,
    pub inbox_interval_minutes: i64,
    pub inbox_detail_level: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailAccount {
    pub id: i64,
    pub provider: String,
    pub account_address: String,
    pub display_name: String,
    pub signature: String,
    pub smtp_host: String,
    pub smtp_port: i64,
    pub smtp_tls: String,
    pub imap_host: String,
    pub imap_port: i64,
    pub imap_tls: String,
    pub is_default: bool,
    pub inbox_enabled: bool,
    pub inbox_interval_minutes: i64,
    pub inbox_detail_level: String,
    pub enabled: bool,
}

pub(crate) fn initialize_database(connection: &Connection) -> rusqlite::Result<()> {
    let transaction = connection.unchecked_transaction()?;
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS pet_state (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            locked INTEGER NOT NULL DEFAULT 0,
            x INTEGER,
            y INTEGER,
            last_active_at INTEGER NOT NULL DEFAULT 0
        );
        INSERT OR IGNORE INTO pet_state (id) VALUES (1);",
    )?;
    migrate_database(&transaction)?;
    transaction.commit()
}

fn migrate_database(connection: &Connection) -> rusqlite::Result<()> {
    let version =
        connection.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?;
    if version < 1 {
        initialize_core_tables(connection)?;
        connection.pragma_update(None, "user_version", 1)?;
    }
    if version < 2 {
        add_column_if_missing(connection, "todos", "reminded_at", "INTEGER")?;
        add_column_if_missing(connection, "todos", "snoozed_until", "INTEGER")?;
        connection.pragma_update(None, "user_version", 2)?;
    }
    if version < 3 {
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS activity_sessions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                app_name TEXT NOT NULL,
                window_title TEXT,
                started_at INTEGER NOT NULL,
                ended_at INTEGER CHECK (ended_at IS NULL OR ended_at >= started_at)
            );
            CREATE INDEX IF NOT EXISTS idx_activity_sessions_started_at
                ON activity_sessions(started_at);",
        )?;
        connection.pragma_update(None, "user_version", 3)?;
    }
    if version < 4 {
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS vision_provider (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                base_url TEXT NOT NULL,
                protocol TEXT NOT NULL,
                model TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 0
            );
            INSERT OR IGNORE INTO vision_provider (id, base_url, protocol, model) VALUES (1, '', '', '');",
        )?;
        connection.pragma_update(None, "user_version", 4)?;
    }
    if version < 5 {
        // 双提醒：ddl 前 1 小时与前 10 分钟各一次，独立标记；旧 reminded_at 视为两个点都已发。
        add_column_if_missing(connection, "todos", "reminded_1h_at", "INTEGER")?;
        add_column_if_missing(connection, "todos", "reminded_10m_at", "INTEGER")?;
        connection.execute_batch(
            "UPDATE todos SET reminded_1h_at = reminded_at, reminded_10m_at = reminded_at
             WHERE reminded_at IS NOT NULL;
             CREATE TABLE IF NOT EXISTS app_settings (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                autostart INTEGER NOT NULL DEFAULT 0,
                city TEXT NOT NULL DEFAULT ''
             );
             INSERT OR IGNORE INTO app_settings (id) VALUES (1);",
        )?;
        connection.pragma_update(None, "user_version", 5)?;
    }
    if version < 6 {
        // 开机会话视图：记录本次启动时刻，时间线按它分组；旧库 backfill 为当前时间。
        add_column_if_missing(
            connection,
            "app_settings",
            "boot_started_at",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        connection.execute_batch(
            "UPDATE app_settings SET boot_started_at = strftime('%s','now') WHERE id = 1 AND boot_started_at = 0;",
        )?;
        connection.pragma_update(None, "user_version", 6)?;
    }
    if version < 7 {
        // 费用账本：每次云请求的 token 与估算费用，供月度预算与用量折线图使用。
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS usage_ledger (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                provider_name TEXT NOT NULL DEFAULT '',
                model TEXT NOT NULL DEFAULT '',
                protocol TEXT NOT NULL DEFAULT '',
                prompt_tokens INTEGER NOT NULL DEFAULT 0,
                completion_tokens INTEGER NOT NULL DEFAULT 0,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                est_cost_usd REAL NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_usage_ledger_created_at
                ON usage_ledger(created_at);",
        )?;
        connection.pragma_update(None, "user_version", 7)?;
    }
    if version < 8 {
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS cyral_activity_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                event_type TEXT NOT NULL,
                source TEXT NOT NULL,
                app_name TEXT NOT NULL DEFAULT '',
                window_title TEXT,
                summary TEXT NOT NULL DEFAULT '',
                observed_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_cyral_activity_events_observed_at
                ON cyral_activity_events(observed_at DESC);",
        )?;
        connection.pragma_update(None, "user_version", 8)?;
    }
    if version < 9 {
        // 三档提醒：截止前 2 小时、30 分钟、5 分钟。旧列保留，避免破坏已有本地数据。
        add_column_if_missing(connection, "todos", "reminded_2h_at", "INTEGER")?;
        add_column_if_missing(connection, "todos", "reminded_30m_at", "INTEGER")?;
        add_column_if_missing(connection, "todos", "reminded_5m_at", "INTEGER")?;
        connection.pragma_update(None, "user_version", 9)?;
    }
    if version < 10 {
        add_column_if_missing(
            connection,
            "messages",
            "source",
            "TEXT NOT NULL DEFAULT 'chat'",
        )?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS activity_themes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                identity_key TEXT NOT NULL UNIQUE,
                kind TEXT NOT NULL CHECK (kind IN ('file', 'media')),
                display_name TEXT NOT NULL,
                first_seen_at INTEGER NOT NULL,
                last_seen_at INTEGER NOT NULL,
                last_mentioned_at INTEGER,
                muted_until_at INTEGER
            );
            CREATE TABLE IF NOT EXISTS activity_theme_days (
                theme_id INTEGER NOT NULL REFERENCES activity_themes(id) ON DELETE CASCADE,
                local_day TEXT NOT NULL,
                active_secs INTEGER NOT NULL DEFAULT 0 CHECK (active_secs >= 0),
                observation_count INTEGER NOT NULL DEFAULT 0 CHECK (observation_count >= 0),
                last_observed_at INTEGER NOT NULL,
                PRIMARY KEY(theme_id, local_day)
            );
            CREATE INDEX IF NOT EXISTS idx_activity_themes_last_seen
                ON activity_themes(last_seen_at DESC);",
        )?;
        connection.pragma_update(None, "user_version", 10)?;
    }
    if version < 11 {
        add_column_if_missing(
            connection,
            "privacy_config",
            "proactive_interaction",
            "INTEGER NOT NULL DEFAULT 1",
        )?;
        add_column_if_missing(
            connection,
            "privacy_config",
            "file_search_desktop",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        add_column_if_missing(
            connection,
            "privacy_config",
            "file_search_documents",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        add_column_if_missing(
            connection,
            "privacy_config",
            "file_search_downloads",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        connection.pragma_update(None, "user_version", 11)?;
    }
    if version < 12 {
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS execution_runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                task_kind TEXT NOT NULL,
                target_label TEXT NOT NULL DEFAULT '',
                status TEXT NOT NULL CHECK (
                    status IN ('confirming', 'running', 'waiting_for_user',
                               'completed', 'failed', 'cancelled')
                ),
                started_at INTEGER NOT NULL,
                finished_at INTEGER,
                error_code TEXT,
                CHECK (finished_at IS NULL OR finished_at >= started_at)
            );
            CREATE INDEX IF NOT EXISTS idx_execution_runs_finished_at
                ON execution_runs(finished_at);

            CREATE TABLE IF NOT EXISTS browser_progress (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                site TEXT NOT NULL,
                media_key TEXT NOT NULL,
                title TEXT NOT NULL,
                position_label TEXT NOT NULL,
                page_url TEXT NOT NULL,
                observed_at INTEGER NOT NULL,
                UNIQUE(site, media_key)
            );
            CREATE INDEX IF NOT EXISTS idx_browser_progress_observed_at
                ON browser_progress(observed_at);",
        )?;
        connection.pragma_update(None, "user_version", 12)?;
    }
    if version < 13 {
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS browser_binding (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                instance_id TEXT NOT NULL,
                profile_label TEXT NOT NULL,
                granted_capabilities TEXT NOT NULL DEFAULT '[]',
                paired_at INTEGER NOT NULL,
                last_seen_at INTEGER NOT NULL CHECK (last_seen_at >= paired_at)
            );",
        )?;
        connection.pragma_update(None, "user_version", 13)?;
    }
    if version < 14 {
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS email_accounts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                provider TEXT NOT NULL CHECK (provider IN ('gmail', 'qq', 'netease', 'custom')),
                account_address TEXT NOT NULL,
                display_name TEXT NOT NULL DEFAULT '',
                signature TEXT NOT NULL DEFAULT '',
                smtp_host TEXT NOT NULL DEFAULT '',
                smtp_port INTEGER NOT NULL DEFAULT 465 CHECK (smtp_port BETWEEN 1 AND 65535),
                smtp_tls TEXT NOT NULL DEFAULT 'implicit' CHECK (smtp_tls IN ('implicit', 'starttls')),
                imap_host TEXT NOT NULL DEFAULT '',
                imap_port INTEGER NOT NULL DEFAULT 993 CHECK (imap_port BETWEEN 1 AND 65535),
                imap_tls TEXT NOT NULL DEFAULT 'implicit' CHECK (imap_tls IN ('implicit', 'starttls')),
                is_default INTEGER NOT NULL DEFAULT 0,
                inbox_enabled INTEGER NOT NULL DEFAULT 0,
                inbox_interval_minutes INTEGER NOT NULL DEFAULT 10 CHECK (inbox_interval_minutes IN (5, 10, 30)),
                inbox_detail_level TEXT NOT NULL DEFAULT 'count' CHECK (inbox_detail_level IN ('count', 'metadata')),
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                UNIQUE(provider, account_address)
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_email_accounts_default
                ON email_accounts(is_default) WHERE is_default = 1;
            CREATE TABLE IF NOT EXISTS mailbox_cursors (
                account_id INTEGER PRIMARY KEY REFERENCES email_accounts(id) ON DELETE CASCADE,
                cursor_kind TEXT NOT NULL CHECK (cursor_kind IN ('gmail_history', 'imap_uid')),
                cursor_value TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS mail_notification_receipts (
                account_id INTEGER NOT NULL REFERENCES email_accounts(id) ON DELETE CASCADE,
                message_hash TEXT NOT NULL,
                notified_at INTEGER NOT NULL,
                PRIMARY KEY(account_id, message_hash)
            );
            CREATE INDEX IF NOT EXISTS idx_mail_notification_receipts_time
                ON mail_notification_receipts(notified_at);",
        )?;
        connection.pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION)?;
    }
    Ok(())
}

fn table_exists(connection: &Connection, table: &str) -> rusqlite::Result<bool> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [table],
            |row| row.get::<_, i64>(0),
        )
        .map(|exists| exists != 0)
}
fn table_has_column(connection: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
    for existing in columns {
        if existing? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn add_column_if_missing(
    connection: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> rusqlite::Result<()> {
    if !table_exists(connection, table)? {
        return Ok(());
    }
    if !table_has_column(connection, table, column)? {
        connection.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {definition}"
        ))?;
    }
    Ok(())
}

fn initialize_core_tables(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            role TEXT NOT NULL CHECK (role IN ('user', 'assistant')),
            content TEXT NOT NULL,
            created_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS model_provider (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            provider_name TEXT NOT NULL,
            base_url TEXT NOT NULL,
            protocol TEXT NOT NULL,
            model TEXT NOT NULL,
            monthly_budget_usd REAL NOT NULL CHECK (monthly_budget_usd > 0 AND monthly_budget_usd <= 10)
        );
        CREATE TABLE IF NOT EXISTS memory_atoms (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            kind TEXT NOT NULL,
            content TEXT NOT NULL COLLATE NOCASE UNIQUE,
            source_excerpt TEXT NOT NULL,
            confidence REAL NOT NULL CHECK (confidence >= 0 AND confidence <= 1),
            created_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS todos (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            deadline_local TEXT NOT NULL,
            remind_before_minutes INTEGER NOT NULL CHECK (remind_before_minutes >= 0),
            source_text TEXT NOT NULL,
            status TEXT NOT NULL CHECK (status IN ('draft', 'active', 'completed')),
            created_at INTEGER NOT NULL,
            reminded_at INTEGER,
            snoozed_until INTEGER,
            UNIQUE(source_text, status)
        );
        CREATE TABLE IF NOT EXISTS privacy_config (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            screen_observation INTEGER NOT NULL DEFAULT 0,
            auto_location INTEGER NOT NULL DEFAULT 0
        );
        INSERT OR IGNORE INTO privacy_config (id) VALUES (1);",
    )
}

pub fn start_activity_session(
    connection: &Connection,
    app_name: &str,
    window_title: Option<&str>,
    started_at: i64,
) -> rusqlite::Result<i64> {
    connection.execute(
        "INSERT INTO activity_sessions (app_name, window_title, started_at)
         VALUES (?1, ?2, ?3)",
        params![app_name, window_title, started_at],
    )?;
    Ok(connection.last_insert_rowid())
}

pub fn end_activity_session(
    connection: &Connection,
    id: i64,
    ended_at: i64,
) -> rusqlite::Result<()> {
    connection.execute(
        "UPDATE activity_sessions SET ended_at = ?1 WHERE id = ?2 AND ended_at IS NULL",
        params![ended_at, id],
    )?;
    Ok(())
}

pub fn load_activity_sessions(connection: &Connection) -> rusqlite::Result<Vec<ActivitySession>> {
    let mut statement = connection.prepare(
        "SELECT id, app_name, window_title, started_at, ended_at
         FROM activity_sessions ORDER BY started_at, id",
    )?;
    let sessions = statement
        .query_map([], |row| {
            Ok(ActivitySession {
                id: row.get(0)?,
                app_name: row.get(1)?,
                window_title: row.get(2)?,
                started_at: row.get(3)?,
                ended_at: row.get(4)?,
            })
        })?
        .collect();
    sessions
}

/// 按"开机会话"起始时间加载活动记录（降序，最新在前），并限制条数。
pub fn load_activity_sessions_since(
    connection: &Connection,
    since: i64,
    limit: i64,
) -> rusqlite::Result<Vec<ActivitySession>> {
    let mut statement = connection.prepare(
        "SELECT id, app_name, window_title, started_at, ended_at
         FROM activity_sessions
         WHERE started_at >= ?1
         ORDER BY started_at DESC, id DESC
         LIMIT ?2",
    )?;
    let sessions = statement
        .query_map(params![since, limit], |row| {
            Ok(ActivitySession {
                id: row.get(0)?,
                app_name: row.get(1)?,
                window_title: row.get(2)?,
                started_at: row.get(3)?,
                ended_at: row.get(4)?,
            })
        })?
        .collect();
    sessions
}

/// 删除超过保留期的活动记录（仅已结束的行；进行中的会话保留，避免误删跨时段活动）。
pub fn delete_expired_activity_sessions(
    connection: &Connection,
    now: i64,
    retention_secs: i64,
) -> rusqlite::Result<usize> {
    let cutoff = now.saturating_sub(retention_secs);
    connection.execute(
        "DELETE FROM activity_sessions
         WHERE ended_at IS NOT NULL AND started_at < ?1",
        params![cutoff],
    )
}

/// 删除滚动 7 天窗口之外的已结束执行记录，并清理退役浏览器功能遗留的进度；进行中任务不会被清理。
pub fn delete_expired_execution_data(
    connection: &Connection,
    now: i64,
) -> rusqlite::Result<(usize, usize)> {
    let cutoff = now.saturating_sub(EXECUTION_RETENTION_SECS);
    let transaction = connection.unchecked_transaction()?;
    let deleted_runs = transaction.execute(
        "DELETE FROM execution_runs
         WHERE finished_at IS NOT NULL AND finished_at < ?1",
        params![cutoff],
    )?;
    let deleted_progress = transaction.execute(
        "DELETE FROM browser_progress WHERE observed_at < ?1",
        params![cutoff],
    )?;
    transaction.commit()?;
    Ok((deleted_runs, deleted_progress))
}

pub fn record_execution_started(
    connection: &Connection,
    task_kind: &str,
    target_label: &str,
    started_at: i64,
) -> rusqlite::Result<i64> {
    connection.execute(
        "INSERT INTO execution_runs
            (task_kind, target_label, status, started_at)
         VALUES (?1, ?2, 'running', ?3)",
        params![task_kind, target_label, started_at],
    )?;
    Ok(connection.last_insert_rowid())
}

pub fn finish_execution_run(
    connection: &Connection,
    run_id: i64,
    status: &str,
    error_code: Option<&str>,
    finished_at: i64,
) -> rusqlite::Result<()> {
    let updated = connection.execute(
        "UPDATE execution_runs
         SET status = ?2, error_code = ?3, finished_at = ?4
         WHERE id = ?1",
        params![run_id, status, error_code, finished_at],
    )?;
    if updated == 0 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(())
}

pub fn load_recent_execution_runs(
    connection: &Connection,
    limit: i64,
) -> rusqlite::Result<Vec<ExecutionRun>> {
    let mut statement = connection.prepare(
        "SELECT id, task_kind, target_label, status, started_at, finished_at, error_code
         FROM execution_runs
         ORDER BY started_at DESC, id DESC
         LIMIT ?1",
    )?;
    let runs = statement
        .query_map(params![limit.clamp(1, 200)], |row| {
            Ok(ExecutionRun {
                id: row.get(0)?,
                task_kind: row.get(1)?,
                target_label: row.get(2)?,
                status: row.get(3)?,
                started_at: row.get(4)?,
                finished_at: row.get(5)?,
                error_code: row.get(6)?,
            })
        })?
        .collect();
    runs
}

/// 查询同一应用、同一窗口标题下已结束的历史活动会话（供"旧文件重现"召回）。
/// 只返回已结束的会话，当前正在进行的会话不计入历史。
pub fn past_sessions_for_title(
    connection: &Connection,
    app_name: &str,
    window_title: &str,
    limit: i64,
) -> rusqlite::Result<Vec<ActivitySession>> {
    let mut statement = connection.prepare(
        "SELECT id, app_name, window_title, started_at, ended_at
         FROM activity_sessions
         WHERE app_name = ?1 AND window_title = ?2 AND ended_at IS NOT NULL
         ORDER BY started_at DESC, id DESC
         LIMIT ?3",
    )?;
    let sessions = statement
        .query_map(params![app_name, window_title, limit], |row| {
            Ok(ActivitySession {
                id: row.get(0)?,
                app_name: row.get(1)?,
                window_title: row.get(2)?,
                started_at: row.get(3)?,
                ended_at: row.get(4)?,
            })
        })?
        .collect();
    sessions
}

pub fn record_cyral_activity_event(
    connection: &Connection,
    event_type: &str,
    source: &str,
    app_name: &str,
    window_title: Option<&str>,
    summary: &str,
    observed_at: i64,
) -> rusqlite::Result<i64> {
    connection.execute(
        "INSERT INTO cyral_activity_events
            (event_type, source, app_name, window_title, summary, observed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            event_type,
            source,
            app_name,
            window_title,
            summary,
            observed_at
        ],
    )?;
    Ok(connection.last_insert_rowid())
}

pub fn load_cyral_activity_events(
    connection: &Connection,
    limit: i64,
) -> rusqlite::Result<Vec<CyralActivityEvent>> {
    let mut statement = connection.prepare(
        "SELECT id, event_type, source, app_name, window_title, summary, observed_at
         FROM cyral_activity_events
         ORDER BY observed_at DESC, id DESC
         LIMIT ?1",
    )?;
    let events = statement
        .query_map(params![limit], |row| {
            Ok(CyralActivityEvent {
                id: row.get(0)?,
                event_type: row.get(1)?,
                source: row.get(2)?,
                app_name: row.get(3)?,
                window_title: row.get(4)?,
                summary: row.get(5)?,
                observed_at: row.get(6)?,
            })
        })?
        .collect();
    events
}

pub fn past_cyral_events_for_title(
    connection: &Connection,
    app_name: &str,
    window_title: &str,
    limit: i64,
) -> rusqlite::Result<Vec<CyralActivityEvent>> {
    let mut statement = connection.prepare(
        "SELECT id, event_type, source, app_name, window_title, summary, observed_at
         FROM cyral_activity_events
         WHERE app_name = ?1 AND window_title = ?2
         ORDER BY observed_at DESC, id DESC
         LIMIT ?3",
    )?;
    let events = statement
        .query_map(params![app_name, window_title, limit], |row| {
            Ok(CyralActivityEvent {
                id: row.get(0)?,
                event_type: row.get(1)?,
                source: row.get(2)?,
                app_name: row.get(3)?,
                window_title: row.get(4)?,
                summary: row.get(5)?,
                observed_at: row.get(6)?,
            })
        })?
        .collect();
    events
}

pub fn delete_expired_cyral_activity_events(
    connection: &Connection,
    now: i64,
) -> rusqlite::Result<usize> {
    connection.execute(
        "DELETE FROM cyral_activity_events WHERE observed_at < ?1",
        params![now.saturating_sub(CYRAL_ACTIVITY_RETENTION_SECS)],
    )
}

/// 将一次可确认的主题活动聚合到本地自然日；时长为前台、未锁屏且非空闲状态的真实时长。
pub fn record_activity_theme(
    connection: &Connection,
    identity_key: &str,
    kind: &str,
    display_name: &str,
    occurred_at: i64,
    active_secs: i64,
) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO activity_themes
            (identity_key, kind, display_name, first_seen_at, last_seen_at)
         VALUES (?1, ?2, ?3, ?4, ?4)
         ON CONFLICT(identity_key) DO UPDATE SET
            display_name = excluded.display_name,
            last_seen_at = MAX(activity_themes.last_seen_at, excluded.last_seen_at)",
        params![identity_key, kind, display_name, occurred_at],
    )?;
    let theme_id: i64 = connection.query_row(
        "SELECT id FROM activity_themes WHERE identity_key = ?1",
        [identity_key],
        |row| row.get(0),
    )?;
    connection.execute(
        "INSERT INTO activity_theme_days
            (theme_id, local_day, active_secs, observation_count, last_observed_at)
         VALUES (?1, date(?2, 'unixepoch', 'localtime'), ?3, 1, ?2)
         ON CONFLICT(theme_id, local_day) DO UPDATE SET
            active_secs = activity_theme_days.active_secs + excluded.active_secs,
            observation_count = activity_theme_days.observation_count + 1,
            last_observed_at = MAX(activity_theme_days.last_observed_at, excluded.last_observed_at)",
        params![theme_id, occurred_at, active_secs.max(0)],
    )?;
    Ok(())
}

pub fn load_activity_themes(
    connection: &Connection,
    limit: i64,
) -> rusqlite::Result<Vec<ActivityTheme>> {
    let mut statement = connection.prepare(
        "SELECT t.identity_key, t.kind, t.display_name,
                COUNT(d.local_day), COALESCE(SUM(d.active_secs), 0),
                COALESCE(SUM(d.observation_count), 0),
                t.first_seen_at, t.last_seen_at, t.last_mentioned_at, t.muted_until_at
         FROM activity_themes t
         JOIN activity_theme_days d ON d.theme_id = t.id
         GROUP BY t.id
         ORDER BY t.last_seen_at DESC, t.id DESC
         LIMIT ?1",
    )?;
    let themes = statement
        .query_map(params![limit], |row| {
            Ok(ActivityTheme {
                identity_key: row.get(0)?,
                kind: row.get(1)?,
                display_name: row.get(2)?,
                active_days: row.get(3)?,
                total_active_secs: row.get(4)?,
                observation_count: row.get(5)?,
                first_seen_at: row.get(6)?,
                last_seen_at: row.get(7)?,
                last_mentioned_at: row.get(8)?,
                muted_until_at: row.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(themes)
}

/// 主题需要至少出现于两个自然日、累计 10 分钟真实活动，并且未被用户静默。
pub fn load_eligible_activity_themes(
    connection: &Connection,
    now: i64,
) -> rusqlite::Result<Vec<ActivityTheme>> {
    let recent_cutoff = now.saturating_sub(7 * 86_400);
    let mention_cooldown_cutoff = now.saturating_sub(86_400);
    let mut themes = load_activity_themes(connection, 50)?;
    themes.retain(|theme| {
        theme.active_days >= 2
            && theme.total_active_secs >= 600
            && theme.last_seen_at >= recent_cutoff
            && theme
                .last_mentioned_at
                .is_none_or(|mentioned| mentioned <= mention_cooldown_cutoff)
            && theme.muted_until_at.is_none_or(|until| until <= now)
    });
    Ok(themes)
}

pub fn mark_activity_theme_mentioned(
    connection: &Connection,
    identity_key: &str,
    mentioned_at: i64,
) -> rusqlite::Result<()> {
    connection.execute(
        "UPDATE activity_themes SET last_mentioned_at = ?2 WHERE identity_key = ?1",
        params![identity_key, mentioned_at],
    )?;
    Ok(())
}

pub fn mute_activity_theme(
    connection: &Connection,
    identity_key: &str,
    muted_until_at: i64,
) -> rusqlite::Result<()> {
    connection.execute(
        "UPDATE activity_themes SET muted_until_at = ?2 WHERE identity_key = ?1",
        params![identity_key, muted_until_at],
    )?;
    Ok(())
}

pub fn delete_activity_theme(connection: &Connection, identity_key: &str) -> rusqlite::Result<()> {
    connection.execute(
        "DELETE FROM activity_themes WHERE identity_key = ?1",
        [identity_key],
    )?;
    Ok(())
}

pub fn save_vision_provider(
    connection: &Connection,
    provider: &VisionProvider,
) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO vision_provider (id, base_url, protocol, model, enabled)
         VALUES (1, ?1, ?2, ?3, ?4)
         ON CONFLICT(id) DO UPDATE SET
            base_url = excluded.base_url,
            protocol = excluded.protocol,
            model = excluded.model,
            enabled = excluded.enabled",
        params![
            provider.base_url,
            provider.protocol,
            provider.model,
            provider.enabled as i64
        ],
    )?;
    Ok(())
}

pub fn load_vision_provider(connection: &Connection) -> rusqlite::Result<VisionProvider> {
    connection.query_row(
        "SELECT base_url, protocol, model, enabled FROM vision_provider WHERE id = 1",
        [],
        |row| {
            Ok(VisionProvider {
                base_url: row.get(0)?,
                protocol: row.get(1)?,
                model: row.get(2)?,
                enabled: row.get(3)?,
            })
        },
    )
}

pub fn load_app_settings(connection: &Connection) -> rusqlite::Result<AppSettings> {
    connection.query_row(
        "SELECT autostart, city FROM app_settings WHERE id = 1",
        [],
        |row| {
            Ok(AppSettings {
                autostart: row.get::<_, i64>(0)? != 0,
                city: row.get::<_, String>(1)?,
            })
        },
    )
}

pub fn save_app_settings(connection: &Connection, settings: &AppSettings) -> rusqlite::Result<()> {
    connection.execute(
        "UPDATE app_settings SET autostart = ?1, city = ?2 WHERE id = 1",
        params![if settings.autostart { 1 } else { 0 }, settings.city.trim()],
    )?;
    Ok(())
}

pub fn load_email_accounts(connection: &Connection) -> rusqlite::Result<Vec<EmailAccount>> {
    let mut statement = connection.prepare(
        "SELECT id, provider, account_address, display_name, signature,
                smtp_host, smtp_port, smtp_tls, imap_host, imap_port, imap_tls,
                is_default, inbox_enabled, inbox_interval_minutes, inbox_detail_level, enabled
         FROM email_accounts ORDER BY is_default DESC, id ASC",
    )?;
    let accounts = statement
        .query_map([], |row| {
            Ok(EmailAccount {
                id: row.get(0)?,
                provider: row.get(1)?,
                account_address: row.get(2)?,
                display_name: row.get(3)?,
                signature: row.get(4)?,
                smtp_host: row.get(5)?,
                smtp_port: row.get(6)?,
                smtp_tls: row.get(7)?,
                imap_host: row.get(8)?,
                imap_port: row.get(9)?,
                imap_tls: row.get(10)?,
                is_default: row.get::<_, i64>(11)? != 0,
                inbox_enabled: row.get::<_, i64>(12)? != 0,
                inbox_interval_minutes: row.get(13)?,
                inbox_detail_level: row.get(14)?,
                enabled: row.get::<_, i64>(15)? != 0,
            })
        })?
        .collect();
    accounts
}

pub fn load_email_account(connection: &Connection, id: i64) -> rusqlite::Result<EmailAccount> {
    connection.query_row(
        "SELECT id, provider, account_address, display_name, signature,
                smtp_host, smtp_port, smtp_tls, imap_host, imap_port, imap_tls,
                is_default, inbox_enabled, inbox_interval_minutes, inbox_detail_level, enabled
         FROM email_accounts WHERE id = ?1",
        [id],
        |row| {
            Ok(EmailAccount {
                id: row.get(0)?,
                provider: row.get(1)?,
                account_address: row.get(2)?,
                display_name: row.get(3)?,
                signature: row.get(4)?,
                smtp_host: row.get(5)?,
                smtp_port: row.get(6)?,
                smtp_tls: row.get(7)?,
                imap_host: row.get(8)?,
                imap_port: row.get(9)?,
                imap_tls: row.get(10)?,
                is_default: row.get::<_, i64>(11)? != 0,
                inbox_enabled: row.get::<_, i64>(12)? != 0,
                inbox_interval_minutes: row.get(13)?,
                inbox_detail_level: row.get(14)?,
                enabled: row.get::<_, i64>(15)? != 0,
            })
        },
    )
}

/// 邮箱身份按 ASCII 大小写不敏感匹配；用于在写入前阻止同一服务商账户重复创建。
pub fn find_email_account_id(
    connection: &Connection,
    provider: &str,
    account_address: &str,
) -> rusqlite::Result<Option<i64>> {
    connection
        .query_row(
            "SELECT id FROM email_accounts
             WHERE provider = ?1 COLLATE NOCASE
               AND account_address = ?2 COLLATE NOCASE
             LIMIT 1",
            params![provider.trim(), account_address.trim()],
            |row| row.get(0),
        )
        .optional()
}

pub fn save_email_account(
    connection: &mut Connection,
    account: &EmailAccountInput,
    now: i64,
) -> rusqlite::Result<EmailAccount> {
    let transaction = connection.transaction()?;
    let make_default = account.is_default
        || transaction.query_row("SELECT COUNT(*) = 0 FROM email_accounts", [], |row| {
            row.get::<_, bool>(0)
        })?;
    if make_default {
        transaction.execute("UPDATE email_accounts SET is_default = 0", [])?;
    }
    let id = if let Some(id) = account.id {
        transaction.execute(
            "UPDATE email_accounts SET
                provider = ?1, account_address = ?2, display_name = ?3, signature = ?4,
                smtp_host = ?5, smtp_port = ?6, smtp_tls = ?7,
                imap_host = ?8, imap_port = ?9, imap_tls = ?10,
                is_default = ?11, inbox_enabled = ?12, inbox_interval_minutes = ?13,
                inbox_detail_level = ?14, enabled = ?15, updated_at = ?16
             WHERE id = ?17",
            params![
                account.provider.trim(),
                account.account_address.trim(),
                account.display_name.trim(),
                account.signature.trim(),
                account.smtp_host.trim(),
                account.smtp_port,
                account.smtp_tls.trim(),
                account.imap_host.trim(),
                account.imap_port,
                account.imap_tls.trim(),
                make_default as i64,
                account.inbox_enabled as i64,
                account.inbox_interval_minutes,
                account.inbox_detail_level.trim(),
                account.enabled as i64,
                now,
                id
            ],
        )?;
        id
    } else {
        transaction.execute(
            "INSERT INTO email_accounts (
                provider, account_address, display_name, signature,
                smtp_host, smtp_port, smtp_tls, imap_host, imap_port, imap_tls,
                is_default, inbox_enabled, inbox_interval_minutes, inbox_detail_level,
                enabled, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?16)",
            params![
                account.provider.trim(),
                account.account_address.trim(),
                account.display_name.trim(),
                account.signature.trim(),
                account.smtp_host.trim(),
                account.smtp_port,
                account.smtp_tls.trim(),
                account.imap_host.trim(),
                account.imap_port,
                account.imap_tls.trim(),
                make_default as i64,
                account.inbox_enabled as i64,
                account.inbox_interval_minutes,
                account.inbox_detail_level.trim(),
                account.enabled as i64,
                now
            ],
        )?;
        transaction.last_insert_rowid()
    };
    let saved = transaction.query_row(
        "SELECT id, provider, account_address, display_name, signature,
                smtp_host, smtp_port, smtp_tls, imap_host, imap_port, imap_tls,
                is_default, inbox_enabled, inbox_interval_minutes, inbox_detail_level, enabled
         FROM email_accounts WHERE id = ?1",
        [id],
        |row| {
            Ok(EmailAccount {
                id: row.get(0)?,
                provider: row.get(1)?,
                account_address: row.get(2)?,
                display_name: row.get(3)?,
                signature: row.get(4)?,
                smtp_host: row.get(5)?,
                smtp_port: row.get(6)?,
                smtp_tls: row.get(7)?,
                imap_host: row.get(8)?,
                imap_port: row.get(9)?,
                imap_tls: row.get(10)?,
                is_default: row.get::<_, i64>(11)? != 0,
                inbox_enabled: row.get::<_, i64>(12)? != 0,
                inbox_interval_minutes: row.get(13)?,
                inbox_detail_level: row.get(14)?,
                enabled: row.get::<_, i64>(15)? != 0,
            })
        },
    )?;
    transaction.commit()?;
    Ok(saved)
}

pub fn delete_email_account(connection: &mut Connection, id: i64) -> rusqlite::Result<bool> {
    let transaction = connection.transaction()?;
    transaction.execute("DELETE FROM mailbox_cursors WHERE account_id = ?1", [id])?;
    transaction.execute(
        "DELETE FROM mail_notification_receipts WHERE account_id = ?1",
        [id],
    )?;
    let removed = transaction.execute("DELETE FROM email_accounts WHERE id = ?1", [id])? > 0;
    if removed {
        transaction.execute(
            "UPDATE email_accounts SET is_default = 1
             WHERE id = (SELECT id FROM email_accounts ORDER BY id ASC LIMIT 1)
               AND NOT EXISTS (SELECT 1 FROM email_accounts WHERE is_default = 1)",
            [],
        )?;
    }
    transaction.commit()?;
    Ok(removed)
}

pub fn delete_expired_mail_receipts(connection: &Connection, now: i64) -> rusqlite::Result<usize> {
    connection.execute(
        "DELETE FROM mail_notification_receipts WHERE notified_at < ?1",
        [now - EXECUTION_RETENTION_SECS],
    )
}

pub fn load_mailbox_cursor(
    connection: &Connection,
    account_id: i64,
) -> rusqlite::Result<Option<(String, String)>> {
    connection
        .query_row(
            "SELECT cursor_kind, cursor_value FROM mailbox_cursors WHERE account_id = ?1",
            [account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
}

pub fn save_mailbox_cursor(
    connection: &Connection,
    account_id: i64,
    cursor_kind: &str,
    cursor_value: &str,
    now: i64,
) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO mailbox_cursors (account_id, cursor_kind, cursor_value, updated_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(account_id) DO UPDATE SET
             cursor_kind = excluded.cursor_kind,
             cursor_value = excluded.cursor_value,
             updated_at = excluded.updated_at",
        params![account_id, cursor_kind, cursor_value, now],
    )?;
    Ok(())
}

/// 只保存加盐后的消息标识；返回 true 表示这是首次通知。
pub fn record_mail_notification_once(
    connection: &Connection,
    account_id: i64,
    message_hash: &str,
    now: i64,
) -> rusqlite::Result<bool> {
    Ok(connection.execute(
        "INSERT OR IGNORE INTO mail_notification_receipts
            (account_id, message_hash, notified_at)
         VALUES (?1, ?2, ?3)",
        params![account_id, message_hash, now],
    )? > 0)
}

/// 统计 Cyral 自本地自然日开始后实际观察到的新邮件数量。
/// 这里只读取去重回执，不保存或读取邮件正文、主题和发件人。
pub fn count_mail_notifications_since(
    connection: &Connection,
    account_id: i64,
    since: i64,
) -> rusqlite::Result<u64> {
    connection.query_row(
        "SELECT COUNT(*) FROM mail_notification_receipts
         WHERE account_id = ?1 AND notified_at >= ?2",
        params![account_id, since],
        |row| row.get(0),
    )
}

pub fn load_boot_started_at(connection: &Connection) -> rusqlite::Result<i64> {
    connection.query_row(
        "SELECT boot_started_at FROM app_settings WHERE id = 1",
        [],
        |row| row.get(0),
    )
}

/// 每次应用启动时更新开机会话起始时间。
pub fn set_boot_started_at(connection: &Connection, now: i64) -> rusqlite::Result<()> {
    connection.execute(
        "UPDATE app_settings SET boot_started_at = ?1 WHERE id = 1",
        params![now],
    )?;
    Ok(())
}

/// 一次云请求的用量记录（估算费用；不存请求正文与 Key）。
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecord {
    pub provider_name: String,
    pub model: String,
    pub protocol: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub est_cost_usd: f64,
    pub created_at: i64,
}

pub fn insert_usage_record(connection: &Connection, record: &UsageRecord) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO usage_ledger
            (provider_name, model, protocol, prompt_tokens, completion_tokens,
             total_tokens, est_cost_usd, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            record.provider_name,
            record.model,
            record.protocol,
            record.prompt_tokens,
            record.completion_tokens,
            record.total_tokens,
            record.est_cost_usd,
            record.created_at,
        ],
    )?;
    Ok(())
}

/// 指定自然月（本地时区，形如 2026-08）的 token 总量与估算费用。
pub fn monthly_usage(connection: &Connection, month: &str) -> rusqlite::Result<(i64, f64)> {
    connection.query_row(
        "SELECT COALESCE(SUM(total_tokens), 0), COALESCE(SUM(est_cost_usd), 0)
         FROM usage_ledger
         WHERE strftime('%Y-%m', created_at, 'unixepoch', 'localtime') = ?1",
        params![month],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
}

/// 近 N 天（本地日期）每日 token 消耗，供折线图使用；按日期升序返回。
pub fn daily_token_usage(
    connection: &Connection,
    days: i64,
    now: i64,
) -> rusqlite::Result<Vec<(String, i64)>> {
    let since = now.saturating_sub(days * 86_400);
    let mut statement = connection.prepare(
        "SELECT date(created_at, 'unixepoch', 'localtime') AS day, SUM(total_tokens)
         FROM usage_ledger
         WHERE created_at >= ?1
         GROUP BY day
         ORDER BY day ASC",
    )?;
    let rows = statement
        .query_map(params![since], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_database_path(test_name: &str) -> std::path::PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "cyral-storage-{test_name}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        directory.join("cyral.db")
    }

    #[test]
    fn records_an_activity_session_with_exact_duration() {
        let connection = Connection::open_in_memory().unwrap();
        initialize_database(&connection).unwrap();

        let id =
            start_activity_session(&connection, "WINWORD.EXE", Some("论文.docx"), 100).unwrap();
        end_activity_session(&connection, id, 183).unwrap();

        let sessions = load_activity_sessions(&connection).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].app_name, "WINWORD.EXE");
        assert_eq!(sessions[0].window_title.as_deref(), Some("论文.docx"));
        assert_eq!(sessions[0].ended_at.unwrap() - sessions[0].started_at, 83);
    }

    #[test]
    fn finds_past_sessions_for_the_same_title() {
        let connection = Connection::open_in_memory().unwrap();
        initialize_database(&connection).unwrap();

        let first = start_activity_session(
            &connection,
            "POWERPNT.EXE",
            Some("报告.pptx - PowerPoint"),
            100,
        )
        .unwrap();
        end_activity_session(&connection, first, 200).unwrap();

        // 不同标题不命中。
        let other = start_activity_session(
            &connection,
            "POWERPNT.EXE",
            Some("别的.pptx - PowerPoint"),
            300,
        )
        .unwrap();
        end_activity_session(&connection, other, 400).unwrap();

        // 未结束的当前会话不计入历史。
        let _open = start_activity_session(
            &connection,
            "POWERPNT.EXE",
            Some("报告.pptx - PowerPoint"),
            500,
        )
        .unwrap();

        let past =
            past_sessions_for_title(&connection, "POWERPNT.EXE", "报告.pptx - PowerPoint", 10)
                .unwrap();
        assert_eq!(past.len(), 1);
        assert_eq!(
            past[0].window_title.as_deref(),
            Some("报告.pptx - PowerPoint")
        );
        assert_eq!(past[0].started_at, 100);
        assert_eq!(past[0].ended_at, Some(200));
    }

    #[test]
    fn aggregates_a_file_theme_across_two_local_days() {
        let connection = Connection::open_in_memory().unwrap();
        initialize_database(&connection).unwrap();

        record_activity_theme(
            &connection,
            "file:POWERPNT.EXE:报告.pptx",
            "file",
            "报告.pptx",
            1_800_000_000,
            900,
        )
        .unwrap();
        record_activity_theme(
            &connection,
            "file:POWERPNT.EXE:报告.pptx",
            "file",
            "报告.pptx",
            1_800_086_400,
            1_200,
        )
        .unwrap();

        let themes = load_eligible_activity_themes(&connection, 1_800_172_800).unwrap();
        assert_eq!(themes.len(), 1);
        assert_eq!(themes[0].display_name, "报告.pptx");
        assert_eq!(themes[0].active_days, 2);
        assert_eq!(themes[0].total_active_secs, 2_100);
        mark_activity_theme_mentioned(&connection, "file:POWERPNT.EXE:报告.pptx", 1_800_172_799)
            .unwrap();
        assert!(load_eligible_activity_themes(&connection, 1_800_172_800)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn muted_theme_is_not_eligible_for_proactive_chat() {
        let connection = Connection::open_in_memory().unwrap();
        initialize_database(&connection).unwrap();
        record_activity_theme(
            &connection,
            "file:WINWORD.EXE:论文.docx",
            "file",
            "论文.docx",
            1_800_000_000,
            900,
        )
        .unwrap();
        record_activity_theme(
            &connection,
            "file:WINWORD.EXE:论文.docx",
            "file",
            "论文.docx",
            1_800_086_400,
            900,
        )
        .unwrap();
        mute_activity_theme(&connection, "file:WINWORD.EXE:论文.docx", 1_800_200_000).unwrap();

        assert!(load_eligible_activity_themes(&connection, 1_800_172_800)
            .unwrap()
            .is_empty());
    }
    #[test]
    fn v5_migration_adds_dual_reminder_columns_and_backfills() {
        let connection = Connection::open_in_memory().unwrap();
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
                    reminded_at INTEGER,
                    snoozed_until INTEGER
                );
                INSERT INTO todos (title, deadline_local, remind_before_minutes, source_text, status, created_at, reminded_at)
                VALUES ('交报告', '2026-08-20T09:00', 60, '测试', 'active', 100, 123);
                PRAGMA user_version = 4;",
            )
            .unwrap();

        migrate_database(&connection).unwrap();

        let (reminded_1h_at, reminded_10m_at): (Option<i64>, Option<i64>) = connection
            .query_row(
                "SELECT reminded_1h_at, reminded_10m_at FROM todos WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(reminded_1h_at, Some(123));
        assert_eq!(reminded_10m_at, Some(123));

        let settings_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM app_settings WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(settings_count, 1);
    }

    #[test]
    fn v6_migration_adds_boot_started_at_and_backfills() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE app_settings (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    autostart INTEGER NOT NULL DEFAULT 0,
                    city TEXT NOT NULL DEFAULT ''
                );
                INSERT INTO app_settings (id) VALUES (1);
                PRAGMA user_version = 5;",
            )
            .unwrap();

        migrate_database(&connection).unwrap();

        let boot_started_at: i64 = connection
            .query_row(
                "SELECT boot_started_at FROM app_settings WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(boot_started_at > 0, "旧库应 backfill 为当前时间");
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            CURRENT_SCHEMA_VERSION
        );
    }

    #[test]
    fn v11_migration_keeps_proactive_enabled_and_file_search_disabled() {
        let connection = Connection::open_in_memory().unwrap();
        initialize_database(&connection).unwrap();

        let columns: Vec<String> = connection
            .prepare("PRAGMA table_info(privacy_config)")
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        for expected in [
            "proactive_interaction",
            "file_search_desktop",
            "file_search_documents",
            "file_search_downloads",
        ] {
            assert!(
                columns.contains(&expected.to_string()),
                "missing {expected}"
            );
        }

        let permissions: (i64, i64, i64, i64) = connection
            .query_row(
                "SELECT proactive_interaction, file_search_desktop,
                        file_search_documents, file_search_downloads
                 FROM privacy_config WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(permissions, (1, 0, 0, 0));
    }
    #[test]
    fn retention_deletes_only_ended_sessions_older_than_three_days() {
        let connection = Connection::open_in_memory().unwrap();
        initialize_database(&connection).unwrap();
        let now = 1_800_000_000i64;
        let retention = 3 * 86_400;

        let old_ended =
            start_activity_session(&connection, "OLD.EXE", None, now - retention - 100).unwrap();
        end_activity_session(&connection, old_ended, now - retention).unwrap();
        let _old_open =
            start_activity_session(&connection, "OLDOPEN.EXE", None, now - retention - 100)
                .unwrap();
        // 进行中的旧会话不删除。
        let recent_ended =
            start_activity_session(&connection, "RECENT.EXE", None, now - 1000).unwrap();
        end_activity_session(&connection, recent_ended, now).unwrap();

        let deleted = delete_expired_activity_sessions(&connection, now, retention).unwrap();

        assert_eq!(deleted, 1);
        let remaining: i64 = connection
            .query_row("SELECT COUNT(*) FROM activity_sessions", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(remaining, 2);
    }

    #[test]
    fn sessions_since_filters_by_boot_time_and_limits() {
        let connection = Connection::open_in_memory().unwrap();
        initialize_database(&connection).unwrap();
        let boot = 1_750_000_000i64;
        start_activity_session(&connection, "OLD.EXE", None, boot - 100).unwrap();
        let new_id = start_activity_session(&connection, "NEW.EXE", None, boot + 100).unwrap();
        let new_id2 = start_activity_session(&connection, "NEW2.EXE", None, boot + 200).unwrap();

        let sessions = load_activity_sessions_since(&connection, boot, 10).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, new_id2); // 降序，最新在前。
        assert_eq!(sessions[1].id, new_id);

        let limited = load_activity_sessions_since(&connection, boot, 1).unwrap();
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].id, new_id2);
    }

    #[test]
    fn retention_window_is_three_days() {
        assert_eq!(ACTIVITY_RETENTION_SECS, 3 * 86_400);
    }

    #[test]
    fn cyral_events_are_newest_first_and_keep_their_observation_source() {
        let connection = Connection::open_in_memory().unwrap();
        initialize_database(&connection).unwrap();
        record_cyral_activity_event(
            &connection,
            "screen_observation",
            "window",
            "CODE.EXE",
            Some("Cyral"),
            "正在编辑项目",
            100,
        )
        .unwrap();
        record_cyral_activity_event(
            &connection,
            "screen_observation",
            "visual",
            "WINWORD.EXE",
            Some("论文.docx"),
            "正在修改结论",
            200,
        )
        .unwrap();

        let events = load_cyral_activity_events(&connection, 10).unwrap();

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].app_name, "WINWORD.EXE");
        assert_eq!(events[0].source, "visual");
        assert_eq!(events[0].summary, "正在修改结论");
    }

    #[test]
    fn cyral_event_retention_is_thirty_days() {
        let connection = Connection::open_in_memory().unwrap();
        initialize_database(&connection).unwrap();
        let now = 1_800_000_000i64;
        for observed_at in [
            now - CYRAL_ACTIVITY_RETENTION_SECS - 1,
            now - CYRAL_ACTIVITY_RETENTION_SECS,
        ] {
            record_cyral_activity_event(
                &connection,
                "screen_observation",
                "window",
                "CODE.EXE",
                None,
                "正在编辑项目",
                observed_at,
            )
            .unwrap();
        }

        let deleted = delete_expired_cyral_activity_events(&connection, now).unwrap();

        assert_eq!(deleted, 1);
        assert_eq!(
            load_cyral_activity_events(&connection, 10).unwrap().len(),
            1
        );
    }

    #[test]
    fn v7_migration_creates_usage_ledger_and_index() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE app_settings (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    autostart INTEGER NOT NULL DEFAULT 0,
                    city TEXT NOT NULL DEFAULT '',
                    boot_started_at INTEGER NOT NULL DEFAULT 0
                );
                INSERT INTO app_settings (id) VALUES (1);
                PRAGMA user_version = 6;",
            )
            .unwrap();

        migrate_database(&connection).unwrap();

        let table_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'usage_ledger'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table_count, 1);
        let index_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_usage_ledger_created_at'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(index_count, 1);
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            CURRENT_SCHEMA_VERSION
        );
    }

    #[test]
    fn v8_migration_creates_cyral_activity_events_without_deleting_legacy_sessions() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE activity_sessions (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    app_name TEXT NOT NULL,
                    window_title TEXT,
                    started_at INTEGER NOT NULL,
                    ended_at INTEGER
                );
                INSERT INTO activity_sessions (app_name, window_title, started_at, ended_at)
                VALUES ('WINWORD.EXE', '论文.docx', 100, 200);
                PRAGMA user_version = 7;",
            )
            .unwrap();

        migrate_database(&connection).unwrap();

        let event_table: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'cyral_activity_events'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let legacy_rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM activity_sessions", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(event_table, 1);
        assert_eq!(legacy_rows, 1, "旧活动记录保持休眠，不静默删除");
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            CURRENT_SCHEMA_VERSION
        );
    }

    #[test]
    fn monthly_usage_sums_only_the_requested_month() {
        let connection = Connection::open_in_memory().unwrap();
        initialize_database(&connection).unwrap();

        // 2026-08-01T00:00:00 本地时间（UTC+8）对应的 unix 秒。
        let aug_first = 1_785_513_600i64;
        // 2026-09-01 本地时间。
        let sep_first = aug_first + 31 * 86_400;
        for (index, (timestamp, tokens, cost)) in [
            (aug_first + 3_600, 1000, 0.001),
            (aug_first + 7_200, 2000, 0.002),
            (sep_first + 3_600, 9000, 0.009),
        ]
        .into_iter()
        .enumerate()
        {
            insert_usage_record(
                &connection,
                &UsageRecord {
                    provider_name: "deepseek".to_string(),
                    model: "deepseek-v4-flash".to_string(),
                    protocol: "responses".to_string(),
                    prompt_tokens: tokens / 2,
                    completion_tokens: tokens / 2,
                    total_tokens: tokens,
                    est_cost_usd: cost,
                    created_at: timestamp,
                },
            )
            .unwrap();
            let _ = index;
        }

        let (aug_tokens, aug_cost) = monthly_usage(&connection, "2026-08").unwrap();
        assert_eq!(aug_tokens, 3000);
        assert!((aug_cost - 0.003).abs() < 1e-9);
        let (sep_tokens, sep_cost) = monthly_usage(&connection, "2026-09").unwrap();
        assert_eq!(sep_tokens, 9000);
        assert!((sep_cost - 0.009).abs() < 1e-9);
        let (empty_tokens, empty_cost) = monthly_usage(&connection, "2026-07").unwrap();
        assert_eq!((empty_tokens, empty_cost), (0, 0.0));
    }

    #[test]
    fn daily_token_usage_groups_by_local_date_and_filters_by_window() {
        let connection = Connection::open_in_memory().unwrap();
        initialize_database(&connection).unwrap();

        let now = 1_785_513_600i64; // 2026-08-01T00:00:00 本地（UTC+8）。
        for (index, (timestamp, tokens)) in [
            (now + 60, 100),
            (now + 3_600, 200),
            (now + 86_400 + 60, 300),
            (now - 40 * 86_400, 999_999), // 超出 30 天窗口。
        ]
        .into_iter()
        .enumerate()
        {
            insert_usage_record(
                &connection,
                &UsageRecord {
                    provider_name: String::new(),
                    model: String::new(),
                    protocol: String::new(),
                    prompt_tokens: tokens,
                    completion_tokens: 0,
                    total_tokens: tokens,
                    est_cost_usd: 0.0,
                    created_at: timestamp,
                },
            )
            .unwrap();
            let _ = index;
        }

        let days = daily_token_usage(&connection, 30, now).unwrap();
        assert_eq!(days.len(), 2);
        assert_eq!(days[0], ("2026-08-01".to_string(), 300));
        assert_eq!(days[1], ("2026-08-02".to_string(), 300));
        assert!(days.windows(2).all(|pair| pair[0].0 < pair[1].0));
    }

    #[test]
    fn legacy_v11_migration_adds_execution_tables_without_touching_existing_data() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE existing_user_data (
                    id INTEGER PRIMARY KEY,
                    value TEXT NOT NULL
                );
                INSERT INTO existing_user_data (id, value) VALUES (1, 'keep-me');
                PRAGMA user_version = 11;",
            )
            .unwrap();

        migrate_database(&connection).unwrap();

        assert!(table_exists(&connection, "execution_runs").unwrap());
        assert!(table_exists(&connection, "browser_progress").unwrap());
        assert_eq!(
            connection
                .query_row(
                    "SELECT value FROM existing_user_data WHERE id = 1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "keep-me"
        );
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            CURRENT_SCHEMA_VERSION
        );
    }

    #[test]
    fn execution_audit_schema_has_no_message_or_file_content_columns() {
        let connection = Connection::open_in_memory().unwrap();
        initialize_database(&connection).unwrap();

        let columns: Vec<String> = connection
            .prepare("PRAGMA table_info(execution_runs)")
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();

        for forbidden in [
            "recipient",
            "email_address",
            "subject",
            "body",
            "file_content",
            "page_content",
        ] {
            assert!(!columns.iter().any(|column| column == forbidden));
        }
    }

    #[test]
    fn execution_data_retention_is_exactly_seven_days() {
        assert_eq!(EXECUTION_RETENTION_SECS, 7 * 86_400);

        let connection = Connection::open_in_memory().unwrap();
        initialize_database(&connection).unwrap();
        let now = 1_800_000_000i64;
        let cutoff = now - EXECUTION_RETENTION_SECS;

        connection
            .execute(
                "INSERT INTO execution_runs
                    (task_kind, target_label, status, started_at, finished_at)
                 VALUES ('open_app', '计算器', 'completed', ?1, ?2)",
                params![cutoff - 20, cutoff - 1],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO execution_runs
                    (task_kind, target_label, status, started_at, finished_at)
                 VALUES ('open_file', '课程作业.pptx', 'completed', ?1, ?2)",
                params![cutoff - 20, cutoff],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO execution_runs
                    (task_kind, target_label, status, started_at, finished_at)
                 VALUES ('gmail_draft', 'Gmail 草稿', 'running', ?1, NULL)",
                params![cutoff - 100],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO browser_progress
                    (site, media_key, title, position_label, page_url, observed_at)
                 VALUES ('4kvm.top', '漫长的季节', '漫长的季节', '第 6 集',
                         'https://www.4kvm.top/', ?1)",
                params![cutoff - 1],
            )
            .unwrap();

        let deleted = delete_expired_execution_data(&connection, now).unwrap();
        assert_eq!(deleted, (1, 1));

        let remaining_runs: i64 = connection
            .query_row("SELECT COUNT(*) FROM execution_runs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(remaining_runs, 2, "边界记录和进行中任务必须保留");
        let remaining_progress: i64 = connection
            .query_row("SELECT COUNT(*) FROM browser_progress", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(remaining_progress, 0);
    }

    #[test]
    fn execution_run_records_only_status_and_non_sensitive_target_label() {
        let connection = Connection::open_in_memory().unwrap();
        initialize_database(&connection).unwrap();

        let run_id =
            record_execution_started(&connection, "open_file", "课程作业.pptx", 1_800_000_000)
                .unwrap();
        finish_execution_run(&connection, run_id, "completed", None, 1_800_000_002).unwrap();

        let runs = load_recent_execution_runs(&connection, 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, run_id);
        assert_eq!(runs[0].task_kind, "open_file");
        assert_eq!(runs[0].target_label, "课程作业.pptx");
        assert_eq!(runs[0].status, "completed");
        assert_eq!(runs[0].started_at, 1_800_000_000);
        assert_eq!(runs[0].finished_at, Some(1_800_000_002));
        assert_eq!(runs[0].error_code, None);
    }

    #[test]
    fn execution_run_rejects_unknown_terminal_status() {
        let connection = Connection::open_in_memory().unwrap();
        initialize_database(&connection).unwrap();
        let run_id =
            record_execution_started(&connection, "open_app", "计算器", 1_800_000_000).unwrap();

        let error = finish_execution_run(
            &connection,
            run_id,
            "pretended_success",
            None,
            1_800_000_001,
        )
        .expect_err("未知状态不得写入审计记录");
        assert!(error.to_string().contains("CHECK constraint failed"));
    }

    #[test]
    fn file_database_creates_a_consistent_backup_before_migration() {
        let path = temporary_database_path("migration-backup");
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE existing_user_data (value TEXT NOT NULL);
                     INSERT INTO existing_user_data (value) VALUES ('keep-me');
                     PRAGMA user_version = 12;",
                )
                .unwrap();
        }

        let connection = open_database_with_recovery(&path).unwrap();
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        drop(connection);

        let backup_path = migration_backup_path(&path, 12);
        let backup = Connection::open(&backup_path).unwrap();
        assert_eq!(
            backup
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            12
        );
        assert_eq!(
            backup
                .query_row("SELECT value FROM existing_user_data", [], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap(),
            "keep-me"
        );
        drop(backup);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn failed_migration_restores_the_pre_migration_database() {
        let path = temporary_database_path("migration-rollback");
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE existing_user_data (value TEXT NOT NULL);
                     INSERT INTO existing_user_data (value) VALUES ('keep-me');
                     CREATE INDEX browser_binding ON existing_user_data(value);
                     PRAGMA user_version = 12;",
                )
                .unwrap();
        }

        let error = open_database_with_recovery(&path).expect_err("迁移冲突必须停止启动");
        assert!(matches!(
            error,
            DatabaseStartupError::MigrationRolledBack(_)
        ));

        let restored = Connection::open(&path).unwrap();
        assert_eq!(
            restored
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            12
        );
        assert_eq!(
            restored
                .query_row("SELECT value FROM existing_user_data", [], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap(),
            "keep-me"
        );
        drop(restored);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn v14_email_schema_stores_configuration_without_credentials_or_message_content() {
        let mut connection = Connection::open_in_memory().unwrap();
        initialize_database(&connection).unwrap();
        let saved = save_email_account(
            &mut connection,
            &EmailAccountInput {
                id: None,
                provider: "qq".to_string(),
                account_address: "sender@qq.com".to_string(),
                display_name: "Cyral User".to_string(),
                signature: "Sent with Cyral".to_string(),
                smtp_host: "smtp.qq.com".to_string(),
                smtp_port: 465,
                smtp_tls: "implicit".to_string(),
                imap_host: "imap.qq.com".to_string(),
                imap_port: 993,
                imap_tls: "implicit".to_string(),
                is_default: true,
                inbox_enabled: true,
                inbox_interval_minutes: 10,
                inbox_detail_level: "count".to_string(),
                enabled: true,
            },
            1_800_000_000,
        )
        .unwrap();
        assert!(saved.is_default);
        assert_eq!(load_email_accounts(&connection).unwrap(), vec![saved]);

        let columns: Vec<String> = connection
            .prepare("PRAGMA table_info(email_accounts)")
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        for forbidden in [
            "password",
            "authorization_code",
            "refresh_token",
            "recipient",
            "subject",
            "body",
            "message_id",
        ] {
            assert!(!columns.iter().any(|column| column == forbidden));
        }
    }

    #[test]
    fn email_identity_lookup_finds_existing_account_case_insensitively() {
        let mut connection = Connection::open_in_memory().unwrap();
        initialize_database(&connection).unwrap();
        let saved = save_email_account(
            &mut connection,
            &EmailAccountInput {
                id: None,
                provider: "gmail".to_string(),
                account_address: "lcr20041104a@gmail.com".to_string(),
                display_name: String::new(),
                signature: String::new(),
                smtp_host: String::new(),
                smtp_port: 465,
                smtp_tls: "implicit".to_string(),
                imap_host: String::new(),
                imap_port: 993,
                imap_tls: "implicit".to_string(),
                is_default: true,
                inbox_enabled: true,
                inbox_interval_minutes: 10,
                inbox_detail_level: "count".to_string(),
                enabled: true,
            },
            1_800_000_000,
        )
        .unwrap();

        assert_eq!(
            find_email_account_id(&connection, "GMAIL", "LCR20041104A@GMAIL.COM").unwrap(),
            Some(saved.id)
        );
        assert_eq!(
            find_email_account_id(&connection, "gmail", "other@gmail.com").unwrap(),
            None
        );
    }

    #[test]
    fn mail_notification_receipts_expire_after_exactly_seven_days() {
        let connection = Connection::open_in_memory().unwrap();
        initialize_database(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO email_accounts
                    (provider, account_address, created_at, updated_at)
                 VALUES ('gmail', 'sender@gmail.com', 1, 1)",
                [],
            )
            .unwrap();
        let id = connection.last_insert_rowid();
        let now = 1_800_000_000;
        let cutoff = now - EXECUTION_RETENTION_SECS;
        connection
            .execute(
                "INSERT INTO mail_notification_receipts (account_id, message_hash, notified_at)
                 VALUES (?1, 'expired', ?2), (?1, 'boundary', ?3)",
                params![id, cutoff - 1, cutoff],
            )
            .unwrap();

        assert_eq!(delete_expired_mail_receipts(&connection, now).unwrap(), 1);
        assert_eq!(
            connection
                .query_row(
                    "SELECT message_hash FROM mail_notification_receipts",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "boundary"
        );
    }

    #[test]
    fn mailbox_cursor_survives_replacement_and_receipts_deduplicate() {
        let connection = Connection::open_in_memory().unwrap();
        initialize_database(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO email_accounts
                    (provider, account_address, created_at, updated_at)
                 VALUES ('qq', 'sender@qq.com', 1, 1)",
                [],
            )
            .unwrap();
        let id = connection.last_insert_rowid();
        save_mailbox_cursor(&connection, id, "imap_uid", "9:20", 100).unwrap();
        save_mailbox_cursor(&connection, id, "imap_uid", "9:24", 120).unwrap();
        assert_eq!(
            load_mailbox_cursor(&connection, id).unwrap(),
            Some(("imap_uid".to_string(), "9:24".to_string()))
        );
        assert!(record_mail_notification_once(&connection, id, "salted-hash", 120).unwrap());
        assert!(!record_mail_notification_once(&connection, id, "salted-hash", 121).unwrap());
        assert!(record_mail_notification_once(&connection, id, "today-2", 130).unwrap());
        assert_eq!(
            count_mail_notifications_since(&connection, id, 121).unwrap(),
            1
        );
        assert_eq!(
            count_mail_notifications_since(&connection, id, 120).unwrap(),
            2
        );
    }
}
