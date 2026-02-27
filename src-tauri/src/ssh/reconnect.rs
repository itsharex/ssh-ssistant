//! SSH Reconnection Manager
//!
//! Implements intelligent reconnection with exponential backoff algorithm.
//! Tracks retry attempts, calculates delays, and determines when to give up.

use std::time::{Duration, Instant};

use super::error_classifier::SshErrorType;
use crate::models::ReconnectSettings;

/// Reconnection Manager
///
/// Manages reconnection attempts with exponential backoff algorithm.
/// Tracks attempt history and calculates appropriate delays.
pub struct ReconnectManager {
    /// Configuration settings
    config: ReconnectSettings,
    /// Current attempt count
    attempt_count: u32,
    /// Last error type encountered
    last_error_type: Option<SshErrorType>,
    /// Time of last connection attempt
    last_attempt_time: Option<Instant>,
    /// Total time spent in reconnection attempts
    total_retry_time: Duration,
}

impl ReconnectManager {
    /// Create a new ReconnectManager with the given settings
    pub fn new(config: ReconnectSettings) -> Self {
        Self {
            config,
            attempt_count: 0,
            last_error_type: None,
            last_attempt_time: None,
            total_retry_time: Duration::ZERO,
        }
    }

    /// Create a new ReconnectManager with default settings
    pub fn with_defaults() -> Self {
        Self::new(ReconnectSettings::default())
    }

    /// Check if auto-reconnect is enabled
    pub fn is_enabled(&self) -> bool {
        self.config.enable_auto_reconnect
    }

    /// Enable or disable auto-reconnect
    pub fn set_enabled(&mut self, enabled: bool) {
        self.config.enable_auto_reconnect = enabled;
    }

    /// Get the current attempt count
    pub fn attempt_count(&self) -> u32 {
        self.attempt_count
    }

    /// Get the maximum attempts allowed
    pub fn max_attempts(&self) -> u32 {
        self.config.max_reconnect_attempts
    }

    /// Get the last error type
    pub fn last_error_type(&self) -> Option<SshErrorType> {
        self.last_error_type
    }

    /// Get the total time spent retrying
    pub fn total_retry_time(&self) -> Duration {
        self.total_retry_time
    }

    /// Check if we should continue retrying
    pub fn should_retry(&self) -> bool {
        if !self.config.enable_auto_reconnect {
            return false;
        }

        // Check attempt limit
        if self.attempt_count >= self.config.max_reconnect_attempts {
            return false;
        }

        // Check if last error was permanent
        if let Some(error_type) = self.last_error_type {
            if !super::error_classifier::SshErrorClassifier::should_retry(error_type) {
                return false;
            }
        }

        true
    }

    /// Calculate the delay before the next reconnection attempt
    ///
    /// Uses exponential backoff algorithm:
    /// delay = min(initial_delay * (multiplier ^ attempt), max_delay)
    ///
    /// Returns None if no more retries should be attempted
    pub fn calculate_delay(&self) -> Option<Duration> {
        if !self.should_retry() {
            return None;
        }

        // Calculate base exponential delay
        let attempt = self.attempt_count;
        let initial_delay = self.config.initial_delay_ms as f64;
        let multiplier = self.config.backoff_multiplier as f64;
        let max_delay = self.config.max_delay_ms as f64;

        // delay = initial * (multiplier ^ attempt)
        let exponential_delay = initial_delay * multiplier.powi(attempt as i32);

        // Apply additional multiplier for rate-limited errors
        let adjusted_delay = if self.last_error_type == Some(SshErrorType::RateLimited) {
            exponential_delay * 2.0 // Double delay for rate-limited scenarios
        } else if self.last_error_type == Some(SshErrorType::ResourceExhausted) {
            exponential_delay * 1.5 // 50% more for resource exhaustion
        } else {
            exponential_delay
        };

        // Cap at max delay
        let final_delay_ms = adjusted_delay.min(max_delay) as u64;

        Some(Duration::from_millis(final_delay_ms))
    }

    /// Record a reconnection attempt
    ///
    /// Call this after a failed connection attempt to update internal state
    pub fn record_attempt(&mut self, error_type: SshErrorType) {
        self.attempt_count += 1;
        self.last_error_type = Some(error_type);
        self.last_attempt_time = Some(Instant::now());

        // Update total retry time with estimated delay
        if let Some(delay) = self.calculate_delay() {
            self.total_retry_time += delay;
        }
    }

    /// Reset the manager state (call on successful connection)
    pub fn reset(&mut self) {
        self.attempt_count = 0;
        self.last_error_type = None;
        self.last_attempt_time = None;
        self.total_retry_time = Duration::ZERO;
    }

    /// Get time elapsed since last attempt
    pub fn time_since_last_attempt(&self) -> Option<Duration> {
        self.last_attempt_time.map(|t| t.elapsed())
    }

    /// Get a status summary for logging/debugging
    pub fn status_summary(&self) -> String {
        let error_desc = self.last_error_type
            .map(|t| super::error_classifier::SshErrorClassifier::describe(t))
            .unwrap_or("No error");

        format!(
            "Attempt {}/{} - Last error: {} - Total retry time: {:?}",
            self.attempt_count,
            self.config.max_reconnect_attempts,
            error_desc,
            self.total_retry_time
        )
    }

