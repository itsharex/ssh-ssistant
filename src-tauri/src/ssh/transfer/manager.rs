//! Transfer manager implementation
//!
//! This module provides the main TransferManager that orchestrates
//! file transfers with state management, connection pooling, and
//! checkpoint support.

use crate::ssh::transfer::async_sftp::AsyncSftp;
use crate::ssh::transfer::checkpoint::{CheckpointManager, TransferCheckpoint};
use crate::ssh::transfer::pool::{PoolStats, TransferPool};
use crate::ssh::transfer::state::TransferState;
use crate::ssh::transfer::types::{TransferError, TransferEvent, TransferHealth, TransferOperation, TransferSettings, TransferStatus};
use crate::models::Connection as SshConnConfig;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tauri::AppHandle;
use tokio::sync::Mutex;

/// Transfer manager for handling file transfers
pub struct TransferManager {
    /// Transfer configuration
    config: SshConnConfig,
    /// Transfer settings
    settings: TransferSettings,
    /// Connection pool for transfers
    pool: Arc<Mutex<TransferPool>>,
    /// Active transfers by ID
    transfers: Arc<Mutex<HashMap<String, Arc<TransferState>>>>,
    /// Checkpoint manager
    checkpoint_manager: Arc<CheckpointManager>,
    /// Event sender for frontend notifications
    event_sender: Option<tokio::sync::mpsc::UnboundedSender<TransferEvent>>,
    /// Statistics tracking
    stats: Arc<Mutex<TransferStats>>,
    /// Next transfer ID
    next_id: Arc<AtomicUsize>,
}

/// Transfer statistics for monitoring
#[derive(Debug, Default)]
struct TransferStats {
    total_transfers: usize,
    active_transfers: usize,
    completed_transfers: usize,
    failed_transfers: usize,
    cancelled_transfers: usize,
    total_bytes_transferred: u64,
    last_transfer_time: Option<Instant>,
}

impl TransferManager {
    /// Create a new transfer manager
    pub fn new(
        config: SshConnConfig,
        settings: TransferSettings,
        app_data_dir: PathBuf,
    ) -> Result<Self, TransferError> {
        let pool = TransferPool::new(config.clone(), settings.clone());
        let checkpoint_manager = CheckpointManager::from_app_data_dir(&app_data_dir)?;

        Ok(Self {
            config,
            settings,
            pool: Arc::new(Mutex::new(pool)),
            transfers: Arc::new(Mutex::new(HashMap::new())),
            checkpoint_manager: Arc::new(checkpoint_manager),
            event_sender: None,
            stats: Arc::new(Mutex::new(TransferStats::default())),
            next_id: Arc::new(AtomicUsize::new(1)),
        })
    }

    /// Set event sender for frontend notifications
    pub fn set_event_sender(&mut self, sender: tokio::sync::mpsc::UnboundedSender<TransferEvent>) {
        self.event_sender = Some(sender);
    }

