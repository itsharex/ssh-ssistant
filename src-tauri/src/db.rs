use crate::models::{
    AIConfig, AppSettings, Connection as SshConnection, ConnectionGroup, ConnectionTimeoutSettings,
    FileManagerSettings, HeartbeatSettings, NetworkAdaptiveSettings, PoolHealthSettings, ReconnectSettings, SshKey, SshPoolSettings, TerminalAppearanceSettings,
};
use rusqlite::{params, Connection, Result};
use tauri::{AppHandle, Manager};

pub fn get_db_path(app_handle: &AppHandle) -> std::path::PathBuf {
    let app_dir = app_handle
        .path()
        .app_data_dir()
        .expect("failed to get app data dir");
    if !app_dir.exists() {
        std::fs::create_dir_all(&app_dir).expect("failed to create app data dir");
    }
    app_dir.join("ssh_assistant.db")
}

pub fn init_db(app_handle: &AppHandle) -> Result<()> {
    let db_path = get_db_path(app_handle);
    let conn = Connection::open(db_path)?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS connections (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            host TEXT NOT NULL,
            port INTEGER NOT NULL,
            username TEXT NOT NULL,
            password TEXT
        )",
        [],
    )?;

    conn.execute(
        r#"CREATE TABLE IF NOT EXISTS settings (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            theme TEXT NOT NULL DEFAULT 'dark',
            language TEXT NOT NULL DEFAULT 'zh',
            ai_api_url TEXT NOT NULL DEFAULT 'https://api.openai.com/v1',
            ai_api_key TEXT NOT NULL DEFAULT '',
            ai_model_name TEXT NOT NULL DEFAULT 'gpt-3.5-turbo',
            terminal_font_size INTEGER NOT NULL DEFAULT 14,
            terminal_font_family TEXT NOT NULL DEFAULT 'Menlo, Monaco, "Courier New", monospace',
            terminal_cursor_style TEXT NOT NULL DEFAULT 'block',
            terminal_line_height REAL NOT NULL DEFAULT 1.0
        )"#,
        [],
    )?;

    // Ensure default row exists
    conn.execute("INSERT OR IGNORE INTO settings (id) VALUES (1)", [])?;

    // Migrations: Add jump host columns if they don't exist
    let _ = conn.execute("ALTER TABLE connections ADD COLUMN jump_host TEXT", []);
    let _ = conn.execute("ALTER TABLE connections ADD COLUMN jump_port INTEGER", []);
    let _ = conn.execute("ALTER TABLE connections ADD COLUMN jump_username TEXT", []);
    let _ = conn.execute("ALTER TABLE connections ADD COLUMN jump_password TEXT", []);

    // Migrations: Add terminal appearance columns if they don't exist
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN terminal_font_size INTEGER NOT NULL DEFAULT 14"#,
        [],
    );
    let _ = conn.execute(r#"ALTER TABLE settings ADD COLUMN terminal_font_family TEXT NOT NULL DEFAULT 'Menlo, Monaco, "Courier New", monospace'"#, []);
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN terminal_cursor_style TEXT NOT NULL DEFAULT 'block'"#,
        [],
    );
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN terminal_line_height REAL NOT NULL DEFAULT 1.0"#,
        [],
    );

    // Migration: Add file manager view mode
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN file_manager_view_mode TEXT NOT NULL DEFAULT 'flat'"#,
        [],
    );

    // Migration: Add SSH pool settings
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN ssh_max_background_sessions INTEGER NOT NULL DEFAULT 10"#,
        [],
    );
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN ssh_enable_auto_cleanup INTEGER NOT NULL DEFAULT 1"#,
        [],
    );
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN ssh_cleanup_interval_minutes INTEGER NOT NULL DEFAULT 5"#,
        [],
    );

    // Migration: Add SFTP buffer size
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN file_manager_sftp_buffer_size INTEGER NOT NULL DEFAULT 512"#,
        [],
    );

    // Migration: Add connection timeout settings
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN connection_timeout_secs INTEGER NOT NULL DEFAULT 15"#,
        [],
    );
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN jump_host_timeout_secs INTEGER NOT NULL DEFAULT 30"#,
        [],
    );
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN local_forward_timeout_secs INTEGER NOT NULL DEFAULT 10"#,
        [],
    );
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN command_timeout_secs INTEGER NOT NULL DEFAULT 30"#,
        [],
    );
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN sftp_operation_timeout_secs INTEGER NOT NULL DEFAULT 60"#,
        [],
    );

    // Groups table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS connection_groups (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            parent_id INTEGER,
            FOREIGN KEY(parent_id) REFERENCES connection_groups(id) ON DELETE CASCADE
        )",
        [],
    )?;

    // Migration: Add group_id to connections
    let _ = conn.execute("ALTER TABLE connections ADD COLUMN group_id INTEGER REFERENCES connection_groups(id) ON DELETE SET NULL", []);

    // Migration: Add os_type to connections with default 'Linux'
    let _ = conn.execute(
        "ALTER TABLE connections ADD COLUMN os_type TEXT NOT NULL DEFAULT 'Linux'",
        [],
    );

    // --- SSH Keys Support ---

    // Create ssh_keys table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ssh_keys (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            content TEXT NOT NULL,
            passphrase TEXT,
            created_at INTEGER NOT NULL
        )",
        [],
    )?;

    // Add auth_type and ssh_key_id to connections
    let _ = conn.execute(
        "ALTER TABLE connections ADD COLUMN auth_type TEXT DEFAULT 'password'",
        [],
    );
    let _ = conn.execute("ALTER TABLE connections ADD COLUMN ssh_key_id INTEGER REFERENCES ssh_keys(id) ON DELETE SET NULL", []);

    // Migration: Add reconnect settings
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN reconnect_max_attempts INTEGER NOT NULL DEFAULT 5"#,
        [],
    );
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN reconnect_initial_delay_ms INTEGER NOT NULL DEFAULT 1000"#,
        [],
    );
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN reconnect_max_delay_ms INTEGER NOT NULL DEFAULT 30000"#,
        [],
    );
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN reconnect_backoff_multiplier REAL NOT NULL DEFAULT 2.0"#,
        [],
    );
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN reconnect_enabled INTEGER NOT NULL DEFAULT 1"#,
        [],
    );

    // Migration: Add heartbeat settings
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN heartbeat_tcp_keepalive_interval_secs INTEGER NOT NULL DEFAULT 60"#,
        [],
    );
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN heartbeat_ssh_keepalive_interval_secs INTEGER NOT NULL DEFAULT 15"#,
        [],
    );
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN heartbeat_app_heartbeat_interval_secs INTEGER NOT NULL DEFAULT 30"#,
        [],
    );
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN heartbeat_timeout_secs INTEGER NOT NULL DEFAULT 5"#,
        [],
    );
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN heartbeat_failed_heartbeats_before_action INTEGER NOT NULL DEFAULT 3"#,
        [],
    );

    // Migration: Add pool health settings
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN pool_health_check_interval_secs INTEGER NOT NULL DEFAULT 60"#,
        [],
    );
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN pool_session_warmup_count INTEGER NOT NULL DEFAULT 1"#,
        [],
    );
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN pool_max_session_age_minutes INTEGER NOT NULL DEFAULT 60"#,
        [],
    );
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN pool_unhealthy_threshold INTEGER NOT NULL DEFAULT 3"#,
        [],
    );

    // Migration: Add network adaptive settings
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN network_adaptive_enabled INTEGER NOT NULL DEFAULT 1"#,
        [],
    );
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN network_latency_check_interval_secs INTEGER NOT NULL DEFAULT 30"#,
        [],
    );
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN network_high_latency_threshold_ms INTEGER NOT NULL DEFAULT 300"#,
        [],
    );
    let _ = conn.execute(
        r#"ALTER TABLE settings ADD COLUMN network_low_bandwidth_threshold_kbps INTEGER NOT NULL DEFAULT 100"#,
        [],
    );

    Ok(())
}

