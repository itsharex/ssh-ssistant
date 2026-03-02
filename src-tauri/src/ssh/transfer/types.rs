//! File transfer type definitions
//!
//! This module defines all types used throughout the file transfer system,
//! including status enums, error types, settings, and events.

use std::time::Duration;

/// Transfer operation type
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferOperation {
    Upload,
    Download,
}

impl std::fmt::Display for TransferOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransferOperation::Upload => write!(f, "upload"),
            TransferOperation::Download => write!(f, "download"),
        }
    }
}

/// Transfer status states following the state machine design
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferStatus {
    // Initial states
    Pending,

    // Active states
    Connecting,
    Transferring,
    Paused,

    // Completion states
    Completed,
    Failed,
    Cancelled,

    // Resume state
    Resuming,
}

impl TransferStatus {
    /// Check if status is a terminal state (no further transitions possible)
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TransferStatus::Completed | TransferStatus::Failed | TransferStatus::Cancelled
        )
    }

    /// Check if status is an active state (transfer can be in progress)
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            TransferStatus::Connecting | TransferStatus::Transferring | TransferStatus::Resuming
        )
    }

    /// Check if transfer can be paused from this state
    pub fn can_pause(&self) -> bool {
        matches!(self, TransferStatus::Transferring)
    }

    /// Check if transfer can be resumed from this state
    pub fn can_resume(&self) -> bool {
        matches!(self, TransferStatus::Paused | TransferStatus::Failed)
    }

    /// Check if transfer can be cancelled from this state
    pub fn can_cancel(&self) -> bool {
        !self.is_terminal()
    }
}

impl std::fmt::Display for TransferStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransferStatus::Pending => write!(f, "pending"),
            TransferStatus::Connecting => write!(f, "connecting"),
            TransferStatus::Transferring => write!(f, "transferring"),
            TransferStatus::Paused => write!(f, "paused"),
            TransferStatus::Completed => write!(f, "completed"),
            TransferStatus::Failed => write!(f, "failed"),
            TransferStatus::Cancelled => write!(f, "cancelled"),
            TransferStatus::Resuming => write!(f, "resuming"),
        }
    }
}

/// Transfer configuration settings
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferSettings {
    // Connection pool settings
    pub max_transfer_connections: usize,
    pub transfer_connection_idle_timeout_secs: u64,

    // Transfer settings
    pub default_chunk_size: usize,
    pub max_concurrent_transfers: usize,

    // Timeout settings
    pub transfer_timeout_secs: u32,
    pub no_progress_timeout_secs: u32,
    pub operation_timeout_secs: u32,

    // Resume/checkpoint settings
    pub enable_resume: bool,
    pub checkpoint_interval_secs: u32,
    pub checkpoint_interval_bytes: u64,
    pub verify_checksum: bool,

    // Retry settings
    pub max_retry_attempts: u32,
    pub retry_delay_ms: u64,
}

impl Default for TransferSettings {
    fn default() -> Self {
        Self {
            max_transfer_connections: 3,
            transfer_connection_idle_timeout_secs: 300,
            default_chunk_size: 65536, // 64KB
            max_concurrent_transfers: 5,
            transfer_timeout_secs: 300, // 5 minutes
            no_progress_timeout_secs: 30,
            operation_timeout_secs: 60,
            enable_resume: true,
            checkpoint_interval_secs: 10,
            checkpoint_interval_bytes: 10 * 1024 * 1024, // 10MB
            verify_checksum: false,
            max_retry_attempts: 3,
            retry_delay_ms: 1000,
        }
    }
}

impl TransferSettings {
    /// Get transfer timeout as Duration
    pub fn transfer_timeout(&self) -> Duration {
        Duration::from_secs(self.transfer_timeout_secs as u64)
    }

    /// Get no-progress timeout as Duration
    pub fn no_progress_timeout(&self) -> Duration {
        Duration::from_secs(self.no_progress_timeout_secs as u64)
    }

