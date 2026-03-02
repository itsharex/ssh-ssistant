//! File transfer module
//!
//! This module provides a complete file transfer system with:
//! - State machine for transfer lifecycle management
//! - Dedicated connection pool isolated from session operations
//! - Async SFTP operations with timeout control
//! - Checkpoint-based resume support
//! - Health monitoring and statistics
//!
//! # Architecture
//!
//! The transfer system is organized into several submodules:
//!
//! - **types**: Core type definitions (status enums, errors, settings, events)
//! - **state**: Transfer state machine with validated transitions
//! - **pool**: Dedicated connection pool for transfer operations
//! - **async_sftp**: Async SFTP wrappers with timeout and retry logic
//! - **checkpoint**: Checkpoint management for resume support
//! - **manager**: Main TransferManager orchestrating all operations
//!
//! # Usage Example
//!
//! ```rust
//! use crate::ssh::transfer::{TransferManager, TransferSettings, TransferOperation};
//!
//! // Create manager with configuration
//! let manager = TransferManager::new(config, settings, app_data_dir)?;
//!
//! // Start a transfer
//! let transfer_id = manager.start_transfer(
//!     TransferOperation::Upload,
//!     local_path,
//!     remote_path,
//!     app_handle,
//! ).await?;
//!
//! // Monitor progress
//! let status = manager.get_transfer_status(&transfer_id);
//! let health = manager.health_check().await;
//! ```

pub mod async_sftp;
pub mod checkpoint;
pub mod manager;
pub mod observability;
pub mod pool;
pub mod prompt;
pub mod retry;
pub mod state;
pub mod types;

// Re-export commonly used types
pub use types::{
    ProgressCallback, TransferError, TransferEvent, TransferHealth, TransferOperation,
    TransferSettings, TransferStatus,
};

pub use state::{TransferState, TransferStateHandle};
pub use manager::TransferManager;
pub use pool::{PoolStats, TransferConnection, TransferPool};
pub use checkpoint::{CheckpointManager, TransferCheckpoint};
pub use async_sftp::AsyncSftp;
