//! Enhanced retry mechanism with exponential backoff and error classification
//!
//! This module provides a sophisticated retry system that adapts to different
//! error types and network conditions for optimal transfer reliability.

use crate::ssh::transfer::types::{TransferError, TransferSettings};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio::time::sleep;

/// Retry strategy configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryStrategy {
    /// Maximum number of retry attempts
    pub max_attempts: u32,
    /// Initial delay between retries
    pub initial_delay: Duration,
    /// Maximum delay between retries
    pub max_delay: Duration,
    /// Backoff multiplier (e.g., 2.0 for exponential backoff)
    pub backoff_multiplier: f64,
    /// Jitter factor to add randomness (0.0 to 1.0)
    pub jitter_factor: f64,
    /// Whether to use adaptive backoff based on error type
    pub adaptive_backoff: bool,
}

impl Default for RetryStrategy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_delay: Duration::from_millis(1000),
            max_delay: Duration::from_secs(60),
            backoff_multiplier: 2.0,
            jitter_factor: 0.1,
            adaptive_backoff: true,
        }
    }
}

impl RetryStrategy {
    /// Create retry strategy from transfer settings
    pub fn from_settings(settings: &TransferSettings) -> Self {
        Self {
            max_attempts: settings.max_retry_attempts,
            initial_delay: settings.retry_delay(),
            max_delay: Duration::from_secs(60), // Cap at 60 seconds
            backoff_multiplier: 2.0,
            jitter_factor: 0.1,
            adaptive_backoff: true,
        }
    }

    /// Calculate delay for a specific attempt
    pub fn calculate_delay(&self, attempt: u32, error: Option<&TransferError>) -> Duration {
        let base_delay = if self.adaptive_backoff {
            self.adaptive_delay(attempt, error)
        } else {
            self.exponential_delay(attempt)
        };

        // Add jitter to prevent thundering herd
        let jitter = if self.jitter_factor > 0.0 {
            let jitter_range = base_delay.as_millis() as f64 * self.jitter_factor;
            let jitter_ms = ((std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() % 1000000) as f64 / 1000000.0 * jitter_range) as u64;
            Duration::from_millis(jitter_ms as u64)
        } else {
            Duration::ZERO
        };

        std::cmp::min(base_delay + jitter, self.max_delay)
    }

    /// Exponential backoff calculation
    fn exponential_delay(&self, attempt: u32) -> Duration {
        let delay_ms = self.initial_delay.as_millis() as f64 * self.backoff_multiplier.powi(attempt as i32);
        Duration::from_millis(delay_ms as u64)
    }

    /// Adaptive delay based on error type
    fn adaptive_delay(&self, attempt: u32, error: Option<&TransferError>) -> Duration {
        let base_delay = self.exponential_delay(attempt);
        
        match error {
            Some(TransferError::TemporaryNetwork(_)) => {
                // Network errors need more time to recover
                Duration::from_millis((base_delay.as_millis() as f64 * 1.5) as u64)
            }
            Some(TransferError::Timeout(_)) => {
                // Timeout errors need longer delays
                Duration::from_millis((base_delay.as_millis() as f64 * 2.0) as u64)
            }
            Some(TransferError::ConnectionLost) => {
                // Connection lost needs significant recovery time
                Duration::from_millis((base_delay.as_millis() as f64 * 3.0) as u64)
            }
            Some(TransferError::WouldBlock) => {
                // Would block errors need minimal delay
                Duration::from_millis(std::cmp::max(100, base_delay.as_millis() / 2) as u64)
            }
            _ => base_delay,
        }
    }
}

/// Retry context for tracking retry attempts
#[derive(Debug, Clone)]
pub struct RetryContext {
    /// Current attempt number (0-based)
    pub attempt: u32,
    /// Maximum attempts allowed
    pub max_attempts: u32,
    /// Start time of the retry sequence
    pub start_time: Instant,
    /// Last error encountered
    pub last_error: Option<TransferError>,
    /// Total time spent retrying
    pub total_retry_time: Duration,
    /// Whether this is a fast retry (for transient errors)
    pub is_fast_retry: bool,
}

