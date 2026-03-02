//! Checkpoint management for resume support
//!
//! This module handles saving and loading transfer checkpoints for
//! resumable file transfers.

use crate::ssh::transfer::types::{TransferError, TransferOperation, TransferSettings};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Transfer checkpoint for resume support
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferCheckpoint {
    /// Unique transfer identifier
    pub transfer_id: String,
    /// Client ID for this transfer
    pub client_id: String,
    /// Transfer operation type
    pub operation: TransferOperation,

    // File information
    pub local_path: PathBuf,
    pub remote_path: String,
    pub file_size: u64,

    // Progress information
    pub transferred: u64,
    pub chunk_size: usize,

    // Checksum information
    pub use_checksum: bool,
    pub local_checksum: Option<String>,
    pub remote_checksum: Option<String>,

    // Time information
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl TransferCheckpoint {
    /// Create a new checkpoint
    pub fn new(
        transfer_id: String,
        client_id: String,
        operation: TransferOperation,
        local_path: PathBuf,
        remote_path: String,
        file_size: u64,
        settings: &TransferSettings,
    ) -> Self {
        let now = Utc::now();
        Self {
            transfer_id,
            client_id,
            operation,
            local_path,
            remote_path,
            file_size,
            transferred: 0,
            chunk_size: settings.default_chunk_size,
            use_checksum: settings.verify_checksum,
            local_checksum: None,
            remote_checksum: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Update transferred bytes
    pub fn update_transferred(&mut self, bytes: u64) {
        self.transferred = bytes;
        self.updated_at = Utc::now();
    }

    /// Get checkpoint file path for a given transfer ID
    fn checkpoint_path(checkpoint_dir: &Path, transfer_id: &str) -> PathBuf {
        checkpoint_dir.join(format!("transfer_{}.json", transfer_id))
    }

    /// Check if this checkpoint is still valid (file hasn't changed)
    pub fn is_valid(&self) -> Result<bool, TransferError> {
        // Check if local file still exists
        if !self.local_path.exists() {
            return Ok(false);
        }

        // Check if local file size matches or is at least as large as transferred
        let metadata =
            fs::metadata(&self.local_path).map_err(|e| TransferError::InvalidPath(format!("Failed to stat local file: {}", e)))?;

        match self.operation {
            TransferOperation::Upload => {
                // For upload, local file should exist and be at least as large as transferred
                Ok(metadata.len() >= self.file_size)
            }
            TransferOperation::Download => {
                // For download, local file should exist and be at least as large as transferred
                Ok(metadata.len() >= self.transferred)
            }
        }
    }

    /// Calculate progress percentage
    pub fn progress(&self) -> f64 {
        if self.file_size == 0 {
            0.0
        } else {
            (self.transferred as f64 / self.file_size as f64).min(1.0)
        }
    }
}

/// Checkpoint manager for saving and loading transfer checkpoints
pub struct CheckpointManager {
    /// Directory to store checkpoints
    checkpoint_dir: PathBuf,
}

impl CheckpointManager {
    /// Create a new checkpoint manager
    pub fn new(checkpoint_dir: PathBuf) -> Result<Self, TransferError> {
        // Ensure checkpoint directory exists
        fs::create_dir_all(&checkpoint_dir).map_err(|e| {
            TransferError::InvalidPath(format!("Failed to create checkpoint directory: {}", e))
        })?;

        Ok(Self { checkpoint_dir })
    }

    /// Create checkpoint manager from app handle
    pub fn from_app_data_dir(app_data_dir: &Path) -> Result<Self, TransferError> {
        let checkpoint_dir = app_data_dir.join("transfer_checkpoints");
        Self::new(checkpoint_dir)
    }

    /// Save a checkpoint
    pub fn save_checkpoint(&self, checkpoint: &TransferCheckpoint) -> Result<(), TransferError> {
        let path = TransferCheckpoint::checkpoint_path(&self.checkpoint_dir, &checkpoint.transfer_id);

        let json =
            serde_json::to_string_pretty(checkpoint).map_err(|e| TransferError::Unknown(format!("Failed to serialize checkpoint: {}", e)))?;

        // Atomic write: write to temp file then rename
        let temp_path = path.with_extension("tmp");
        fs::write(&temp_path, json)
            .map_err(|e| TransferError::Unknown(format!("Failed to write checkpoint file: {}", e)))?;

        fs::rename(&temp_path, &path)
            .map_err(|e| TransferError::Unknown(format!("Failed to rename checkpoint file: {}", e)))?;

        Ok(())
    }

    /// Load a checkpoint
    pub fn load_checkpoint(&self, transfer_id: &str) -> Result<Option<TransferCheckpoint>, TransferError> {
        let path = TransferCheckpoint::checkpoint_path(&self.checkpoint_dir, transfer_id);

        if !path.exists() {
            return Ok(None);
        }

        let json = fs::read_to_string(&path)
            .map_err(|e| TransferError::Unknown(format!("Failed to read checkpoint file: {}", e)))?;

        let checkpoint: TransferCheckpoint =
            serde_json::from_str(&json).map_err(|e| TransferError::InvalidCheckpoint)?;

        Ok(Some(checkpoint))
    }

    /// Delete a checkpoint
    pub fn delete_checkpoint(&self, transfer_id: &str) -> Result<(), TransferError> {
        let path = TransferCheckpoint::checkpoint_path(&self.checkpoint_dir, transfer_id);

        if path.exists() {
            fs::remove_file(&path)
                .map_err(|e| TransferError::Unknown(format!("Failed to delete checkpoint: {}", e)))?;
        }

        Ok(())
    }

    /// List all checkpoints for a client
    pub fn list_checkpoints(&self, client_id: &str) -> Result<Vec<TransferCheckpoint>, TransferError> {
        let mut checkpoints = Vec::new();

        let entries =
            fs::read_dir(&self.checkpoint_dir).map_err(|e| TransferError::Unknown(format!("Failed to read checkpoint directory: {}", e)))?;

        for entry in entries {
            let entry = entry.map_err(|e| TransferError::Unknown(format!("Failed to read directory entry: {}", e)))?;
            let path = entry.path();

            // Only process JSON files
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }

            // Read and parse checkpoint
            let json = fs::read_to_string(&path)
                .map_err(|e| TransferError::Unknown(format!("Failed to read checkpoint file: {}", e)))?;

            let checkpoint: TransferCheckpoint = serde_json::from_str(&json)
                .map_err(|e| TransferError::InvalidCheckpoint)?;

            // Filter by client ID
            if checkpoint.client_id == client_id {
                checkpoints.push(checkpoint);
            }
        }

        // Sort by updated time (newest first)
        checkpoints.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

        Ok(checkpoints)
    }

    /// Clean up old checkpoints
    pub fn cleanup_old_checkpoints(&self, max_age_days: i64) -> Result<usize, TransferError> {
        let cutoff = Utc::now() - chrono::Duration::days(max_age_days);
        let mut removed = 0;

        let entries =
            fs::read_dir(&self.checkpoint_dir).map_err(|e| TransferError::Unknown(format!("Failed to read checkpoint directory: {}", e)))?;

        for entry in entries {
            let entry = entry.map_err(|e| TransferError::Unknown(format!("Failed to read directory entry: {}", e)))?;
            let path = entry.path();

            // Only process JSON files
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }

            // Read checkpoint to check date
            if let Ok(json) = fs::read_to_string(&path) {
                if let Ok(checkpoint) = serde_json::from_str::<TransferCheckpoint>(&json) {
                    if checkpoint.updated_at < cutoff {
                        // Delete old checkpoint
                        if fs::remove_file(&path).is_ok() {
                            removed += 1;
                        }
                    }
                }
            }
        }

        Ok(removed)
    }

    /// Verify a checkpoint is still valid
    pub fn verify_checkpoint(&self, checkpoint: &TransferCheckpoint) -> Result<bool, TransferError> {
        checkpoint.is_valid()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_checkpoint_creation() {
        let checkpoint = TransferCheckpoint::new(
            "test-id".to_string(),
            "client-1".to_string(),
            TransferOperation::Upload,
            PathBuf::from("/tmp/test.txt"),
            "/remote/test.txt".to_string(),
            1024,
            &TransferSettings::default(),
        );

        assert_eq!(checkpoint.transfer_id, "test-id");
        assert_eq!(checkpoint.client_id, "client-1");
        assert_eq!(checkpoint.transferred, 0);
        assert!((checkpoint.progress() - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_checkpoint_progress() {
        let mut checkpoint = TransferCheckpoint::new(
            "test-id".to_string(),
            "client-1".to_string(),
            TransferOperation::Upload,
            PathBuf::from("/tmp/test.txt"),
            "/remote/test.txt".to_string(),
            1024,
            &TransferSettings::default(),
        );

        checkpoint.update_transferred(512);
        assert_eq!(checkpoint.transferred, 512);
        assert!((checkpoint.progress() - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_checkpoint_save_load() {
        let temp_dir = TempDir::new().unwrap();
        let manager = CheckpointManager::new(temp_dir.path().to_path_buf()).unwrap();

        let checkpoint = TransferCheckpoint::new(
            "test-id".to_string(),
            "client-1".to_string(),
            TransferOperation::Upload,
            PathBuf::from("/tmp/test.txt"),
            "/remote/test.txt".to_string(),
            1024,
            &TransferSettings::default(),
        );

        // Save checkpoint
        manager.save_checkpoint(&checkpoint).unwrap();

        // Load checkpoint
        let loaded = manager.load_checkpoint("test-id").unwrap().unwrap();

        assert_eq!(loaded.transfer_id, checkpoint.transfer_id);
        assert_eq!(loaded.client_id, checkpoint.client_id);
        assert_eq!(loaded.transferred, checkpoint.transferred);
    }

    #[test]
    fn test_checkpoint_delete() {
        let temp_dir = TempDir::new().unwrap();
        let manager = CheckpointManager::new(temp_dir.path().to_path_buf()).unwrap();

        let checkpoint = TransferCheckpoint::new(
            "test-id".to_string(),
            "client-1".to_string(),
            TransferOperation::Upload,
            PathBuf::from("/tmp/test.txt"),
            "/remote/test.txt".to_string(),
            1024,
            &TransferSettings::default(),
        );

        manager.save_checkpoint(&checkpoint).unwrap();

        // Verify exists
        assert!(manager.load_checkpoint("test-id").unwrap().is_some());

        // Delete
        manager.delete_checkpoint("test-id").unwrap();

        // Verify gone
        assert!(manager.load_checkpoint("test-id").unwrap().is_none());
    }

    #[test]
    fn test_list_checkpoints() {
        let temp_dir = TempDir::new().unwrap();
        let manager = CheckpointManager::new(temp_dir.path().to_path_buf()).unwrap();

        // Create multiple checkpoints
        for i in 0..3 {
            let checkpoint = TransferCheckpoint::new(
                format!("test-id-{}", i),
                "client-1".to_string(),
                TransferOperation::Upload,
                PathBuf::from(format!("/tmp/test{}.txt", i)),
                format!("/remote/test{}.txt", i),
                1024,
                &TransferSettings::default(),
            );
            manager.save_checkpoint(&checkpoint).unwrap();
        }

        // List checkpoints
        let checkpoints = manager.list_checkpoints("client-1").unwrap();

        assert_eq!(checkpoints.len(), 3);
    }

    #[test]
    fn test_checkpoint_is_valid_upload() {
        let temp_file = TempDir::new().unwrap();
        let file_path = temp_file.path().join("test.txt");

        // Create test file
        fs::write(&file_path, b"test content").unwrap();

        let checkpoint = TransferCheckpoint::new(
            "test-id".to_string(),
            "client-1".to_string(),
            TransferOperation::Upload,
            file_path.clone(),
            "/remote/test.txt".to_string(),
            12, // file size
            &TransferSettings::default(),
        );

        assert!(checkpoint.is_valid().unwrap());
    }
}
