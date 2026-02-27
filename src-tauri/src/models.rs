use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SshKey {
    pub id: Option<i64>,
    pub name: String,
    pub content: String,
    pub passphrase: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub id: Option<i64>,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: Option<String>,
    pub auth_type: Option<String>, // "password" or "key", default "password"
    pub ssh_key_id: Option<i64>,
    // Jump host configuration
    pub jump_host: Option<String>,
    pub jump_port: Option<u16>,
    pub jump_username: Option<String>,
    pub jump_password: Option<String>,
    pub group_id: Option<i64>,
    pub os_type: Option<String>, // Default "Linux" for backward compatibility

    // Internal use for connection (not stored in connections table)
    pub key_content: Option<String>,
    pub key_passphrase: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionGroup {
    pub id: Option<i64>,
    pub name: String,
    pub parent_id: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: i64, // Unix timestamp
    pub permissions: u32,
    pub uid: u32,
    pub owner: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AIConfig {
    pub api_url: String,
    pub api_key: String,
    pub model_name: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAppearanceSettings {
    pub font_size: i32,
    pub font_family: String,
    pub cursor_style: String,
    pub line_height: f32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FileManagerSettings {
    pub view_mode: String,
    pub sftp_buffer_size: i32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SshPoolSettings {
    pub max_background_sessions: i32,
    pub enable_auto_cleanup: bool,
    pub cleanup_interval_minutes: i32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionTimeoutSettings {
    pub connection_timeout_secs: u32,
    pub jump_host_timeout_secs: u32,
    pub local_forward_timeout_secs: u32,
    pub command_timeout_secs: u32,
    pub sftp_operation_timeout_secs: u32,
}

impl Default for ConnectionTimeoutSettings {
    fn default() -> Self {
        Self {
            connection_timeout_secs: 15,
            jump_host_timeout_secs: 30,
            local_forward_timeout_secs: 10,
            command_timeout_secs: 30,
            sftp_operation_timeout_secs: 60,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ReconnectSettings {
    pub max_reconnect_attempts: u32,           // 最大重连次数，默认 5
    pub initial_delay_ms: u32,                 // 初始延迟，默认 1000ms
    pub max_delay_ms: u32,                     // 最大延迟，默认 30000ms
    pub backoff_multiplier: f32,               // 退避倍数，默认 2.0
    pub enable_auto_reconnect: bool,           // 是否启用自动重连，默认 true
}

impl Default for ReconnectSettings {
    fn default() -> Self {
        Self {
            max_reconnect_attempts: 5,
            initial_delay_ms: 1000,
            max_delay_ms: 30000,
            backoff_multiplier: 2.0,
            enable_auto_reconnect: true,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatSettings {
    pub tcp_keepalive_interval_secs: u32,      // TCP keepalive 间隔，默认 60
    pub ssh_keepalive_interval_secs: u32,      // SSH keepalive 间隔，默认 15
    pub app_heartbeat_interval_secs: u32,      // 应用层心跳间隔，默认 30
    pub heartbeat_timeout_secs: u32,           // 心跳超时，默认 5
    pub failed_heartbeats_before_action: u32,  // 触发动作前的心跳失败次数，默认 3
}

impl Default for HeartbeatSettings {
    fn default() -> Self {
        Self {
            tcp_keepalive_interval_secs: 60,
            ssh_keepalive_interval_secs: 15,
            app_heartbeat_interval_secs: 30,
            heartbeat_timeout_secs: 5,
            failed_heartbeats_before_action: 3,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PoolHealthSettings {
    pub health_check_interval_secs: u32,     // 健康检查间隔，默认 60
    pub session_warmup_count: u32,           // 预热会话数量，默认 1
    pub max_session_age_minutes: u32,        // 会话最大存活时间，默认 60
    pub unhealthy_threshold: u32,            // 判定为不健康的失败次数，默认 3
}

impl Default for PoolHealthSettings {
    fn default() -> Self {
        Self {
            health_check_interval_secs: 60,
            session_warmup_count: 1,
            max_session_age_minutes: 60,
            unhealthy_threshold: 3,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub theme: String,
    pub language: String,
    pub ai: AIConfig,
    pub terminal_appearance: TerminalAppearanceSettings,
    pub file_manager: FileManagerSettings,
    pub ssh_pool: SshPoolSettings,
    #[serde(default)]
    pub connection_timeout: ConnectionTimeoutSettings,
    #[serde(default)]
    pub reconnect: ReconnectSettings,
    #[serde(default)]
    pub heartbeat: HeartbeatSettings,
    #[serde(default)]
    pub pool_health: PoolHealthSettings,
    #[serde(default)]
    pub network_adaptive: NetworkAdaptiveSettings,
}
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Transfer {
    pub id: String,
    pub session_id: String,
    pub name: String,
    pub local_path: String,
    pub remote_path: String,
    pub transfer_type: String, // "upload" | "download"
    pub status: String, // "pending" | "running" | "paused" | "completed" | "error" | "cancelled"
    pub total_size: u64,
    pub transferred: u64,
    pub created_at: i64,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionStatusEvent {
    pub session_id: String,
    pub status: ConnectionStatus,
    pub timestamp: i64,
    pub details: Option<String>,
    pub metrics: Option<ConnectionMetrics>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum ConnectionStatus {
    Connecting,
    Connected,
    Authenticating,
    Ready,
    Degraded,
    Reconnecting,
    Disconnected,
    Error,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionMetrics {
    pub uptime_secs: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub latency_ms: u32,
    pub reconnect_count: u32,
    pub last_error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum NetworkQuality {
    Excellent,  // < 50ms
    Good,       // 50-150ms
    Fair,       // 150-300ms
    Poor,       // > 300ms
    Unknown,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct NetworkAdaptiveSettings {
    pub enable_adaptive: bool,               // 是否启用自适应，默认 true
    pub latency_check_interval_secs: u32,    // 延迟检测间隔，默认 30
    pub high_latency_threshold_ms: u32,      // 高延迟阈值，默认 300
    pub low_bandwidth_threshold_kbps: u32,   // 低带宽阈值，默认 100
}

impl Default for NetworkAdaptiveSettings {
    fn default() -> Self {
        Self {
            enable_adaptive: true,
            latency_check_interval_secs: 30,
            high_latency_threshold_ms: 300,
            low_bandwidth_threshold_kbps: 100,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct NetworkStatus {
    pub latency_ms: u32,                     // 当前延迟
    pub bandwidth_kbps: Option<u32>,         // 估算带宽
    pub quality: NetworkQuality,             // 网络质量等级
    pub last_update: i64,                    // 最后更新时间戳
}

impl Default for NetworkStatus {
    fn default() -> Self {
        Self {
            latency_ms: 0,
            bandwidth_kbps: None,
            quality: NetworkQuality::Unknown,
            last_update: 0,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AdaptiveParams {
    pub heartbeat_interval_secs: u32,
    pub sftp_buffer_size: usize,
    pub command_timeout_secs: u32,
    pub keepalive_interval_secs: u32,
}