#[tauri::command]
pub fn get_connections(app_handle: AppHandle) -> Result<Vec<SshConnection>, String> {
    let db_path = get_db_path(&app_handle);
    let conn = Connection::open(db_path).map_err(|e| e.to_string())?;

    let mut stmt = conn.prepare("SELECT id, name, host, port, username, password, jump_host, jump_port, jump_username, jump_password, group_id, os_type, auth_type, ssh_key_id FROM connections")
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map([], |row| {
            Ok(SshConnection {
                id: row.get(0)?,
                name: row.get(1)?,
                host: row.get(2)?,
                port: row.get(3)?,
                username: row.get(4)?,
                password: row.get(5)?,
                jump_host: row.get(6)?,
                jump_port: row.get(7)?,
                jump_username: row.get(8)?,
                jump_password: row.get(9)?,
                group_id: row.get(10)?,
                os_type: row.get(11)?,
                auth_type: row.get(12)?,
                ssh_key_id: row.get(13)?,
                key_content: None,
                key_passphrase: None,
            })
        })
        .map_err(|e| e.to_string())?;

    let mut connections = Vec::new();
    for row in rows {
        connections.push(row.map_err(|e| e.to_string())?);
    }
    Ok(connections)
}

#[tauri::command]
pub fn get_groups(app_handle: AppHandle) -> Result<Vec<ConnectionGroup>, String> {
    let db_path = get_db_path(&app_handle);
    let conn = Connection::open(db_path).map_err(|e| e.to_string())?;

    let mut stmt = conn
        .prepare("SELECT id, name, parent_id FROM connection_groups")
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map([], |row| {
            Ok(ConnectionGroup {
                id: row.get(0)?,
                name: row.get(1)?,
                parent_id: row.get(2)?,
            })
        })
        .map_err(|e| e.to_string())?;

    let mut groups = Vec::new();
    for row in rows {
        groups.push(row.map_err(|e| e.to_string())?);
    }
    Ok(groups)
}