    /// Start a new transfer
    pub async fn start_transfer(
        &self,
        operation: TransferOperation,
        local_path: PathBuf,
        remote_path: String,
        app_handle: AppHandle,
    ) -> Result<String, TransferError> {
        // Generate transfer ID
        let transfer_id = format!("transfer_{}", self.next_id.fetch_add(1, Ordering::Relaxed));

        // Get file size
        let file_size = std::fs::metadata(&local_path)
            .map_err(|e| TransferError::InvalidPath(format!("Failed to stat file: {}", e)))?
            .len();

        // Create transfer state
        let state = Arc::new(TransferState::new(transfer_id.clone(), file_size));

        // Create checkpoint if resume is enabled
        if self.settings.enable_resume {
            let checkpoint = TransferCheckpoint::new(
                transfer_id.clone(),
                self.config
                    .id
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                operation,
                local_path.clone(),
                remote_path.clone(),
                file_size,
                &self.settings,
            );

            self.checkpoint_manager
                .save_checkpoint(&checkpoint)
                .map_err(|e| TransferError::Unknown(format!("Failed to save checkpoint: {}", e)))?;
        }

        // Register transfer
        self.transfers
            .blocking_lock()
            .insert(transfer_id.clone(), state.clone());

        // Update stats
        self.update_stats(|stats| {
            stats.total_transfers += 1;
            stats.active_transfers += 1;
        });

        // Emit started event
        self.emit_event(TransferEvent::Started {
            id: transfer_id.clone(),
            operation,
        });

        // Spawn transfer task
        let state_clone = state.clone();
        let pool_clone = self.pool.clone();
        let settings_clone = self.settings.clone();
        let checkpoint_manager_clone = Arc::clone(&self.checkpoint_manager);
        let transfers_clone = self.transfers.clone();
        let stats_clone = self.stats.clone();
        let event_sender_clone = self.event_sender.clone();
        let local_path_clone = local_path.clone();
        let remote_path_clone = remote_path.clone();
        let transfer_id_clone = transfer_id.clone();
        let app_handle_clone = app_handle.clone();

        tokio::spawn(async move {
            let checkpoint_manager_for_delete = Arc::clone(&checkpoint_manager_clone);
            let result = Self::execute_transfer(
                state_clone.clone(),
                pool_clone,
                &settings_clone,
                checkpoint_manager_clone,
                operation,
                local_path_clone,
                remote_path_clone,
                app_handle_clone,
            )
            .await;

            // Handle completion
            match result {
                Ok(bytes) => {
                    state_clone.complete().unwrap();
                    let duration = state_clone.elapsed().as_secs();

                    // Update stats
                    {
                        let mut stats = stats_clone.lock().await;
                        stats.active_transfers = stats.active_transfers.saturating_sub(1);
                        stats.completed_transfers += 1;
                        stats.total_bytes_transferred += bytes;
                        stats.last_transfer_time = Some(Instant::now());
                    }

                    // Remove from active transfers
                    transfers_clone
                        .lock()
                        .await
                        .remove(&transfer_id_clone);

                    // Delete checkpoint
                    let _ = checkpoint_manager_for_delete.delete_checkpoint(&transfer_id_clone);

                    // Emit completed event
                    if let Some(sender) = event_sender_clone {
                        let _ = sender.send(TransferEvent::Completed {
                            id: transfer_id_clone,
                            duration_secs: duration,
                            total_bytes: bytes,
                        });
                    }
                }
                Err(e) => {
                    let transferred = state_clone.transferred();
                    state_clone.fail(e.to_string()).unwrap();

                    // Update stats
                    {
                        let mut stats = stats_clone.lock().await;
                        stats.active_transfers = stats.active_transfers.saturating_sub(1);
                        stats.failed_transfers += 1;
                    }

                    // Remove from active transfers (keep checkpoint for resume)
                    transfers_clone
                        .lock()
                        .await
                        .remove(&transfer_id_clone);

                    // Emit failed event
                    if let Some(sender) = event_sender_clone {
                        let _ = sender.send(TransferEvent::Failed {
                            id: transfer_id_clone,
                            error: e.to_string(),
                            transferred,
                        });
                    }
                }
            }
        });

        Ok(transfer_id)
    }

    /// Pause a transfer
    pub fn pause_transfer(&self, transfer_id: &str) -> Result<(), TransferError> {
        let transfers = self.transfers.blocking_lock();

        if let Some(state) = transfers.get(transfer_id) {
            state.pause()?;
            self.emit_event(TransferEvent::Paused {
                id: transfer_id.to_string(),
                transferred: state.transferred(),
            });
            Ok(())
        } else {
            Err(TransferError::CannotResume(format!(
                "Transfer not found: {}",
                transfer_id
            )))
        }
    }