impl RetryContext {
    /// Create new retry context
    pub fn new(max_attempts: u32) -> Self {
        Self {
            attempt: 0,
            max_attempts,
            start_time: Instant::now(),
            last_error: None,
            total_retry_time: Duration::ZERO,
            is_fast_retry: false,
        }
    }

    /// Check if more retries are allowed
    pub fn can_retry(&self) -> bool {
        self.attempt < self.max_attempts
    }

    /// Check if this is the last attempt
    pub fn is_last_attempt(&self) -> bool {
        self.attempt == self.max_attempts - 1
    }

    /// Increment attempt counter
    pub fn next_attempt(&mut self, error: TransferError) {
        self.last_error = Some(error.clone());
        self.attempt += 1;
        self.is_fast_retry = self.should_fast_retry(&error);
    }

    /// Determine if this should be a fast retry
    fn should_fast_retry(&self, error: &TransferError) -> bool {
        matches!(error, TransferError::WouldBlock) && self.attempt < 2
    }

    /// Get elapsed time since start
    pub fn elapsed(&self) -> Duration {
        self.start_time.elapsed()
    }

    /// Update total retry time
    pub fn update_retry_time(&mut self, duration: Duration) {
        self.total_retry_time += duration;
    }
}

/// Retry result
#[derive(Debug, Clone)]
pub enum RetryResult<T> {
    /// Operation succeeded
    Success(T),
    /// Operation failed after all retries
    Failed {
        last_error: TransferError,
        total_attempts: u32,
        total_time: Duration,
    },
    /// Operation was cancelled
    Cancelled,
}

/// Enhanced retry executor
pub struct RetryExecutor {
    strategy: RetryStrategy,
}

impl RetryExecutor {
    /// Create new retry executor
    pub fn new(strategy: RetryStrategy) -> Self {
        Self { strategy }
    }

    /// Execute an operation with retry logic
    pub async fn execute<F, T, Fut>(&self, mut operation: F) -> RetryResult<T>
    where
        F: FnMut(u32) -> Fut,
        Fut: std::future::Future<Output = Result<T, TransferError>>,
    {
        let mut context = RetryContext::new(self.strategy.max_attempts);
        
        loop {
            // Execute the operation
            let attempt_start = Instant::now();
            let result = operation(context.attempt).await;
            let attempt_duration = attempt_start.elapsed();
            
            match result {
                Ok(value) => {
                    // Operation succeeded
                    return RetryResult::Success(value);
                }
                Err(error) => {
                    // Check if error is retryable
                    if !error.is_retryable() {
                        return RetryResult::Failed {
                            last_error: error,
                            total_attempts: context.attempt + 1,
                            total_time: context.elapsed(),
                        };
                    }

                    // Check if we can retry
                    if !context.can_retry() {
                        return RetryResult::Failed {
                            last_error: error,
                            total_attempts: context.attempt + 1,
                            total_time: context.elapsed(),
                        };
                    }

                    // Log retry attempt
                    eprintln!(
                        "[Retry] Attempt {} failed: {}, retrying in {:?}",
                        context.attempt + 1,
                        error,
                        self.strategy.calculate_delay(context.attempt + 1, Some(&error))
                    );

                    // Move to next attempt
                    context.next_attempt(error);
                    context.update_retry_time(attempt_duration);

                    // Calculate and wait for delay
                    let delay = self.strategy.calculate_delay(context.attempt, context.last_error.as_ref());
                    
                    // For fast retries, use minimal delay
                    let actual_delay = if context.is_fast_retry {
                        std::cmp::min(delay, Duration::from_millis(100))
                    } else {
                        delay
                    };

                    sleep(actual_delay).await;
                }
            }
        }
    }

