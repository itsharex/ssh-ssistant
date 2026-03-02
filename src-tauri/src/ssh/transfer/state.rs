//! Transfer state machine implementation
//!
//! This module provides a state machine for tracking transfer states and
//! managing valid state transitions according to the architecture design.

use crate::ssh::transfer::types::{TransferError, TransferStatus};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

/// Transfer state tracking with thread-safe state transitions
#[derive(Debug)]
pub struct TransferState {
    /// Unique identifier for this transfer
    id: String,
    /// Current status
    status: Mutex<TransferStatus>,
    /// Bytes transferred so far
    transferred: AtomicU64,
    /// Total file size in bytes
    total: u64,
    /// When the transfer was created
    created_at: Instant,
    /// When the transfer last had progress update
    last_progress_at: Mutex<Instant>,
    /// Cancellation flag
    cancelled: AtomicBool,
    /// Error message if failed
    error: Mutex<Option<String>>,
}

impl TransferState {
    /// Create a new transfer state in Pending status
    pub fn new(id: String, total: u64) -> Self {
        let now = Instant::now();
        Self {
            id,
            status: Mutex::new(TransferStatus::Pending),
            transferred: AtomicU64::new(0),
            total,
            created_at: now,
            last_progress_at: Mutex::new(now),
            cancelled: AtomicBool::new(false),
            error: Mutex::new(None),
        }
    }

    /// Get the transfer ID
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Get current status
    pub fn status(&self) -> TransferStatus {
        *self.status.blocking_lock()
    }

    /// Get bytes transferred
    pub fn transferred(&self) -> u64 {
        self.transferred.load(Ordering::Relaxed)
    }

    /// Get total bytes
    pub fn total(&self) -> u64 {
        self.total
    }

    /// Get progress as a percentage (0.0 to 1.0)
    pub fn progress(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.transferred() as f64 / self.total as f64
        }
    }

    /// Get time since creation
    pub fn elapsed(&self) -> std::time::Duration {
        self.created_at.elapsed()
    }

    /// Get time since last progress update
    pub fn time_since_progress(&self) -> std::time::Duration {
        self.last_progress_at
            .blocking_lock()
            .elapsed()
    }

    /// Check if transfer is cancelled
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// Get error message if any
    pub fn error(&self) -> Option<String> {
        self.error
            .blocking_lock()
            .as_ref()
            .cloned()
    }

    /// Update transferred bytes (returns true if value changed)
    pub fn update_transferred(&self, bytes: u64) -> bool {
        let prev = self.transferred.fetch_max(bytes, Ordering::Relaxed);
        if bytes > prev {
            *self.last_progress_at.blocking_lock() = Instant::now();
            true
        } else {
            false
        }
    }

    /// Add to transferred bytes (returns new total)
    pub fn add_transferred(&self, bytes: u64) -> u64 {
        let new_total = self.transferred.fetch_add(bytes, Ordering::Relaxed) + bytes;
        *self.last_progress_at.blocking_lock() = Instant::now();
        new_total
    }

    /// Transition to Connecting status
    pub fn start(&self) -> Result<(), TransferError> {
        let mut status = self.status.blocking_lock();
        self.validate_transition(*status, TransferStatus::Connecting)?;
        *status = TransferStatus::Connecting;
        Ok(())
    }

    /// Transition to Transferring status
    pub fn begin_transfer(&self) -> Result<(), TransferError> {
        let mut status = self.status.blocking_lock();
        self.validate_transition(*status, TransferStatus::Transferring)?;
        *status = TransferStatus::Transferring;
        Ok(())
    }

    /// Transition to Paused status
    pub fn pause(&self) -> Result<(), TransferError> {
        let mut status = self.status.blocking_lock();
        self.validate_transition(*status, TransferStatus::Paused)?;
        *status = TransferStatus::Paused;
        Ok(())
    }

    /// Transition to Resuming or Transferring status
    pub fn resume(&self) -> Result<(), TransferError> {
        let mut status = self.status.blocking_lock();
        let current = *status;

        // From Paused, go to Transferring
        // From Failed, go to Resuming
        let new_status = match current {
            TransferStatus::Paused => TransferStatus::Transferring,
            TransferStatus::Failed => TransferStatus::Resuming,
            _ => {
                return Err(TransferError::CannotResume(format!(
                    "Cannot resume from status: {:?}",
                    current
                )))
            }
        };

        self.validate_transition(current, new_status)?;
        *status = new_status;
        Ok(())
    }

    /// Transition to Completed status
    pub fn complete(&self) -> Result<(), TransferError> {
        let mut status = self.status.blocking_lock();
        self.validate_transition(*status, TransferStatus::Completed)?;
        *status = TransferStatus::Completed;
        Ok(())
    }

    /// Transition to Failed status with error message
    pub fn fail(&self, error: String) -> Result<(), TransferError> {
        let mut status = self.status.blocking_lock();
        self.validate_transition(*status, TransferStatus::Failed)?;
        *status = TransferStatus::Failed;
        *self.error.blocking_lock() = Some(error);
        Ok(())
    }

    /// Transition to Cancelled status
    pub fn cancel(&self) -> Result<(), TransferError> {
        self.cancelled.store(true, Ordering::Relaxed);

        let mut status = self.status.blocking_lock();
        if !status.is_terminal() {
            *status = TransferStatus::Cancelled;
            Ok(())
        } else {
            Err(TransferError::CannotResume(format!(
                "Cannot cancel from terminal status: {:?}",
                *status
            )))
        }
    }

    /// Validate that a state transition is allowed
    fn validate_transition(
        &self,
        from: TransferStatus,
        to: TransferStatus,
    ) -> Result<(), TransferError> {
        // Define valid transitions
        let valid = match (from, to) {
            // From Pending
            (TransferStatus::Pending, TransferStatus::Connecting) => true,

            // From Connecting
            (TransferStatus::Connecting, TransferStatus::Transferring) => true,
            (TransferStatus::Connecting, TransferStatus::Failed) => true,
            (TransferStatus::Connecting, TransferStatus::Cancelled) => true,

            // From Transferring
            (TransferStatus::Transferring, TransferStatus::Paused) => true,
            (TransferStatus::Transferring, TransferStatus::Completed) => true,
            (TransferStatus::Transferring, TransferStatus::Failed) => true,
            (TransferStatus::Transferring, TransferStatus::Cancelled) => true,

            // From Paused
            (TransferStatus::Paused, TransferStatus::Transferring) => true,
            (TransferStatus::Paused, TransferStatus::Cancelled) => true,

            // From Failed
            (TransferStatus::Failed, TransferStatus::Resuming) => true,
            (TransferStatus::Failed, TransferStatus::Cancelled) => true,

            // From Resuming
            (TransferStatus::Resuming, TransferStatus::Transferring) => true,
            (TransferStatus::Resuming, TransferStatus::Failed) => true,
            (TransferStatus::Resuming, TransferStatus::Cancelled) => true,

            // All other transitions are invalid
            _ => false,
        };

        if valid {
            Ok(())
        } else {
            Err(TransferError::CannotResume(format!(
                "Invalid state transition: {:?} -> {:?}",
                from, to
            )))
        }
    }

    /// Check if transfer appears stuck (no progress for too long)
    pub fn is_stuck(&self, timeout: std::time::Duration) -> bool {
        let status = self.status();
        if !status.is_active() {
            return false;
        }

        // Don't consider connecting as stuck unless it's been very long
        if status == TransferStatus::Connecting {
            return self.time_since_progress() > timeout * 3;
        }

        self.time_since_progress() > timeout
    }

    /// Create a cloneable handle for the state
    pub fn handle(&self) -> TransferStateHandle {
        TransferStateHandle {
            id: self.id.clone(),
            state: Arc::new(StateInner {
                status: *self.status.blocking_lock(),
                transferred: self.transferred(),
                total: self.total,
                is_cancelled: self.is_cancelled(),
            }),
        }
    }
}