    /// Get operation timeout as Duration
    pub fn operation_timeout(&self) -> Duration {
        Duration::from_secs(self.operation_timeout_secs as u64)
    }

    /// Get checkpoint interval as Duration
    pub fn checkpoint_interval(&self) -> Duration {
        Duration::from_secs(self.checkpoint_interval_secs as u64)
    }

    /// Get idle timeout for connection cleanup as Duration
    pub fn idle_timeout(&self) -> Duration {
        Duration::from_secs(self.transfer_connection_idle_timeout_secs)
    }

    /// Get retry delay as Duration
    pub fn retry_delay(&self) -> Duration {
        Duration::from_millis(self.retry_delay_ms)
    }
}

/// Transfer events for monitoring and logging
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferEvent {
    Started {
        id: String,
        operation: TransferOperation,
    },
    Progress {
        id: String,
        transferred: u64,
        total: u64,
        speed_bps: f64,
    },
    Paused {
        id: String,
        transferred: u64,
    },
    Resumed {
        id: String,
        from_offset: u64,
    },
    Completed {
        id: String,
        duration_secs: u64,
        total_bytes: u64,
    },
    Failed {
        id: String,
        error: String,
        transferred: u64,
    },
    Cancelled {
        id: String,
        transferred: u64,
    },
    CheckpointSaved {
        id: String,
        transferred: u64,
    },
}

impl std::fmt::Display for TransferEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransferEvent::Started { id, operation } => {
                write!(f, "Transfer {} started ({})", id, operation)
            }
            TransferEvent::Progress { id, transferred, total, speed_bps } => {
                write!(
                    f,
                    "Transfer {} progress: {}/{} bytes ({:.2} MB/s)",
                    id,
                    transferred,
                    total,
                    speed_bps / 1_000_000.0
                )
            }
            TransferEvent::Paused { id, transferred } => {
                write!(f, "Transfer {} paused at {} bytes", id, transferred)
            }
            TransferEvent::Resumed { id, from_offset } => {
                write!(f, "Transfer {} resumed from offset {}", id, from_offset)
            }
            TransferEvent::Completed { id, duration_secs, total_bytes } => {
                write!(
                    f,
                    "Transfer {} completed: {} bytes in {}s",
                    id, total_bytes, duration_secs
                )
            }
            TransferEvent::Failed { id, error, transferred } => {
                write!(f, "Transfer {} failed at {} bytes: {}", id, transferred, error)
            }
            TransferEvent::Cancelled { id, transferred } => {
                write!(f, "Transfer {} cancelled at {} bytes", id, transferred)
            }
            TransferEvent::CheckpointSaved { id, transferred } => {
                write!(f, "Transfer {} checkpoint saved at {} bytes", id, transferred)
            }
        }
    }
}

/// Transfer error types with retry information
#[derive(Debug, Clone, thiserror::Error)]
pub enum TransferError {
    // Retryable errors
    #[error("Temporary network error: {0}")]
    TemporaryNetwork(String),

    #[error("Operation timed out: {0}")]
    Timeout(String),

    #[error("Operation would block (non-blocking mode)")]
    WouldBlock,

    // Non-retryable errors
    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("Disk full: {0}")]
    DiskFull(String),

    #[error("Invalid path: {0}")]
    InvalidPath(String),

    #[error("Authentication failed: {0}")]
    AuthenticationFailed(String),

    #[error("Connection lost")]
    ConnectionLost,

    // Control errors
    #[error("Transfer cancelled by user")]
    Cancelled,

    // Checkpoint/resume errors
    #[error("Checkpoint mismatch: {0}")]
    CheckpointMismatch(String),

    #[error("Cannot resume transfer: {0}")]
    CannotResume(String),

    #[error("Invalid checkpoint data")]
    InvalidCheckpoint,

    // Other errors
    #[error("Unknown error: {0}")]
    Unknown(String),
}