    /// Execute with cancellation support
    pub async fn execute_with_cancel<F, T, Fut>(
        &self,
        mut operation: F,
        cancel_flag: &std::sync::atomic::AtomicBool,
    ) -> RetryResult<T>
    where
        F: FnMut(u32) -> Fut,
        Fut: std::future::Future<Output = Result<T, TransferError>>,
    {
        let mut context = RetryContext::new(self.strategy.max_attempts);
        
        loop {
            // Check for cancellation
            if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                return RetryResult::Cancelled;
            }

            let attempt_start = Instant::now();
            let result = operation(context.attempt).await;
            let attempt_duration = attempt_start.elapsed();
            
            match result {
                Ok(value) => return RetryResult::Success(value),
                Err(error) => {
                    if !error.is_retryable() || !context.can_retry() {
                        return RetryResult::Failed {
                            last_error: error,
                            total_attempts: context.attempt + 1,
                            total_time: context.elapsed(),
                        };
                    }

                    context.next_attempt(error);
                    context.update_retry_time(attempt_duration);

                    let delay = self.strategy.calculate_delay(context.attempt, context.last_error.as_ref());
                    let actual_delay = if context.is_fast_retry {
                        std::cmp::min(delay, Duration::from_millis(100))
                    } else {
                        delay
                    };

                    // Check for cancellation during delay
                    let mut remaining_delay = actual_delay;
                    while remaining_delay > Duration::ZERO {
                        if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                            return RetryResult::Cancelled;
                        }
                        
                        let sleep_duration = std::cmp::min(remaining_delay, Duration::from_millis(100));
                        sleep(sleep_duration).await;
                        remaining_delay -= sleep_duration;
                    }
                }
            }
        }
    }

    /// Get retry statistics
    pub fn get_stats(&self, context: &RetryContext) -> RetryStats {
        RetryStats {
            total_attempts: context.attempt + 1,
            max_attempts: context.max_attempts,
            elapsed_time: context.elapsed(),
            total_retry_time: context.total_retry_time,
            last_error: context.last_error.clone(),
            success_rate: if context.attempt > 0 {
                Some(1.0 / (context.attempt + 1) as f64)
            } else {
                Some(1.0)
            },
        }
    }
}

/// Retry statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryStats {
    pub total_attempts: u32,
    pub max_attempts: u32,
    pub elapsed_time: Duration,
    pub total_retry_time: Duration,
    pub last_error: Option<TransferError>,
    pub success_rate: Option<f64>,
}

/// Circuit breaker pattern for preventing cascading failures
pub struct CircuitBreaker {
    /// Failure threshold
    failure_threshold: usize,
    /// Recovery timeout
    recovery_timeout: Duration,
    /// Current state
    state: std::sync::RwLock<CircuitBreakerState>,
    /// Failure count
    failure_count: std::sync::atomic::AtomicUsize,
    /// Last failure time
    last_failure_time: std::sync::RwLock<Option<Instant>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CircuitBreakerState {
    Closed,    // Normal operation
    Open,      // Failing, reject calls
    HalfOpen,  // Testing if recovered
}

impl CircuitBreaker {
    /// Create new circuit breaker
    pub fn new(failure_threshold: usize, recovery_timeout: Duration) -> Self {
        Self {
            failure_threshold,
            recovery_timeout,
            state: std::sync::RwLock::new(CircuitBreakerState::Closed),
            failure_count: std::sync::atomic::AtomicUsize::new(0),
            last_failure_time: std::sync::RwLock::new(None),
        }
    }

    /// Check if operation should be allowed
    pub fn allow_operation(&self) -> bool {
        let state = *self.state.read().unwrap();
        
        match state {
            CircuitBreakerState::Closed => true,
            CircuitBreakerState::Open => {
                let last_failure = *self.last_failure_time.read().unwrap();
                if let Some(last) = last_failure {
                    if last.elapsed() > self.recovery_timeout {
                        // Try to transition to half-open
                        if let Ok(mut state_guard) = self.state.write() {
                            *state_guard = CircuitBreakerState::HalfOpen;
                            return true;
                        }
                    }
                }
                false
            }
            CircuitBreakerState::HalfOpen => true,
        }
    }

    /// Record successful operation
    pub fn record_success(&self) {
        let current_count = self.failure_count.load(std::sync::atomic::Ordering::Relaxed);
        if current_count > 0 {
            self.failure_count.store(0, std::sync::atomic::Ordering::Relaxed);
        }
        
        if let Ok(mut state) = self.state.write() {
            *state = CircuitBreakerState::Closed;
        }
    }

