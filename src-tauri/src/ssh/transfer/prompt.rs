//! User-friendly prompt system for file transfers
//!
//! This module provides comprehensive user interaction capabilities
//! including confirmations, warnings, error messages, and progress feedback.

use crate::ssh::transfer::types::{TransferError, TransferOperation, TransferStatus};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use tauri::{AppHandle, Emitter};

/// Prompt types for user interactions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PromptType {
    /// Confirmation before starting transfer
    Confirmation,
    /// Warning about potential issues
    Warning,
    /// Error message with suggestions
    Error,
    /// Information about transfer progress
    Info,
    /// Question requiring user input
    Question,
}

/// Prompt severity levels
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PromptSeverity {
    Low,
    Medium,
    High,
    Critical,
}

/// User prompt with rich context
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferPrompt {
    /// Unique prompt ID
    pub id: String,
    /// Prompt type
    pub prompt_type: PromptType,
    /// Severity level
    pub severity: PromptSeverity,
    /// Title/header
    pub title: String,
    /// Main message
    pub message: String,
    /// Detailed description
    pub description: Option<String>,
    /// Transfer ID if applicable
    pub transfer_id: Option<String>,
    /// Operation type
    pub operation: Option<TransferOperation>,
    /// Current status
    pub status: Option<TransferStatus>,
    /// Available actions
    pub actions: Vec<PromptAction>,
    /// Additional context
    pub context: HashMap<String, String>,
    /// Timestamp
    pub timestamp: u64,
    /// Whether prompt requires user response
    pub requires_response: bool,
}

/// Available actions for prompts
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptAction {
    /// Action ID
    pub id: String,
    /// Display label
    pub label: String,
    /// Action style (primary, secondary, danger)
    pub style: String,
    /// Whether this is the default action
    pub is_default: bool,
}

/// User response to a prompt
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptResponse {
    /// Prompt ID
    pub prompt_id: String,
    /// Action ID that was selected
    pub action_id: String,
    /// Additional user input
    pub user_input: Option<String>,
    /// Timestamp
    pub timestamp: u64,
}

/// Progress notification for user feedback
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressNotification {
    /// Transfer ID
    pub transfer_id: String,
    /// Operation type
    pub operation: TransferOperation,
    /// Current status
    pub status: TransferStatus,
    /// Progress percentage (0-100)
    pub percentage: f64,
    /// Transferred bytes
    pub transferred_bytes: u64,
    /// Total bytes
    pub total_bytes: u64,
    /// Current speed (bytes per second)
    pub speed_bps: f64,
    /// Estimated time remaining (seconds)
    pub eta_seconds: Option<u64>,
    /// Current file being processed
    pub current_file: Option<String>,
    /// Message
    pub message: String,
}

/// Prompt manager for user interactions
pub struct PromptManager {
    app_handle: AppHandle,
    next_prompt_id: std::sync::atomic::AtomicU64,
}

impl PromptManager {
    /// Create a new prompt manager
    pub fn new(app_handle: AppHandle) -> Self {
        Self {
            app_handle,
            next_prompt_id: std::sync::atomic::AtomicU64::new(1),
        }
    }

