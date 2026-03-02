// Default timeout values (used when settings are not available)
pub const DEFAULT_CONNECTION_TIMEOUT_SECS: u32 = 15;
pub const DEFAULT_JUMP_HOST_TIMEOUT_SECS: u32 = 30;
pub const DEFAULT_LOCAL_FORWARD_TIMEOUT_SECS: u32 = 10;
pub const DEFAULT_COMMAND_TIMEOUT_SECS: u32 = 30;
pub const DEFAULT_SFTP_OPERATION_TIMEOUT_SECS: u32 = 60;

use crate::models::ConnectionTimeoutSettings;

pub fn get_connection_timeout(settings: Option<&ConnectionTimeoutSettings>) -> std::time::Duration {
    std::time::Duration::from_secs(
        settings
            .map(|s| s.connection_timeout_secs)
            .unwrap_or(DEFAULT_CONNECTION_TIMEOUT_SECS) as u64
    )
}

pub fn get_jump_host_timeout(settings: Option<&ConnectionTimeoutSettings>) -> std::time::Duration {
    std::time::Duration::from_secs(
        settings
            .map(|s| s.jump_host_timeout_secs)
            .unwrap_or(DEFAULT_JUMP_HOST_TIMEOUT_SECS) as u64
    )
}

pub fn get_local_forward_timeout(settings: Option<&ConnectionTimeoutSettings>) -> std::time::Duration {
    std::time::Duration::from_secs(
        settings
            .map(|s| s.local_forward_timeout_secs)
            .unwrap_or(DEFAULT_LOCAL_FORWARD_TIMEOUT_SECS) as u64
    )
}

pub fn get_command_timeout(settings: Option<&ConnectionTimeoutSettings>) -> std::time::Duration {
    std::time::Duration::from_secs(
        settings
            .map(|s| s.command_timeout_secs)
            .unwrap_or(DEFAULT_COMMAND_TIMEOUT_SECS) as u64
    )
}

pub fn get_sftp_operation_timeout(settings: Option<&ConnectionTimeoutSettings>) -> std::time::Duration {
    std::time::Duration::from_secs(
        settings
            .map(|s| s.sftp_operation_timeout_secs)
            .unwrap_or(DEFAULT_SFTP_OPERATION_TIMEOUT_SECS) as u64
    )
}

#[derive(Debug, Clone)]
pub enum ShellMsg {
    Data(Vec<u8>),
    Resize { rows: u16, cols: u16 },
    Exit,
}

#[derive(Clone, serde::Serialize)]
pub struct ProgressPayload {
    pub id: String,
    pub transferred: u64,
    pub total: u64,
}

pub mod client;
pub mod command;
pub mod connection;
pub mod error_classifier;
pub mod events;
pub mod file_ops;
pub mod health_check;
pub mod heartbeat;
pub mod keys;
pub mod manager;
pub mod network_monitor;
pub mod reconnect;
pub mod system;
pub mod terminal;
pub mod utils;
pub mod wsl;
pub mod transfer;

// Re-export main types and functions for backward compatibility
pub use client::AppState;
pub use error_classifier::{SshErrorClassifier, SshErrorType};
pub use health_check::{
    HealthAction, PoolHealthChecker, PoolHealthReport, SessionHealth, SessionHealthMetadata,
};
pub use heartbeat::{HeartbeatAction, HeartbeatManager, HeartbeatResult, HeartbeatStatus};
pub use manager::SshCommand;
pub use network_monitor::NetworkMonitor;
pub use reconnect::ReconnectManager;
pub use utils::{execute_ssh_operation, ssh2_retry};

// Re-export transfer module types
pub use transfer::{
    TransferManager, TransferOperation, TransferSettings, TransferError,
    TransferEvent, TransferHealth, TransferStatus,
    TransferState, TransferStateHandle, TransferCheckpoint, CheckpointManager,
    PoolStats, TransferConnection, TransferPool, AsyncSftp,
};