    /// Record failed operation
    pub fn record_failure(&self) {
        let new_count = self.failure_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        
        *self.last_failure_time.write().unwrap() = Some(Instant::now());
        
        if new_count >= self.failure_threshold {
            if let Ok(mut state) = self.state.write() {
                *state = CircuitBreakerState::Open;
            }
        }
    }

    /// Get current state
    pub fn get_state(&self) -> CircuitBreakerState {
        *self.state.read().unwrap()
    }

    /// Get failure count
    pub fn get_failure_count(&self) -> usize {
        self.failure_count.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Reset the circuit breaker
    pub fn reset(&self) {
        self.failure_count.store(0, std::sync::atomic::Ordering::Relaxed);
        *self.last_failure_time.write().unwrap() = None;
        if let Ok(mut state) = self.state.write() {
            *state = CircuitBreakerState::Closed;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_retry_strategy_delay_calculation() {
        let strategy = RetryStrategy::default();
        
        // Test exponential backoff
        let delay1 = strategy.calculate_delay(0, None);
        let delay2 = strategy.calculate_delay(1, None);
        let delay3 = strategy.calculate_delay(2, None);
        
        assert!(delay2 > delay1);
        assert!(delay3 > delay2);
        
        // Test adaptive delay for different error types
        let network_error = TransferError::TemporaryNetwork("test".to_string());
        let timeout_error = TransferError::Timeout("test".to_string());
        let would_block_error = TransferError::WouldBlock;
        
        let network_delay = strategy.calculate_delay(1, Some(&network_error));
        let timeout_delay = strategy.calculate_delay(1, Some(&timeout_error));
        let would_block_delay = strategy.calculate_delay(1, Some(&would_block_error));
        
        assert!(timeout_delay > network_delay);
        assert!(network_delay > would_block_delay);
    }

    #[tokio::test]
    async fn test_retry_executor_success() {
        let strategy = RetryStrategy::default();
        let executor = RetryExecutor::new(strategy);
        
        let mut call_count = 0;
        let result = executor.execute(|attempt| async move {
            call_count += 1;
            if attempt == 0 {
                Err(TransferError::TemporaryNetwork("First attempt fails".to_string()))
            } else {
                Ok("success")
            }
        }).await;
        
        match result {
            RetryResult::Success(value) => {
                assert_eq!(value, "success");
                assert_eq!(call_count, 2);
            }
            _ => panic!("Expected success"),
        }
    }

    #[tokio::test]
    async fn test_retry_executor_failure() {
        let strategy = RetryStrategy {
            max_attempts: 2,
            ..Default::default()
        };
        let executor = RetryExecutor::new(strategy);
        
        let result = executor.execute(|_| async {
            Err(TransferError::PermissionDenied("Always fails".to_string()))
        }).await;
        
        match result {
            RetryResult::Failed { last_error, total_attempts, .. } => {
                assert!(matches!(last_error, TransferError::PermissionDenied(_)));
                assert_eq!(total_attempts, 1); // Non-retryable errors don't retry
            }
            _ => panic!("Expected failure"),
        }
    }

    #[test]
    fn test_circuit_breaker() {
        let breaker = CircuitBreaker::new(3, Duration::from_millis(100));
        
        // Initially closed
        assert_eq!(breaker.get_state(), CircuitBreakerState::Closed);
        assert!(breaker.allow_operation());
        
        // Record failures
        breaker.record_failure();
        breaker.record_failure();
        assert_eq!(breaker.get_failure_count(), 2);
        assert!(breaker.allow_operation()); // Still closed
        
        // Third failure trips the breaker
        breaker.record_failure();
        assert_eq!(breaker.get_state(), CircuitBreakerState::Open);
        assert!(!breaker.allow_operation()); // Now open
        
        // Record success resets
        breaker.record_success();
        assert_eq!(breaker.get_state(), CircuitBreakerState::Closed);
        assert!(breaker.allow_operation());
    }
}