    /// Resume a paused or failed transfer
    pub async fn resume_transfer(&self, transfer_id: &str) -> Result<(), TransferError> {
        // Try to load checkpoint first
        let checkpoint = self
            .checkpoint_manager
            .load_checkpoint(transfer_id)?
            .ok_or_else(|| TransferError::CannotResume(format!("No checkpoint found for transfer: {}", transfer_id)))?;

        // Verify checkpoint is still valid
        if !self.checkpoint_manager.verify_checkpoint(&checkpoint)? {
            return Err(TransferError::CheckpointMismatch(
                "Checkpoint is no longer valid".to_string(),
            ));
        }

        // Get or create transfer state
        let state = {
            let mut transfers = self.transfers.blocking_lock();

            if let Some(existing) = transfers.get(transfer_id) {
                existing.clone()
            } else {
                let new_state = Arc::new(TransferState::new(transfer_id.to_string(), checkpoint.file_size));
                transfers.insert(transfer_id.to_string(), new_state.clone());
                new_state
            }
        };

        // Resume the transfer
        state.resume()?;

        // Update stats
        self.update_stats(|stats| {
            stats.active_transfers += 1;
        });

        // Emit resumed event
        self.emit_event(TransferEvent::Resumed {
            id: transfer_id.to_string(),
            from_offset: checkpoint.transferred,
        });

        // Spawn resume task
        let state_clone = state.clone();
        let pool_clone = self.pool.clone();
        let settings_clone = self.settings.clone();
        let checkpoint_manager_clone = Arc::clone(&self.checkpoint_manager);
        let transfers_clone = self.transfers.clone();
        let stats_clone = self.stats.clone();
        let event_sender_clone = self.event_sender.clone();
        let local_path = checkpoint.local_path.clone();
        let remote_path = checkpoint.remote_path.clone();
        let transfer_id_clone = transfer_id.to_string();
        let offset = checkpoint.transferred;

        tokio::spawn(async move {
            let checkpoint_manager_for_delete = Arc::clone(&checkpoint_manager_clone);
            let result = Self::execute_resume_transfer(
                state_clone.clone(),
                pool_clone,
                &settings_clone,
                checkpoint_manager_clone,
                checkpoint.operation,
                local_path,
                remote_path,
                offset,
            )
            .await;

            // Handle completion (same as start_transfer)
            match result {
                Ok(bytes) => {
                    state_clone.complete().unwrap();
                    let duration = state_clone.elapsed().as_secs();

                    {
                        let mut stats = stats_clone.lock().await;
                        stats.active_transfers = stats.active_transfers.saturating_sub(1);
                        stats.completed_transfers += 1;
                        stats.total_bytes_transferred += bytes;
                        stats.last_transfer_time = Some(Instant::now());
                    }

                    transfers_clone
                        .lock()
                        .await
                        .remove(&transfer_id_clone);

                    let _ = checkpoint_manager_for_delete.delete_checkpoint(&transfer_id_clone);

                    if let Some(sender) = event_sender_clone {
                        let _ = sender.send(TransferEvent::Completed {
                            id: transfer_id_clone,
                            duration_secs: duration,
                            total_bytes: bytes,
                        });
                    }
                }
                Err(e) => {
                    let transferred = state_clone.transferred();
                    state_clone.fail(e.to_string()).unwrap();

                    {
                        let mut stats = stats_clone.lock().await;
                        stats.active_transfers = stats.active_transfers.saturating_sub(1);
                        stats.failed_transfers += 1;
                    }

                    if let Some(sender) = event_sender_clone {
                        let _ = sender.send(TransferEvent::Failed {
                            id: transfer_id_clone,
                            error: e.to_string(),
                            transferred,
                        });
                    }
                }
            }
        });

        Ok(())
    }

    /// Cancel a transfer
    pub fn cancel_transfer(&self, transfer_id: &str) -> Result<(), TransferError> {
        let transfers = self.transfers.blocking_lock();

        if let Some(state) = transfers.get(transfer_id) {
            state.cancel()?;
            let transferred = state.transferred();

            // Update stats
            self.update_stats(|stats| {
                stats.active_transfers = stats.active_transfers.saturating_sub(1);
                stats.cancelled_transfers += 1;
            });

            // Emit cancelled event
            self.emit_event(TransferEvent::Cancelled {
                id: transfer_id.to_string(),
                transferred,
            });

            Ok(())
        } else {
            Err(TransferError::CannotResume(format!(
                "Transfer not found: {}",
                transfer_id
            )))
        }
    }