#[tauri::command]
pub fn create_connection(app_handle: AppHandle, conn: SshConnection) -> Result<(), String> {
    println!("Creating connection: {:?}", conn);
    let db_path = get_db_path(&app_handle);
    let db_conn = Connection::open(db_path).map_err(|e| e.to_string())?;

    db_conn.execute(
        "INSERT INTO connections (name, host, port, username, password, jump_host, jump_port, jump_username, jump_password, group_id, os_type, auth_type, ssh_key_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![conn.name, conn.host, conn.port, conn.username, conn.password, conn.jump_host, conn.jump_port, conn.jump_username, conn.jump_password, conn.group_id, conn.os_type, conn.auth_type.unwrap_or("password".to_string()), conn.ssh_key_id],
    ).map_err(|e| {
        println!("Error inserting connection: {}", e);
        e.to_string()
    })?;
    println!("Connection created successfully");
    Ok(())
}

#[tauri::command]
pub fn update_connection(app_handle: AppHandle, conn: SshConnection) -> Result<(), String> {
    let db_path = get_db_path(&app_handle);
    let db_conn = Connection::open(db_path).map_err(|e| e.to_string())?;

    db_conn.execute(
        "UPDATE connections SET name=?1, host=?2, port=?3, username=?4, password=?5, jump_host=?6, jump_port=?7, jump_username=?8, jump_password=?9, group_id=?10, os_type=?11, auth_type=?12, ssh_key_id=?13 WHERE id=?14",
        params![conn.name, conn.host, conn.port, conn.username, conn.password, conn.jump_host, conn.jump_port, conn.jump_username, conn.jump_password, conn.group_id, conn.os_type, conn.auth_type.unwrap_or("password".to_string()), conn.ssh_key_id, conn.id],
    ).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn delete_connection(app_handle: AppHandle, id: i64) -> Result<(), String> {
    let db_path = get_db_path(&app_handle);
    let db_conn = Connection::open(db_path).map_err(|e| e.to_string())?;

    db_conn
        .execute("DELETE FROM connections WHERE id = ?1", params![id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn create_group(app_handle: AppHandle, group: ConnectionGroup) -> Result<(), String> {
    let db_path = get_db_path(&app_handle);
    let db_conn = Connection::open(db_path).map_err(|e| e.to_string())?;

    db_conn
        .execute(
            "INSERT INTO connection_groups (name, parent_id) VALUES (?1, ?2)",
            params![group.name, group.parent_id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn update_group(app_handle: AppHandle, group: ConnectionGroup) -> Result<(), String> {
    let db_path = get_db_path(&app_handle);
    let db_conn = Connection::open(db_path).map_err(|e| e.to_string())?;

    db_conn
        .execute(
            "UPDATE connection_groups SET name=?1, parent_id=?2 WHERE id=?3",
            params![group.name, group.parent_id, group.id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn delete_group(app_handle: AppHandle, id: i64) -> Result<(), String> {
    let db_path = get_db_path(&app_handle);
    let db_conn = Connection::open(db_path).map_err(|e| e.to_string())?;

    // Note: ON DELETE CASCADE on parent_id handles subgroups
    // But for connections, we set group_id to NULL (ON DELETE SET NULL)
    db_conn
        .execute("DELETE FROM connection_groups WHERE id = ?1", params![id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn get_settings(app_handle: AppHandle) -> Result<AppSettings, String> {
    let db_path = get_db_path(&app_handle);
    let conn = Connection::open(db_path).map_err(|e| e.to_string())?;

    let mut stmt = conn.prepare("SELECT theme, language, ai_api_url, ai_api_key, ai_model_name, terminal_font_size, terminal_font_family, terminal_cursor_style, terminal_line_height, file_manager_view_mode, ssh_max_background_sessions, ssh_enable_auto_cleanup, ssh_cleanup_interval_minutes, file_manager_sftp_buffer_size, connection_timeout_secs, jump_host_timeout_secs, local_forward_timeout_secs, command_timeout_secs, sftp_operation_timeout_secs, reconnect_max_attempts, reconnect_initial_delay_ms, reconnect_max_delay_ms, reconnect_backoff_multiplier, reconnect_enabled, heartbeat_tcp_keepalive_interval_secs, heartbeat_ssh_keepalive_interval_secs, heartbeat_app_heartbeat_interval_secs, heartbeat_timeout_secs, heartbeat_failed_heartbeats_before_action, pool_health_check_interval_secs, pool_session_warmup_count, pool_max_session_age_minutes, pool_unhealthy_threshold, network_adaptive_enabled, network_latency_check_interval_secs, network_high_latency_threshold_ms, network_low_bandwidth_threshold_kbps FROM settings WHERE id = 1")
        .map_err(|e| e.to_string())?;

    let mut rows = stmt
        .query_map([], |row| {
            Ok(AppSettings {
                theme: row.get(0)?,
                language: row.get(1)?,
                ai: AIConfig {
                    api_url: row.get(2)?,
                    api_key: row.get(3)?,
                    model_name: row.get(4)?,
                },
                terminal_appearance: TerminalAppearanceSettings {
                    font_size: row.get::<_, Option<i32>>(5)?.unwrap_or(14),
                    font_family: row
                        .get::<_, Option<String>>(6)?
                        .unwrap_or_else(|| "Menlo, Monaco, \"Courier New\", monospace".to_string()),
                    cursor_style: row
                        .get::<_, Option<String>>(7)?
                        .unwrap_or_else(|| "block".to_string()),
                    line_height: row.get::<_, Option<f32>>(8)?.unwrap_or(1.0),
                },
                file_manager: FileManagerSettings {
                    view_mode: row
                        .get::<_, Option<String>>(9)?
                        .unwrap_or_else(|| "flat".to_string()),
                    sftp_buffer_size: row.get::<_, Option<i32>>(13)?.unwrap_or(512),
                },
                ssh_pool: SshPoolSettings {
                    max_background_sessions: row.get::<_, Option<i32>>(10)?.unwrap_or(10),
                    enable_auto_cleanup: row.get::<_, Option<bool>>(11)?.unwrap_or(true),
                    cleanup_interval_minutes: row.get::<_, Option<i32>>(12)?.unwrap_or(5),
                },
                connection_timeout: ConnectionTimeoutSettings {
                    connection_timeout_secs: row.get::<_, Option<u32>>(14)?.unwrap_or(15),
                    jump_host_timeout_secs: row.get::<_, Option<u32>>(15)?.unwrap_or(30),
                    local_forward_timeout_secs: row.get::<_, Option<u32>>(16)?.unwrap_or(10),
                    command_timeout_secs: row.get::<_, Option<u32>>(17)?.unwrap_or(30),
                    sftp_operation_timeout_secs: row.get::<_, Option<u32>>(18)?.unwrap_or(60),
                },
                reconnect: ReconnectSettings {
                    max_reconnect_attempts: row.get::<_, Option<u32>>(19)?.unwrap_or(5),
                    initial_delay_ms: row.get::<_, Option<u32>>(20)?.unwrap_or(1000),
                    max_delay_ms: row.get::<_, Option<u32>>(21)?.unwrap_or(30000),
                    backoff_multiplier: row.get::<_, Option<f32>>(22)?.unwrap_or(2.0),
                    enable_auto_reconnect: row.get::<_, Option<bool>>(23)?.unwrap_or(true),
                },
                heartbeat: HeartbeatSettings {
                    tcp_keepalive_interval_secs: row.get::<_, Option<u32>>(24)?.unwrap_or(60),
                    ssh_keepalive_interval_secs: row.get::<_, Option<u32>>(25)?.unwrap_or(15),
                    app_heartbeat_interval_secs: row.get::<_, Option<u32>>(26)?.unwrap_or(30),
                    heartbeat_timeout_secs: row.get::<_, Option<u32>>(27)?.unwrap_or(5),
                    failed_heartbeats_before_action: row.get::<_, Option<u32>>(28)?.unwrap_or(3),
                },
                pool_health: PoolHealthSettings {
                    health_check_interval_secs: row.get::<_, Option<u32>>(29)?.unwrap_or(60),
                    session_warmup_count: row.get::<_, Option<u32>>(30)?.unwrap_or(1),
                    max_session_age_minutes: row.get::<_, Option<u32>>(31)?.unwrap_or(60),
                    unhealthy_threshold: row.get::<_, Option<u32>>(32)?.unwrap_or(3),
                },
                network_adaptive: NetworkAdaptiveSettings {
                    enable_adaptive: row.get::<_, Option<bool>>(33)?.unwrap_or(true),
                    latency_check_interval_secs: row.get::<_, Option<u32>>(34)?.unwrap_or(30),
                    high_latency_threshold_ms: row.get::<_, Option<u32>>(35)?.unwrap_or(300),
                    low_bandwidth_threshold_kbps: row.get::<_, Option<u32>>(36)?.unwrap_or(100),
                },
            })
        })
        .map_err(|e| e.to_string())?;

    if let Some(row) = rows.next() {
        row.map_err(|e| e.to_string())
    } else {
        Err("Settings not found".to_string())
    }
}

#[tauri::command]
pub fn save_settings(app_handle: AppHandle, settings: AppSettings) -> Result<(), String> {
    let db_path = get_db_path(&app_handle);
    let conn = Connection::open(db_path).map_err(|e| e.to_string())?;

    conn.execute(
        "UPDATE settings SET theme=?1, language=?2, ai_api_url=?3, ai_api_key=?4, ai_model_name=?5, terminal_font_size=?6, terminal_font_family=?7, terminal_cursor_style=?8, terminal_line_height=?9, file_manager_view_mode=?10, ssh_max_background_sessions=?11, ssh_enable_auto_cleanup=?12, ssh_cleanup_interval_minutes=?13, file_manager_sftp_buffer_size=?14, connection_timeout_secs=?15, jump_host_timeout_secs=?16, local_forward_timeout_secs=?17, command_timeout_secs=?18, sftp_operation_timeout_secs=?19, reconnect_max_attempts=?20, reconnect_initial_delay_ms=?21, reconnect_max_delay_ms=?22, reconnect_backoff_multiplier=?23, reconnect_enabled=?24, heartbeat_tcp_keepalive_interval_secs=?25, heartbeat_ssh_keepalive_interval_secs=?26, heartbeat_app_heartbeat_interval_secs=?27, heartbeat_timeout_secs=?28, heartbeat_failed_heartbeats_before_action=?29, pool_health_check_interval_secs=?30, pool_session_warmup_count=?31, pool_max_session_age_minutes=?32, pool_unhealthy_threshold=?33, network_adaptive_enabled=?34, network_latency_check_interval_secs=?35, network_high_latency_threshold_ms=?36, network_low_bandwidth_threshold_kbps=?37 WHERE id = 1",
        params![
            settings.theme,
            settings.language,
            settings.ai.api_url,
            settings.ai.api_key,
            settings.ai.model_name,
            settings.terminal_appearance.font_size,
            settings.terminal_appearance.font_family,
            settings.terminal_appearance.cursor_style,
            settings.terminal_appearance.line_height,
            settings.file_manager.view_mode,
            settings.ssh_pool.max_background_sessions,
            settings.ssh_pool.enable_auto_cleanup,
            settings.ssh_pool.cleanup_interval_minutes,
            settings.file_manager.sftp_buffer_size,
            settings.connection_timeout.connection_timeout_secs,
            settings.connection_timeout.jump_host_timeout_secs,
            settings.connection_timeout.local_forward_timeout_secs,
            settings.connection_timeout.command_timeout_secs,
            settings.connection_timeout.sftp_operation_timeout_secs,
            settings.reconnect.max_reconnect_attempts,
            settings.reconnect.initial_delay_ms,
            settings.reconnect.max_delay_ms,
            settings.reconnect.backoff_multiplier,
            settings.reconnect.enable_auto_reconnect,
            settings.heartbeat.tcp_keepalive_interval_secs,
            settings.heartbeat.ssh_keepalive_interval_secs,
            settings.heartbeat.app_heartbeat_interval_secs,
            settings.heartbeat.heartbeat_timeout_secs,
            settings.heartbeat.failed_heartbeats_before_action,
            settings.pool_health.health_check_interval_secs,
            settings.pool_health.session_warmup_count,
            settings.pool_health.max_session_age_minutes,
            settings.pool_health.unhealthy_threshold,
            settings.network_adaptive.enable_adaptive,
            settings.network_adaptive.latency_check_interval_secs,
            settings.network_adaptive.high_latency_threshold_ms,
            settings.network_adaptive.low_bandwidth_threshold_kbps,
        ],
    ).map_err(|e| e.to_string())?;

    Ok(())
}

// --- SSH Key Commands ---

#[tauri::command]
pub fn get_ssh_keys(app_handle: AppHandle) -> Result<Vec<SshKey>, String> {
    let db_path = get_db_path(&app_handle);
    let conn = Connection::open(db_path).map_err(|e| e.to_string())?;

    let mut stmt = conn
        .prepare("SELECT id, name, content, passphrase, created_at FROM ssh_keys ORDER BY created_at ASC")
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map([], |row| {
            Ok(SshKey {
                id: row.get(0)?,
                name: row.get(1)?,
                content: row.get(2)?,
                passphrase: row.get(3)?,
                created_at: row.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;

    let mut keys = Vec::new();
    for row in rows {
        keys.push(row.map_err(|e| e.to_string())?);
    }
    Ok(keys)
}

#[tauri::command]
pub fn create_ssh_key(app_handle: AppHandle, key: SshKey) -> Result<(), String> {
    let db_path = get_db_path(&app_handle);
    let conn = Connection::open(db_path).map_err(|e| e.to_string())?;

    conn.execute(
        "INSERT INTO ssh_keys (name, content, passphrase, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![key.name, key.content, key.passphrase, key.created_at],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn delete_ssh_key(app_handle: AppHandle, id: i64) -> Result<(), String> {
    let db_path = get_db_path(&app_handle);
    let conn = Connection::open(db_path).map_err(|e| e.to_string())?;

    conn.execute("DELETE FROM ssh_keys WHERE id = ?1", params![id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn get_ssh_key_by_id(app_handle: &AppHandle, id: i64) -> Result<Option<SshKey>, String> {
    let db_path = get_db_path(app_handle);
    let conn = Connection::open(db_path).map_err(|e| e.to_string())?;

    let mut stmt = conn
        .prepare("SELECT id, name, content, passphrase, created_at FROM ssh_keys WHERE id = ?1")
        .map_err(|e| e.to_string())?;

    let mut rows = stmt
        .query_map(params![id], |row| {
            Ok(SshKey {
                id: row.get(0)?,
                name: row.get(1)?,
                content: row.get(2)?,
                passphrase: row.get(3)?,
                created_at: row.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;

    if let Some(row) = rows.next() {
        Ok(Some(row.map_err(|e| e.to_string())?))
    } else {
        Ok(None)
    }
}

#[tauri::command]
pub fn generate_ssh_key(
    app_handle: AppHandle,
    name: String,
    algorithm: String,
    passphrase: Option<String>,
) -> Result<SshKey, String> {
    let (private_key, _public_key) =
        crate::ssh::keys::generate_key_pair(&algorithm, passphrase.as_deref())?;

    let key = SshKey {
        id: None, // Will be set by DB
        name,
        content: private_key,
        passphrase,
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
    };

    let db_path = get_db_path(&app_handle);
    let conn = Connection::open(db_path).map_err(|e| e.to_string())?;

    conn.execute(
        "INSERT INTO ssh_keys (name, content, passphrase, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![key.name, key.content, key.passphrase, key.created_at],
    )
    .map_err(|e| e.to_string())?;

    let id = conn.last_insert_rowid();

    Ok(SshKey {
        id: Some(id),
        ..key
    })
}
