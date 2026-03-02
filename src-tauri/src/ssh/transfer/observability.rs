//! Enhanced observability system for file transfers
//!
//! This module provides comprehensive logging, metrics collection,
//! and monitoring capabilities for the transfer system.

use crate::ssh::transfer::types::{TransferEvent, TransferHealth, TransferOperation, TransferStatus};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Comprehensive transfer metrics
#[derive(Debug, Serialize)]
pub struct TransferMetrics {
    /// Total number of transfers initiated
    pub total_transfers: AtomicUsize,
    /// Currently active transfers
    pub active_transfers: AtomicUsize,
    /// Completed transfers
    pub completed_transfers: AtomicUsize,
    /// Failed transfers
    pub cancelled_transfers: AtomicUsize,
    /// Total bytes transferred
    pub total_bytes_transferred: AtomicU64,
    /// Current transfer rate (bytes per second)
    pub current_bps: AtomicU64,
    /// Average transfer rate (bytes per second)
    pub avg_bps: AtomicU64,
    /// Peak transfer rate (bytes per second)
    pub peak_bps: AtomicU64,
    /// Total transfer time (milliseconds)
    pub total_transfer_time_ms: AtomicU64,
    /// Number of retry attempts
    pub retry_attempts: AtomicUsize,
    /// Number of connection errors
    pub connection_errors: AtomicUsize,
    /// Number of permission errors
    pub permission_errors: AtomicUsize,
    /// Number of timeout errors
    pub timeout_errors: AtomicUsize,
}

impl Default for TransferMetrics {
    fn default() -> Self {
        Self {
            total_transfers: AtomicUsize::new(0),
            active_transfers: AtomicUsize::new(0),
            completed_transfers: AtomicUsize::new(0),
            cancelled_transfers: AtomicUsize::new(0),
            total_bytes_transferred: AtomicU64::new(0),
            current_bps: AtomicU64::new(0),
            avg_bps: AtomicU64::new(0),
            peak_bps: AtomicU64::new(0),
            total_transfer_time_ms: AtomicU64::new(0),
            retry_attempts: AtomicUsize::new(0),
            connection_errors: AtomicUsize::new(0),
            permission_errors: AtomicUsize::new(0),
            timeout_errors: AtomicUsize::new(0),
        }
    }
}

impl TransferMetrics {
    /// Record transfer start
    pub fn record_transfer_start(&self) {
        self.total_transfers.fetch_add(1, Ordering::Relaxed);
        self.active_transfers.fetch_add(1, Ordering::Relaxed);
    }

    /// Record transfer completion
    pub fn record_transfer_complete(&self, bytes: u64, duration_ms: u64) {
        self.active_transfers.fetch_sub(1, Ordering::Relaxed);
        self.completed_transfers.fetch_add(1, Ordering::Relaxed);
        self.total_bytes_transferred.fetch_add(bytes, Ordering::Relaxed);
        self.total_transfer_time_ms.fetch_add(duration_ms, Ordering::Relaxed);

        // Update average speed
        let total_bytes = self.total_bytes_transferred.load(Ordering::Relaxed);
        let total_time_ms = self.total_transfer_time_ms.load(Ordering::Relaxed);
        if total_time_ms > 0 {
            let avg_bps = (total_bytes * 1000) / total_time_ms;
            self.avg_bps.store(avg_bps, Ordering::Relaxed);
        }

        // Update peak speed
        let current_bps = if duration_ms > 0 { (bytes * 1000) / duration_ms } else { 0 };
        let peak_bps = self.peak_bps.load(Ordering::Relaxed);
        if current_bps > peak_bps {
            self.peak_bps.store(current_bps, Ordering::Relaxed);
        }
        self.current_bps.store(current_bps, Ordering::Relaxed);
    }

    /// Record transfer failure
    pub fn record_transfer_failed(&self, error_type: &str) {
        self.active_transfers.fetch_sub(1, Ordering::Relaxed);
        
        match error_type {
            "connection" | "network" => self.connection_errors.fetch_add(1, Ordering::Relaxed),
            "permission" | "auth" => self.permission_errors.fetch_add(1, Ordering::Relaxed),
            "timeout" => self.timeout_errors.fetch_add(1, Ordering::Relaxed),
            _ => 0,
        };
    }

    /// Record transfer cancellation
    pub fn record_transfer_cancelled(&self) {
        self.active_transfers.fetch_sub(1, Ordering::Relaxed);
        self.cancelled_transfers.fetch_add(1, Ordering::Relaxed);
    }

    /// Record retry attempt
    pub fn record_retry_attempt(&self) {
        self.retry_attempts.fetch_add(1, Ordering::Relaxed);
    }