    /// Get health status of the transfer system
    pub async fn health_check(&self) -> TransferHealth {
        let stuck_timeout = self.settings.no_progress_timeout();

        let transfers = self.transfers.lock().await;
        let stats = self.stats.lock().await;

        let mut health = TransferHealth {
            active_transfers: transfers.len(),
            stuck_transfers: 0,
            failed_transfers: stats.failed_transfers,
            avg_speed_bps: 0.0,
            pool_usage: 0.0,
        };

        // Count stuck transfers
        for state in transfers.values() {
            if state.is_stuck(stuck_timeout) {
                health.stuck_transfers += 1;
            }
        }

        // Calculate average speed from recent transfers
        if let Some(last_time) = stats.last_transfer_time {
            let elapsed = last_time.elapsed().as_secs_f64();
            if elapsed > 0.0 && stats.total_bytes_transferred > 0 {
                health.avg_speed_bps = stats.total_bytes_transferred as f64 / elapsed;
            }
        }

        // Calculate pool usage
        let pool = self.pool.lock().await;
        let total_conns = pool.total_connections();
        let idle_conns = pool.total_idle_connections();

        if total_conns > 0 {
            health.pool_usage = (total_conns - idle_conns) as f64 / total_conns as f64;
        }

        health
    }

    /// Get transfer status
    pub fn get_transfer_status(&self, transfer_id: &str) -> Option<TransferStatus> {
        let transfers = self.transfers.blocking_lock();
        transfers.get(transfer_id).map(|s| s.status())
    }

    /// Get pool statistics
    pub fn get_pool_stats(&self, client_id: &str) -> Option<PoolStats> {
        let pool = self.pool.blocking_lock();
        pool.stats(client_id)
    }

    /// Execute a transfer
    async fn execute_transfer(
        state: Arc<TransferState>,
        pool: Arc<Mutex<TransferPool>>,
        settings: &TransferSettings,
        checkpoint_manager: Arc<CheckpointManager>,
        operation: TransferOperation,
        local_path: PathBuf,
        remote_path: String,
        app_handle: AppHandle,
    ) -> Result<u64, TransferError> {
        // Update state to connecting
        state.start()?;

        // Acquire connection from pool
        let conn = {
            let mut pool_guard = pool.lock().await;
            pool_guard.acquire("default").await
                .map_err(|e| TransferError::Unknown(e))?
        };
        let conn_for_release = conn.clone();
        let mut conn_guard = conn.lock().await;

        // Update state to transferring
        state.begin_transfer()?;

        // Get SFTP channel
        let sftp = conn_guard.sftp()?;

        // Create async SFTP wrapper
        let mut async_sftp = AsyncSftp::new(sftp, settings);

        // Cancel flag
        let cancel_flag = Arc::new(AtomicBool::new(false));

        // Progress callback
        let state_clone = state.clone();
        let checkpoint_manager_clone = Arc::clone(&checkpoint_manager);
        let transfer_id = state.id().to_string();
        let checkpoint_interval = settings.checkpoint_interval();
        let last_checkpoint = Arc::new(Mutex::new(Instant::now()));

        let progress_callback = move |transferred: u64, _total: u64| {
            state_clone.update_transferred(transferred);

            // Periodic checkpoint save
            let mut last = last_checkpoint.blocking_lock();
            if last.elapsed() >= checkpoint_interval {
                *last = Instant::now();
                if let Some(mut checkpoint) = checkpoint_manager_clone.load_checkpoint(&transfer_id).ok().flatten() {
                    checkpoint.update_transferred(transferred);
                    let _ = checkpoint_manager_clone.save_checkpoint(&checkpoint);
                }
            }

            // Emit progress event to frontend
            let _ = state_clone; // Use state to avoid unused warning

            // Calculate speed if needed (currently not used)
            let _speed = 0.0;

            // Note: In real implementation, emit to frontend
            // For now, we'll just track internally
        };

        // Execute operation
        let result = match operation {
            TransferOperation::Download => {
                async_sftp
                    .download_with_timeout(&remote_path, &local_path, progress_callback, &cancel_flag)
                    .await
            }
            TransferOperation::Upload => {
                async_sftp
                    .upload_with_timeout(&local_path, &remote_path, progress_callback, &cancel_flag)
                    .await
            }
        };

        // Release connection back to pool
        {
            let mut pool_guard = pool.lock().await;
            pool_guard.release("default", conn_for_release);
        }

        result
    }