/// A lightweight, thread-safe handle to transfer state for external use
#[derive(Debug, Clone)]
pub struct TransferStateHandle {
    id: String,
    state: Arc<StateInner>,
}

#[derive(Debug)]
struct StateInner {
    status: TransferStatus,
    transferred: u64,
    total: u64,
    is_cancelled: bool,
}

impl TransferStateHandle {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn status(&self) -> TransferStatus {
        self.state.status
    }

    pub fn transferred(&self) -> u64 {
        self.state.transferred
    }

    pub fn total(&self) -> u64 {
        self.state.total
    }

    pub fn progress(&self) -> f64 {
        if self.state.total == 0 {
            0.0
        } else {
            self.state.transferred as f64 / self.state.total as f64
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.state.is_cancelled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_creation() {
        let state = TransferState::new("test-id".to_string(), 1000);
        assert_eq!(state.id(), "test-id");
        assert_eq!(state.status(), TransferStatus::Pending);
        assert_eq!(state.transferred(), 0);
        assert_eq!(state.total(), 1000);
    }

    #[test]
    fn test_valid_transitions() {
        let state = TransferState::new("test-id".to_string(), 1000);

        // Pending -> Connecting
        state.start().unwrap();
        assert_eq!(state.status(), TransferStatus::Connecting);

        // Connecting -> Transferring
        state.begin_transfer().unwrap();
        assert_eq!(state.status(), TransferStatus::Transferring);

        // Transferring -> Paused
        state.pause().unwrap();
        assert_eq!(state.status(), TransferStatus::Paused);

        // Paused -> Transferring (resume)
        state.resume().unwrap();
        assert_eq!(state.status(), TransferStatus::Transferring);

        // Transferring -> Completed
        state.complete().unwrap();
        assert_eq!(state.status(), TransferStatus::Completed);
    }

    #[test]
    fn test_resume_from_failed() {
        let state = TransferState::new("test-id".to_string(), 1000);

        state.start().unwrap();
        state.begin_transfer().unwrap();
        state.fail("test error".to_string()).unwrap();
        assert_eq!(state.status(), TransferStatus::Failed);

        state.resume().unwrap();
        assert_eq!(state.status(), TransferStatus::Resuming);
    }

    #[test]
    fn test_invalid_transition() {
        let state = TransferState::new("test-id".to_string(), 1000);

        // Can't go directly from Pending to Transferring
        assert!(state.begin_transfer().is_err());
        assert_eq!(state.status(), TransferStatus::Pending);
    }

    #[test]
    fn test_cannot_cancel_terminal_state() {
        let state = TransferState::new("test-id".to_string(), 1000);

        state.start().unwrap();
        state.begin_transfer().unwrap();
        state.complete().unwrap();

        // Can't cancel after completion
        assert!(state.cancel().is_err());
        assert_eq!(state.status(), TransferStatus::Completed);
    }

    #[test]
    fn test_progress_tracking() {
        let state = TransferState::new("test-id".to_string(), 1000);

        assert_eq!(state.transferred(), 0);
        state.add_transferred(100);
        assert_eq!(state.transferred(), 100);
        state.add_transferred(200);
        assert_eq!(state.transferred(), 300);

        assert!((state.progress() - 0.3).abs() < 0.001);
    }

    #[test]
    fn test_stuck_detection() {
        let state = TransferState::new("test-id".to_string(), 1000);

        state.start().unwrap();
        state.begin_transfer().unwrap();

        // Should not be stuck immediately
        assert!(!state.is_stuck(std::time::Duration::from_secs(30)));
    }
}