    /// Get current health status
    pub fn get_health(&self) -> TransferHealth {
        let active = self.active_transfers.load(Ordering::Relaxed);
        let failed = self.connection_errors.load(Ordering::Relaxed) 
                   + self.permission_errors.load(Ordering::Relaxed)
                   + self.timeout_errors.load(Ordering::Relaxed);
        
        TransferHealth {
            active_transfers: active,
            stuck_transfers: if active > 0 { failed } else { 0 },
            failed_transfers: failed,
            avg_speed_bps: self.avg_bps.load(Ordering::Relaxed) as f64,
            pool_usage: 0.0, // Will be calculated by pool
        }
    }

    /// Reset all metrics
    pub fn reset(&self) {
        self.total_transfers.store(0, Ordering::Relaxed);
        self.active_transfers.store(0, Ordering::Relaxed);
        self.completed_transfers.store(0, Ordering::Relaxed);
        self.cancelled_transfers.store(0, Ordering::Relaxed);
        self.total_bytes_transferred.store(0, Ordering::Relaxed);
        self.current_bps.store(0, Ordering::Relaxed);
        self.avg_bps.store(0, Ordering::Relaxed);
        self.peak_bps.store(0, Ordering::Relaxed);
        self.total_transfer_time_ms.store(0, Ordering::Relaxed);
        self.retry_attempts.store(0, Ordering::Relaxed);
        self.connection_errors.store(0, Ordering::Relaxed);
        self.permission_errors.store(0, Ordering::Relaxed);
        self.timeout_errors.store(0, Ordering::Relaxed);
    }
}

/// Detailed transfer log entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferLogEntry {
    /// Timestamp of the log entry
    pub timestamp: u64,
    /// Transfer ID
    pub transfer_id: String,
    /// Log level
    pub level: LogLevel,
    /// Log message
    pub message: String,
    /// Additional context
    pub context: HashMap<String, String>,
    /// Operation type
    pub operation: Option<TransferOperation>,
    /// Current status
    pub status: Option<TransferStatus>,
    /// Progress information
    pub progress: Option<ProgressInfo>,
}

/// Log levels for transfer events
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LogLevel {
    Debug,
    Info,
    Warning,
    Error,
    Critical,
}

/// Progress information for log entries
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressInfo {
    pub transferred_bytes: u64,
    pub total_bytes: u64,
    pub speed_bps: f64,
    pub percentage: f64,
}

/// Observability manager for transfers
pub struct ObservabilityManager {
    /// Transfer metrics
    metrics: Arc<TransferMetrics>,
    /// Transfer logs
    logs: Arc<RwLock<Vec<TransferLogEntry>>>,
    /// Maximum number of log entries to keep
    max_logs: usize,
    /// Whether logging is enabled
    logging_enabled: Arc<AtomicBool>,
    /// Log level filter
    log_level_filter: Arc<RwLock<LogLevel>>,
}

impl ObservabilityManager {
    /// Create a new observability manager
    pub fn new(max_logs: usize) -> Self {
        Self {
            metrics: Arc::new(TransferMetrics::default()),
            logs: Arc::new(RwLock::new(Vec::new())),
            max_logs,
            logging_enabled: Arc::new(AtomicBool::new(true)),
            log_level_filter: Arc::new(RwLock::new(LogLevel::Debug)),
        }
    }

    /// Get metrics reference
    pub fn metrics(&self) -> Arc<TransferMetrics> {
        Arc::clone(&self.metrics)
    }

    /// Log a transfer event
    pub async fn log_event(
        &self,
        transfer_id: &str,
        level: LogLevel,
        message: String,
        operation: Option<TransferOperation>,
        status: Option<TransferStatus>,
        progress: Option<ProgressInfo>,
    ) {
        if !self.logging_enabled.load(Ordering::Relaxed) {
            return;
        }

        // Check log level filter
        let filter = self.log_level_filter.read().await;
        if !self.should_log(&level, &filter) {
            return;
        }
        drop(filter);

        let entry = TransferLogEntry {
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            transfer_id: transfer_id.to_string(),
            level,
            message,
            context: HashMap::new(),
            operation,
            status,
            progress,
        };

        let mut logs = self.logs.write().await;
        logs.push(entry);

        // Trim logs if necessary
        if logs.len() > self.max_logs {
            let remove_count = logs.len() - self.max_logs;
            logs.drain(0..remove_count);
        }
    }

    /// Get recent logs
    pub async fn get_recent_logs(&self, limit: Option<usize>) -> Vec<TransferLogEntry> {
        let logs = self.logs.read().await;
        let start_idx = if let Some(limit) = limit {
            logs.len().saturating_sub(limit)
        } else {
            0
        };
        logs[start_idx..].to_vec()
    }

    /// Get logs for a specific transfer
    pub async fn get_transfer_logs(&self, transfer_id: &str) -> Vec<TransferLogEntry> {
        let logs = self.logs.read().await;
        logs.iter()
            .filter(|entry| entry.transfer_id == transfer_id)
            .cloned()
            .collect()
    }