    /// Execute a resume transfer
    async fn execute_resume_transfer(
        state: Arc<TransferState>,
        pool: Arc<Mutex<TransferPool>>,
        settings: &TransferSettings,
        checkpoint_manager: Arc<CheckpointManager>,
        operation: TransferOperation,
        local_path: PathBuf,
        remote_path: String,
        offset: u64,
    ) -> Result<u64, TransferError> {
        // Update state to resuming
        state.start()?;
        state.begin_transfer()?;

        // Acquire connection from pool
        let conn = {
            let mut pool_guard = pool.lock().await;
            pool_guard.acquire("default").await
                .map_err(|e| TransferError::Unknown(e))?
        };
        let conn_for_release = conn.clone();
        let mut conn_guard = conn.lock().await;

        // Get SFTP channel
        let sftp = conn_guard.sftp()?;

        // Create async SFTP wrapper
        let mut async_sftp = AsyncSftp::new(sftp, settings);

        // Cancel flag
        let cancel_flag = Arc::new(AtomicBool::new(false));

        // Get file size
        let file_size = state.total();

        // Progress callback
        let state_clone = state.clone();
        let checkpoint_manager_clone = Arc::clone(&checkpoint_manager);
        let transfer_id = state.id().to_string();
        let checkpoint_interval = settings.checkpoint_interval();
        let last_checkpoint = Arc::new(Mutex::new(Instant::now()));

        let progress_callback = move |transferred: u64, _total: u64| {
            state_clone.update_transferred(transferred);

            // Periodic checkpoint save
            let mut last = last_checkpoint.blocking_lock();
            if last.elapsed() >= checkpoint_interval {
                *last = Instant::now();
                if let Some(mut checkpoint) = checkpoint_manager_clone.load_checkpoint(&transfer_id).ok().flatten() {
                    checkpoint.update_transferred(transferred);
                    let _ = checkpoint_manager_clone.save_checkpoint(&checkpoint);
                }
            }
        };

        // Execute resume operation
        let result = match operation {
            TransferOperation::Download => {
                async_sftp
                    .resume_download(&remote_path, &local_path, offset, file_size, progress_callback, &cancel_flag)
                    .await
            }
            TransferOperation::Upload => {
                async_sftp
                    .resume_upload(&local_path, &remote_path, offset, file_size, progress_callback, &cancel_flag)
                    .await
            }
        };

        // Release connection back to pool
        {
            let mut pool_guard = pool.lock().await;
            pool_guard.release("default", conn_for_release);
        }

        result
    }

    /// Update statistics with a closure
    fn update_stats<F>(&self, f: F)
    where
        F: FnOnce(&mut TransferStats),
    {
        let mut stats = self.stats.blocking_lock();
        f(&mut stats);
    }

    /// Emit a transfer event
    fn emit_event(&self, event: TransferEvent) {
        if let Some(ref sender) = self.event_sender {
            let _ = sender.send(event);
        }
    }

    /// Clean up resources
    pub async fn cleanup(&self) {
        // Clean up idle connections
        {
            let mut pool = self.pool.lock().await;
            pool.cleanup_idle().await;
        }

        // Clean up old checkpoints
        let _ = self.checkpoint_manager.cleanup_old_checkpoints(7); // 7 days
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manager_creation() {
        let temp_dir = tempfile::tempdir().unwrap();

        let config = SshConnConfig {
            id: Some(1),
            name: "test".to_string(),
            host: "localhost".to_string(),
            port: 22,
            username: "test".to_string(),
            password: None,
            auth_type: None,
            ssh_key_id: None,
            jump_host: None,
            jump_port: None,
            jump_username: None,
            jump_password: None,
            group_id: None,
            os_type: None,
            key_content: None,
            key_passphrase: None,
        };

        let settings = TransferSettings::default();

        let manager = TransferManager::new(config, settings, temp_dir.path().to_path_buf());

        assert!(manager.is_ok());
    }

    #[test]
    fn test_transfer_stats() {
        let stats = TransferStats::default();
        assert_eq!(stats.total_transfers, 0);
        assert_eq!(stats.active_transfers, 0);
    }
}