    /// Check if we're in a rapid retry state (multiple recent failures)
    pub fn is_rapid_retry(&self) -> bool {
        if self.attempt_count < 3 {
            return false;
        }

        // If we've had 3+ attempts in the last 30 seconds, it's rapid retry
        if let Some(time) = self.time_since_last_attempt() {
            time < Duration::from_secs(30)
        } else {
            false
        }
    }

    /// Get the next delay synchronously (blocking)
    /// Returns the delay duration or None if no more retries
    pub fn get_next_delay(&self) -> Option<Duration> {
        self.calculate_delay()
    }

    /// Wait for the calculated delay (blocking)
    /// Returns true if waited, false if no retry should happen
    pub fn wait_for_retry(&self) -> bool {
        if let Some(delay) = self.calculate_delay() {
            std::thread::sleep(delay);
            true
        } else {
            false
        }
    }
}

/// Builder for creating ReconnectManager with custom settings
pub struct ReconnectManagerBuilder {
    config: ReconnectSettings,
}

impl ReconnectManagerBuilder {
    pub fn new() -> Self {
        Self {
            config: ReconnectSettings::default(),
        }
    }

    pub fn max_attempts(mut self, attempts: u32) -> Self {
        self.config.max_reconnect_attempts = attempts;
        self
    }

    pub fn initial_delay_ms(mut self, ms: u32) -> Self {
        self.config.initial_delay_ms = ms;
        self
    }

    pub fn max_delay_ms(mut self, ms: u32) -> Self {
        self.config.max_delay_ms = ms;
        self
    }

    pub fn backoff_multiplier(mut self, multiplier: f32) -> Self {
        self.config.backoff_multiplier = multiplier;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.config.enable_auto_reconnect = enabled;
        self
    }

    pub fn build(self) -> ReconnectManager {
        ReconnectManager::new(self.config)
    }
}

impl Default for ReconnectManagerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_settings() {
        let manager = ReconnectManager::with_defaults();
        assert_eq!(manager.attempt_count(), 0);
        assert!(manager.is_enabled());
        assert!(manager.should_retry());
    }

    #[test]
    fn test_exponential_backoff() {
        let mut manager = ReconnectManager::with_defaults();

        // First attempt
        manager.record_attempt(SshErrorType::Temporary);
        let delay1 = manager.calculate_delay().unwrap();
        assert_eq!(delay1, Duration::from_millis(1000)); // 1000 * 2^1 = 2000, but we use attempt as exponent start

        // Second attempt
        manager.record_attempt(SshErrorType::Temporary);
        let delay2 = manager.calculate_delay().unwrap();
        assert!(delay2 > delay1);

        // Third attempt
        manager.record_attempt(SshErrorType::Temporary);
        let delay3 = manager.calculate_delay().unwrap();
        assert!(delay3 > delay2);
    }

    #[test]
    fn test_max_delay_cap() {
        let manager = ReconnectManagerBuilder::new()
            .initial_delay_ms(10000)
            .max_delay_ms(30000)
            .backoff_multiplier(3.0)
            .build();

        // Even with high multiplier, delay should be capped
        let delay = manager.calculate_delay().unwrap();
        assert!(delay <= Duration::from_millis(30000));
    }

    #[test]
    fn test_permanent_error_no_retry() {
        let mut manager = ReconnectManager::with_defaults();
        manager.record_attempt(SshErrorType::Permanent);

        assert!(!manager.should_retry());
        assert!(manager.calculate_delay().is_none());
    }

    #[test]
    fn test_rate_limit_extended_delay() {
        let mut manager1 = ReconnectManager::with_defaults();
        let mut manager2 = ReconnectManager::with_defaults();

        manager1.record_attempt(SshErrorType::Temporary);
        manager2.record_attempt(SshErrorType::RateLimited);

        let delay1 = manager1.calculate_delay().unwrap();
        let delay2 = manager2.calculate_delay().unwrap();

        // Rate-limited should have longer delay
        assert!(delay2 > delay1);
    }

    #[test]
    fn test_reset() {
        let mut manager = ReconnectManager::with_defaults();

        manager.record_attempt(SshErrorType::Temporary);
        manager.record_attempt(SshErrorType::Temporary);
        assert_eq!(manager.attempt_count(), 2);

        manager.reset();
        assert_eq!(manager.attempt_count(), 0);
        assert!(manager.last_error_type().is_none());
    }

    #[test]
    fn test_max_attempts() {
        let mut manager = ReconnectManagerBuilder::new()
            .max_attempts(3)
            .build();

        manager.record_attempt(SshErrorType::Temporary);
        assert!(manager.should_retry());

        manager.record_attempt(SshErrorType::Temporary);
        assert!(manager.should_retry());

        manager.record_attempt(SshErrorType::Temporary);
        // After 3 attempts, should not retry
        assert!(!manager.should_retry());
    }

    #[test]
    fn test_disabled() {
        let manager = ReconnectManagerBuilder::new()
            .enabled(false)
            .build();

        assert!(!manager.should_retry());
    }
}