impl TransferError {
    /// Check if this error is retryable
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            TransferError::TemporaryNetwork(_)
                | TransferError::Timeout(_)
                | TransferError::WouldBlock
                | TransferError::ConnectionLost
        )
    }

    /// Check if this error indicates a connection issue
    pub fn is_connection_error(&self) -> bool {
        matches!(
            self,
            TransferError::TemporaryNetwork(_)
                | TransferError::ConnectionLost
                | TransferError::Timeout(_)
        )
    }

    /// Check if this error is a permission issue
    pub fn is_permission_error(&self) -> bool {
        matches!(self, TransferError::PermissionDenied(_) | TransferError::AuthenticationFailed(_))
    }
}

impl From<ssh2::Error> for TransferError {
    fn from(err: ssh2::Error) -> Self {
        // Try to map SSH2 errors to our transfer errors
        let err_msg = err.to_string();
        if err_msg.contains("timeout") || err_msg.contains("timed out") {
            TransferError::Timeout(err_msg)
        } else if err_msg.contains("permission") || err_msg.contains("denied") {
            TransferError::PermissionDenied(err_msg)
        } else if err_msg.contains("auth") {
            TransferError::AuthenticationFailed(err_msg)
        } else {
            TransferError::Unknown(err_msg)
        }
    }
}

impl From<std::io::Error> for TransferError {
    fn from(err: std::io::Error) -> Self {
        match err.kind() {
            std::io::ErrorKind::PermissionDenied => {
                TransferError::PermissionDenied(err.to_string())
            }
            std::io::ErrorKind::NotFound => TransferError::InvalidPath(err.to_string()),
            std::io::ErrorKind::WouldBlock => TransferError::WouldBlock,
            std::io::ErrorKind::TimedOut => TransferError::Timeout(err.to_string()),
            std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted => {
                TransferError::ConnectionLost
            }
            _ => TransferError::Unknown(err.to_string()),
        }
    }
}

impl From<String> for TransferError {
    fn from(err: String) -> Self {
        TransferError::Unknown(err)
    }
}

/// Progress callback type for transfer operations
pub type ProgressCallback = Box<dyn Fn(u64, u64) + Send + Sync>;

/// Health status of the transfer system
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferHealth {
    pub active_transfers: usize,
    pub stuck_transfers: usize,
    pub failed_transfers: usize,
    pub avg_speed_bps: f64,
    pub pool_usage: f64,
}

impl Default for TransferHealth {
    fn default() -> Self {
        Self {
            active_transfers: 0,
            stuck_transfers: 0,
            failed_transfers: 0,
            avg_speed_bps: 0.0,
            pool_usage: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transfer_status_transitions() {
        assert!(!TransferStatus::Pending.is_terminal());
        assert!(!TransferStatus::Transferring.is_terminal());
        assert!(TransferStatus::Completed.is_terminal());
        assert!(TransferStatus::Failed.is_terminal());
        assert!(TransferStatus::Cancelled.is_terminal());
    }

    #[test]
    fn test_transfer_status_operations() {
        assert!(TransferStatus::Transferring.can_pause());
        assert!(TransferStatus::Paused.can_resume());
        assert!(TransferStatus::Failed.can_resume());
        assert!(!TransferStatus::Completed.can_cancel());
        assert!(TransferStatus::Transferring.can_cancel());
    }

    #[test]
    fn test_error_classification() {
        let timeout_err = TransferError::Timeout("test".to_string());
        assert!(timeout_err.is_retryable());
        assert!(timeout_err.is_connection_error());

        let perm_err = TransferError::PermissionDenied("test".to_string());
        assert!(!perm_err.is_retryable());
        assert!(perm_err.is_permission_error());
    }

    #[test]
    fn test_settings_defaults() {
        let settings = TransferSettings::default();
        assert_eq!(settings.default_chunk_size, 65536);
        assert_eq!(settings.max_transfer_connections, 3);
        assert!(settings.enable_resume);
    }
}