    /// Generate next prompt ID
    fn next_id(&self) -> String {
        format!("prompt_{}", self.next_prompt_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }

    /// Send a prompt to the frontend
    async fn send_prompt(&self, prompt: TransferPrompt) -> Result<(), String> {
        self.app_handle
            .emit("transfer-prompt", &prompt)
            .map_err(|e| format!("Failed to emit prompt: {}", e))?;
        Ok(())
    }

    /// Show confirmation dialog before transfer
    pub async fn confirm_transfer(
        &self,
        transfer_id: &str,
        operation: TransferOperation,
        local_path: &str,
        remote_path: &str,
        file_size: u64,
    ) -> Result<bool, String> {
        let prompt = TransferPrompt {
            id: self.next_id(),
            prompt_type: PromptType::Confirmation,
            severity: PromptSeverity::Medium,
            title: format!("Confirm {}", operation),
            message: format!(
                "Are you sure you want to {} {}?",
                operation,
                Path::new(local_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file")
            ),
            description: Some(format!(
                "Local: {}\nRemote: {}\nSize: {}",
                local_path,
                remote_path,
                self.format_bytes(file_size)
            )),
            transfer_id: Some(transfer_id.to_string()),
            operation: Some(operation),
            status: None,
            actions: vec![
                PromptAction {
                    id: "confirm".to_string(),
                    label: format!("Yes, {}", operation),
                    style: "primary".to_string(),
                    is_default: true,
                },
                PromptAction {
                    id: "cancel".to_string(),
                    label: "Cancel".to_string(),
                    style: "secondary".to_string(),
                    is_default: false,
                },
            ],
            context: HashMap::new(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            requires_response: true,
        };

        self.send_prompt(prompt).await?;
        // In a real implementation, we'd wait for the response
        // For now, we'll assume the user confirms
        Ok(true)
    }

    /// Show warning about potential issues
    pub async fn show_warning(
        &self,
        transfer_id: &str,
        operation: TransferOperation,
        warning: &str,
        suggestion: Option<&str>,
    ) -> Result<(), String> {
        let prompt = TransferPrompt {
            id: self.next_id(),
            prompt_type: PromptType::Warning,
            severity: PromptSeverity::Medium,
            title: "Transfer Warning".to_string(),
            message: warning.to_string(),
            description: suggestion.map(|s| s.to_string()),
            transfer_id: Some(transfer_id.to_string()),
            operation: Some(operation),
            status: None,
            actions: vec![
                PromptAction {
                    id: "acknowledge".to_string(),
                    label: "OK".to_string(),
                    style: "primary".to_string(),
                    is_default: true,
                },
                PromptAction {
                    id: "cancel".to_string(),
                    label: "Cancel Transfer".to_string(),
                    style: "danger".to_string(),
                    is_default: false,
                },
            ],
            context: HashMap::new(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            requires_response: true,
        };

        self.send_prompt(prompt).await
    }

    /// Show error message with recovery suggestions
    pub async fn show_error(
        &self,
        transfer_id: &str,
        operation: TransferOperation,
        error: &TransferError,
        transferred_bytes: u64,
    ) -> Result<(), String> {
        let (title, message, suggestion) = self.format_error(error, transferred_bytes);

        let prompt = TransferPrompt {
            id: self.next_id(),
            prompt_type: PromptType::Error,
            severity: PromptSeverity::High,
            title,
            message,
            description: Some(suggestion),
            transfer_id: Some(transfer_id.to_string()),
            operation: Some(operation),
            status: Some(TransferStatus::Failed),
            actions: vec![
                PromptAction {
                    id: "retry".to_string(),
                    label: "Retry".to_string(),
                    style: "primary".to_string(),
                    is_default: error.is_retryable(),
                },
                PromptAction {
                    id: "resume".to_string(),
                    label: "Resume".to_string(),
                    style: "secondary".to_string(),
                    is_default: false,
                },
                PromptAction {
                    id: "cancel".to_string(),
                    label: "Cancel".to_string(),
                    style: "danger".to_string(),
                    is_default: !error.is_retryable(),
                },
            ],
            context: {
                let mut ctx = HashMap::new();
                ctx.insert("error_type".to_string(), format!("{:?}", error));
                ctx.insert("retryable".to_string(), error.is_retryable().to_string());
                ctx.insert("transferred_bytes".to_string(), transferred_bytes.to_string());
                ctx
            },
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            requires_response: true,
        };

        self.send_prompt(prompt).await
    }

    /// Show information about transfer
    pub async fn show_info(
        &self,
        transfer_id: &str,
        operation: TransferOperation,
        status: TransferStatus,
        message: &str,
    ) -> Result<(), String> {
        let prompt = TransferPrompt {
            id: self.next_id(),
            prompt_type: PromptType::Info,
            severity: PromptSeverity::Low,
            title: format!("Transfer Information - {}", status),
            message: message.to_string(),
            description: None,
            transfer_id: Some(transfer_id.to_string()),
            operation: Some(operation),
            status: Some(status),
            actions: vec![PromptAction {
                id: "ok".to_string(),
                label: "OK".to_string(),
                style: "primary".to_string(),
                is_default: true,
            }],
            context: HashMap::new(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            requires_response: false,
        };

        self.send_prompt(prompt).await
    }

    /// Send progress notification
    pub async fn send_progress(&self, notification: ProgressNotification) -> Result<(), String> {
        self.app_handle
            .emit("transfer-progress", &notification)
            .map_err(|e| format!("Failed to emit progress: {}", e))?;
        Ok(())
    }

    /// Format error message with suggestions
    fn format_error(&self, error: &TransferError, transferred_bytes: u64) -> (String, String, String) {
        let (title, message, suggestion) = match error {
            TransferError::TemporaryNetwork(msg) => (
                "Network Error".to_string(),
                format!("Temporary network issue: {}", msg),
                format!(
                    "The transfer encountered a network error. This is usually temporary.\n\
                     Transferred: {}\n\
                     Suggestion: Retry the transfer. If the problem persists, check your network connection.",
                    self.format_bytes(transferred_bytes)
                ),
            ),
            TransferError::Timeout(msg) => (
                "Timeout Error".to_string(),
                format!("Operation timed out: {}", msg),
                format!(
                    "The operation took too long to complete.\n\
                     Transferred: {}\n\
                     Suggestion: Try again with a larger timeout or check network conditions.",
                    self.format_bytes(transferred_bytes)
                ),
            ),
            TransferError::PermissionDenied(msg) => (
                "Permission Error".to_string(),
                format!("Permission denied: {}", msg),
                "You don't have permission to perform this operation.\n\
                 Suggestion: Check file permissions and try again with appropriate access rights.".to_string(),
            ),
            TransferError::DiskFull(msg) => (
                "Disk Full Error".to_string(),
                format!("Disk full: {}", msg),
                "There is not enough disk space to complete the transfer.\n\
                 Suggestion: Free up disk space and try again.".to_string(),
            ),
            TransferError::InvalidPath(msg) => (
                "Invalid Path Error".to_string(),
                format!("Invalid path: {}", msg),
                "The specified path is invalid or doesn't exist.\n\
                 Suggestion: Check the file paths and try again.".to_string(),
            ),
            TransferError::AuthenticationFailed(msg) => (
                "Authentication Error".to_string(),
                format!("Authentication failed: {}", msg),
                "Failed to authenticate with the server.\n\
                 Suggestion: Check your credentials and try again.".to_string(),
            ),
            TransferError::ConnectionLost => (
                "Connection Lost".to_string(),
                "Connection to the server was lost".to_string(),
                format!(
                    "The SSH connection was interrupted.\n\
                     Transferred: {}\n\
                     Suggestion: The transfer can be resumed automatically.",
                    self.format_bytes(transferred_bytes)
                ),
            ),
            TransferError::Cancelled => (
                "Transfer Cancelled".to_string(),
                "The transfer was cancelled by the user".to_string(),
                format!(
                    "You cancelled the transfer.\n\
                     Transferred: {}",
                    self.format_bytes(transferred_bytes)
                ),
            ),
            TransferError::CheckpointMismatch(msg) => (
                "Checkpoint Error".to_string(),
                format!("Checkpoint mismatch: {}", msg),
                "The transfer checkpoint is invalid or corrupted.\n\
                 Suggestion: Start the transfer from the beginning.".to_string(),
            ),
            TransferError::CannotResume(msg) => (
                "Resume Error".to_string(),
                format!("Cannot resume transfer: {}", msg),
                "The transfer cannot be resumed.\n\
                 Suggestion: Start the transfer from the beginning.".to_string(),
            ),
            TransferError::InvalidCheckpoint => (
                "Invalid Checkpoint".to_string(),
                "The checkpoint data is invalid".to_string(),
                "The transfer checkpoint is corrupted.\n\
                 Suggestion: Start the transfer from the beginning.".to_string(),
            ),
            TransferError::WouldBlock => (
                "Blocking Operation".to_string(),
                "Operation would block (non-blocking mode)".to_string(),
                "The operation cannot be completed immediately in non-blocking mode.\n\
                 Suggestion: This is usually temporary and the operation will retry automatically.".to_string(),
            ),
            TransferError::Unknown(msg) => (
                "Unknown Error".to_string(),
                format!("Unknown error: {}", msg),
                format!(
                    "An unexpected error occurred.\n\
                     Transferred: {}\n\
                     Suggestion: Try again or contact support if the problem persists.",
                    self.format_bytes(transferred_bytes)
                ),
            ),
        };

        (title, message, suggestion)
    }

    /// Format bytes in human-readable format
    fn format_bytes(&self, bytes: u64) -> String {
        const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
        let mut size = bytes as f64;
        let mut unit_index = 0;

        while size >= 1024.0 && unit_index < UNITS.len() - 1 {
            size /= 1024.0;
            unit_index += 1;
        }

        if unit_index == 0 {
            format!("{} {}", bytes, UNITS[unit_index])
        } else {
            format!("{:.2} {}", size, UNITS[unit_index])
        }
    }
}

/// Helper functions for creating common prompts
impl PromptManager {
    /// Prompt for overwrite confirmation
    pub async fn confirm_overwrite(
        &self,
        transfer_id: &str,
        operation: TransferOperation,
        file_path: &str,
        existing_size: u64,
        new_size: u64,
    ) -> Result<bool, String> {
        let prompt = TransferPrompt {
            id: self.next_id(),
            prompt_type: PromptType::Question,
            severity: PromptSeverity::Medium,
            title: "File Already Exists".to_string(),
            message: format!("{} already exists. Do you want to overwrite it?", 
                Path::new(file_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file")
            ),
            description: Some(format!(
                "Existing file: {}\nNew file: {}\nPath: {}",
                self.format_bytes(existing_size),
                self.format_bytes(new_size),
                file_path
            )),
            transfer_id: Some(transfer_id.to_string()),
            operation: Some(operation),
            status: None,
            actions: vec![
                PromptAction {
                    id: "overwrite".to_string(),
                    label: "Overwrite".to_string(),
                    style: "primary".to_string(),
                    is_default: true,
                },
                PromptAction {
                    id: "rename".to_string(),
                    label: "Rename New".to_string(),
                    style: "secondary".to_string(),
                    is_default: false,
                },
                PromptAction {
                    id: "skip".to_string(),
                    label: "Skip".to_string(),
                    style: "secondary".to_string(),
                    is_default: false,
                },
                PromptAction {
                    id: "cancel".to_string(),
                    label: "Cancel Transfer".to_string(),
                    style: "danger".to_string(),
                    is_default: false,
                },
            ],
            context: HashMap::new(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            requires_response: true,
        };

        self.send_prompt(prompt).await?;
        // In a real implementation, we'd wait for the response
        Ok(true)
    }

    /// Prompt for large file transfer confirmation
    pub async fn confirm_large_file(
        &self,
        transfer_id: &str,
        operation: TransferOperation,
        file_path: &str,
        file_size: u64,
    ) -> Result<bool, String> {
        let prompt = TransferPrompt {
            id: self.next_id(),
            prompt_type: PromptType::Warning,
            severity: PromptSeverity::Medium,
            title: "Large File Transfer".to_string(),
            message: format!("This is a large file transfer: {}", self.format_bytes(file_size)),
            description: Some(format!(
                "File: {}\nSize: {}\nThis may take a long time to transfer.",
                file_path,
                self.format_bytes(file_size)
            )),
            transfer_id: Some(transfer_id.to_string()),
            operation: Some(operation),
            status: None,
            actions: vec![
                PromptAction {
                    id: "proceed".to_string(),
                    label: "Proceed".to_string(),
                    style: "primary".to_string(),
                    is_default: true,
                },
                PromptAction {
                    id: "cancel".to_string(),
                    label: "Cancel".to_string(),
                    style: "secondary".to_string(),
                    is_default: false,
                },
            ],
            context: HashMap::new(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            requires_response: true,
        };

        self.send_prompt(prompt).await?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_bytes() {
        let manager = PromptManager::new(tauri::generate_context!().handle());
        
        assert_eq!(manager.format_bytes(512), "512 B");
        assert_eq!(manager.format_bytes(1024), "1.00 KB");
        assert_eq!(manager.format_bytes(1536), "1.50 KB");
        assert_eq!(manager.format_bytes(1048576), "1.00 MB");
        assert_eq!(manager.format_bytes(1073741824), "1.00 GB");
    }
}