    /// Enable/disable logging
    pub fn set_logging_enabled(&self, enabled: bool) {
        self.logging_enabled.store(enabled, Ordering::Relaxed);
    }

    /// Set log level filter
    pub async fn set_log_level(&self, level: LogLevel) {
        let mut filter = self.log_level_filter.write().await;
        *filter = level;
    }

    /// Clear all logs
    pub async fn clear_logs(&self) {
        let mut logs = self.logs.write().await;
        logs.clear();
    }

    /// Get system health summary
    pub async fn get_health_summary(&self) -> HealthSummary {
        let metrics = &self.metrics;
        let recent_logs = self.get_recent_logs(Some(100)).await;
        
        let error_count = recent_logs.iter()
            .filter(|entry| matches!(entry.level, LogLevel::Error | LogLevel::Critical))
            .count();
        
        let warning_count = recent_logs.iter()
            .filter(|entry| matches!(entry.level, LogLevel::Warning))
            .count();

        HealthSummary {
            health: metrics.get_health(),
            error_count,
            warning_count,
            uptime_seconds: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }

    /// Check if a log level should be recorded
    fn should_log(&self, level: &LogLevel, filter: &LogLevel) -> bool {
        match (level, filter) {
            (LogLevel::Debug, LogLevel::Debug) => true,
            (LogLevel::Info, LogLevel::Debug | LogLevel::Info) => true,
            (LogLevel::Warning, LogLevel::Debug | LogLevel::Info | LogLevel::Warning) => true,
            (LogLevel::Error, LogLevel::Debug | LogLevel::Info | LogLevel::Warning | LogLevel::Error) => true,
            (LogLevel::Critical, _) => true,
            _ => false,
        }
    }
}

/// Health summary for the transfer system
#[derive(Debug, Clone, Serialize)]
pub struct HealthSummary {
    pub health: TransferHealth,
    pub error_count: usize,
    pub warning_count: usize,
    pub uptime_seconds: u64,
}

/// Macro for convenient logging
#[macro_export]
macro_rules! transfer_log {
    ($manager:expr, $level:expr, $transfer_id:expr, $message:expr $(,)?) => {
        $manager.log_event(
            $transfer_id,
            $level,
            $message.to_string(),
            None,
            None,
            None,
        ).await
    };
    ($manager:expr, $level:expr, $transfer_id:expr, $message:expr, $operation:expr $(,)?) => {
        $manager.log_event(
            $transfer_id,
            $level,
            $message.to_string(),
            Some($operation),
            None,
            None,
        ).await
    };
    ($manager:expr, $level:expr, $transfer_id:expr, $message:expr, $operation:expr, $status:expr $(,)?) => {
        $manager.log_event(
            $transfer_id,
            $level,
            $message.to_string(),
            Some($operation),
            Some($status),
            None,
        ).await
    };
    ($manager:expr, $level:expr, $transfer_id:expr, $message:expr, $operation:expr, $status:expr, $progress:expr $(,)?) => {
        $manager.log_event(
            $transfer_id,
            $level,
            $message.to_string(),
            Some($operation),
            Some($status),
            Some($progress),
        ).await
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_metrics_recording() {
        let metrics = TransferMetrics::default();
        
        metrics.record_transfer_start();
        assert_eq!(metrics.total_transfers.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.active_transfers.load(Ordering::Relaxed), 1);
        
        metrics.record_transfer_complete(1024, 1000);
        assert_eq!(metrics.active_transfers.load(Ordering::Relaxed), 0);
        assert_eq!(metrics.completed_transfers.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.total_bytes_transferred.load(Ordering::Relaxed), 1024);
    }

    #[tokio::test]
    async fn test_observability_logging() {
        let obs = ObservabilityManager::new(100);
        
        obs.log_event(
            "test_transfer",
            LogLevel::Info,
            "Test message".to_string(),
            Some(TransferOperation::Upload),
            Some(TransferStatus::Transferring),
            None,
        ).await;
        
        let logs = obs.get_recent_logs(None).await;
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].transfer_id, "test_transfer");
        assert!(matches!(logs[0].level, LogLevel::Info));
    }

    #[tokio::test]
    async fn test_log_level_filtering() {
        let obs = ObservabilityManager::new(100);
        obs.set_log_level(LogLevel::Warning).await;
        
        // This should not be logged
        obs.log_event(
            "test_transfer",
            LogLevel::Info,
            "Info message".to_string(),
            None,
            None,
            None,
        ).await;
        
        // This should be logged
        obs.log_event(
            "test_transfer",
            LogLevel::Warning,
            "Warning message".to_string(),
            None,
            None,
            None,
        ).await;
        
        let logs = obs.get_recent_logs(None).await;
        assert_eq!(logs.len(), 1);
        assert!(matches!(logs[0].level, LogLevel::Warning));
    }
}
