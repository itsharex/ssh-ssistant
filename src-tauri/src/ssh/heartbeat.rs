use crate::models::HeartbeatSettings;
use serde::{Deserialize, Serialize};
use ssh2::Session;
use std::io::{ErrorKind, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Heartbeat detection level
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HeartbeatLevel {
    /// TCP layer (socket2 keepalive) - fastest, least reliable
    Tcp,
    /// SSH layer (keepalive_send) - medium speed, good reliability
    Ssh,
    /// Application layer (execute 'echo heartbeat') - slowest, most reliable
    App,
}

/// Result of a heartbeat check
#[derive(Debug, Clone)]
pub enum HeartbeatResult {
    /// All checks passed
    Success,
    /// Check timed out
    Timeout,
    /// Check failed with error
    Failed(String),
    /// Session is dead and needs reconnection
    SessionDead,
}

/// Current heartbeat status
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatStatus {
    /// TCP layer alive status
    pub tcp_alive: bool,
    /// SSH layer alive status
    pub ssh_alive: bool,
    /// Application layer alive status
    pub app_alive: bool,
    /// Timestamp of last successful heartbeat
    pub last_success: Option<i64>,
    /// Number of consecutive failures
    pub consecutive_failures: u32,
    /// Latency in milliseconds (from last app-level check)
    pub latency_ms: Option<u32>,
}

impl Default for HeartbeatStatus {
    fn default() -> Self {
        Self {
            tcp_alive: true,
            ssh_alive: true,
            app_alive: true,
            last_success: None,
            consecutive_failures: 0,
            latency_ms: None,
        }
    }
}

/// Recommended action based on heartbeat failures
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HeartbeatAction {
    /// No action needed - connection is healthy
    None,
    /// Send keepalive packet
    SendKeepalive,
    /// Attempt background silent reconnection
    ReconnectBackground,
    /// Notify user of connection issues
    NotifyUser,
    /// Force reconnection
    ForceReconnect,
}

/// Heartbeat manager for layered connection health monitoring
pub struct HeartbeatManager {
    settings: HeartbeatSettings,
    status: HeartbeatStatus,
    last_tcp_check: Instant,
    last_ssh_check: Instant,
    last_app_check: Instant,
    shutdown_signal: Option<Arc<AtomicBool>>,
}

impl HeartbeatManager {
    /// Create a new heartbeat manager with given settings
    pub fn new(settings: HeartbeatSettings) -> Self {
        Self {
            settings,
            status: HeartbeatStatus::default(),
            last_tcp_check: Instant::now(),
            last_ssh_check: Instant::now(),
            last_app_check: Instant::now(),
            shutdown_signal: None,
        }
    }

    /// Create heartbeat manager with shutdown signal
    pub fn with_shutdown(settings: HeartbeatSettings, shutdown_signal: Arc<AtomicBool>) -> Self {
        Self {
            settings,
            status: HeartbeatStatus::default(),
            last_tcp_check: Instant::now(),
            last_ssh_check: Instant::now(),
            last_app_check: Instant::now(),
            shutdown_signal: Some(shutdown_signal),
        }
    }

    /// Get the settings
    pub fn settings(&self) -> &HeartbeatSettings {
        &self.settings
    }

    /// Update settings
    pub fn update_settings(&mut self, settings: HeartbeatSettings) {
        self.settings = settings;
    }

    /// Check if we should perform heartbeat at given level
    pub fn should_check(&self, level: HeartbeatLevel) -> bool {
        if let Some(signal) = &self.shutdown_signal {
            if signal.load(Ordering::Relaxed) {
                return false;
            }
        }

        let (last_check, interval) = match level {
            HeartbeatLevel::Tcp => (&self.last_tcp_check, self.settings.tcp_keepalive_interval_secs),
            HeartbeatLevel::Ssh => (&self.last_ssh_check, self.settings.ssh_keepalive_interval_secs),
            HeartbeatLevel::App => (&self.last_app_check, self.settings.app_heartbeat_interval_secs),
        };

        last_check.elapsed() >= Duration::from_secs(interval as u64)
    }

    /// Perform layered heartbeat detection
    /// Returns the result of the deepest check performed
    pub fn perform_heartbeat(&mut self, session: &Session) -> HeartbeatResult {
        let mut result = HeartbeatResult::Success;

        // 1. TCP layer check (fastest)
        if self.should_check(HeartbeatLevel::Tcp) {
            self.status.tcp_alive = self.check_tcp(session);
            self.last_tcp_check = Instant::now();

            if !self.status.tcp_alive {
                result = HeartbeatResult::Failed("TCP layer not responding".to_string());
            }
        }

        // 2. SSH layer check (medium)
        if self.should_check(HeartbeatLevel::Ssh) && self.status.tcp_alive {
            match self.check_ssh(session) {
                HeartbeatResult::Success => {
                    self.status.ssh_alive = true;
                }
                other => {
                    self.status.ssh_alive = false;
                    result = other;
                }
            }
            self.last_ssh_check = Instant::now();
        }

        // 3. Application layer check (slowest but most reliable)
        // Only perform if SSH layer is OK and it's time
        if self.should_check(HeartbeatLevel::App) && self.status.ssh_alive {
            match self.check_app(session) {
                HeartbeatResult::Success => {
                    self.status.app_alive = true;
                }
                other => {
                    self.status.app_alive = false;
                    result = other;
                }
            }
            self.last_app_check = Instant::now();
        }

        // Update failure tracking
        match &result {
            HeartbeatResult::Success => {
                self.status.consecutive_failures = 0;
                self.status.last_success = Some(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs() as i64,
                );
            }
            HeartbeatResult::SessionDead => {
                self.status.consecutive_failures += 1;
                self.status.tcp_alive = false;
                self.status.ssh_alive = false;
                self.status.app_alive = false;
            }
            _ => {
                self.status.consecutive_failures += 1;
            }
        }

        result
    }

    /// TCP layer health check
    /// Uses SSH keepalive_send which operates at TCP level
    fn check_tcp(&self, session: &Session) -> bool {
        // In non-blocking mode, keepalive_send might return WouldBlock
        // We use a simple heuristic: if session is still valid, TCP is alive
        // The actual TCP keepalive is configured at socket level
        match session.keepalive_send() {
            Ok(_) => true,
            Err(e) => {
                // WouldBlock in non-blocking mode is not a real failure
                if e.code() == ssh2::ErrorCode::Session(-37) {
                    true
                } else {
                    eprintln!("[Heartbeat] TCP check failed: {}", e);
                    false
                }
            }
        }
    }

    /// SSH layer health check
    /// Sends SSH keepalive and verifies response
    fn check_ssh(&self, session: &Session) -> HeartbeatResult {
        let timeout = Duration::from_secs(self.settings.heartbeat_timeout_secs as u64);
        let start = Instant::now();

        // Try to open a channel to verify SSH session is responsive
        loop {
            match session.channel_session() {
                Ok(mut channel) => {
                    // Immediately close the channel - we just want to verify connectivity
                    let _ = channel.close();
                    return HeartbeatResult::Success;
                }
                Err(e) => {
                    // Check for WouldBlock (EAGAIN) in non-blocking mode
                    if e.code() == ssh2::ErrorCode::Session(-37) {
                        if start.elapsed() > timeout {
                            return HeartbeatResult::Timeout;
                        }
                        // Short sleep before retry
                        std::thread::sleep(Duration::from_millis(50));
                        continue;
                    }
                    // Real error - session is dead
                    return HeartbeatResult::SessionDead;
                }
            }
        }
    }

    /// Application layer health check
    /// Executes a simple command to verify end-to-end connectivity
    fn check_app(&mut self, session: &Session) -> HeartbeatResult {
        let timeout = Duration::from_secs(self.settings.heartbeat_timeout_secs as u64);
        let start = Instant::now();

        // Try to open a channel
        let mut channel = loop {
            if let Some(signal) = &self.shutdown_signal {
                if signal.load(Ordering::Relaxed) {
                    return HeartbeatResult::Failed("Shutdown requested".to_string());
                }
            }

            match session.channel_session() {
                Ok(ch) => break ch,
                Err(e) => {
                    if e.code() == ssh2::ErrorCode::Session(-37) {
                        if start.elapsed() > timeout {
                            return HeartbeatResult::Timeout;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                        continue;
                    }
                    return HeartbeatResult::SessionDead;
                }
            }
        };

        // Execute a lightweight command
        match channel.exec("echo hb") {
            Ok(_) => {}
            Err(e) => {
                if e.code() == ssh2::ErrorCode::Session(-37) {
                    // WouldBlock - wait and retry once
                    std::thread::sleep(Duration::from_millis(100));
                    if let Err(e) = channel.exec("echo hb") {
                        return HeartbeatResult::Failed(format!("Exec failed: {}", e));
                    }
                } else {
                    return HeartbeatResult::Failed(format!("Exec failed: {}", e));
                }
            }
        }

        // Read response with timeout
        let mut response = String::new();
        let mut buf = [0u8; 64];

        loop {
            if let Some(signal) = &self.shutdown_signal {
                if signal.load(Ordering::Relaxed) {
                    let _ = channel.close();
                    return HeartbeatResult::Failed("Shutdown requested".to_string());
                }
            }

            match channel.read(&mut buf) {
                Ok(0) => break, // EOF
                Ok(n) => {
                    response.push_str(&String::from_utf8_lossy(&buf[..n]));
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    if start.elapsed() > timeout {
                        let _ = channel.close();
                        return HeartbeatResult::Timeout;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
                Err(e) => {
                    let _ = channel.close();
                    return HeartbeatResult::Failed(format!("Read failed: {}", e));
                }
            }
        }

        let _ = channel.close();

        // Calculate and store latency
        let latency_ms = start.elapsed().as_millis() as u32;
        self.status.latency_ms = Some(latency_ms);

        // Verify response
        if response.trim() == "hb" {
            HeartbeatResult::Success
        } else {
            HeartbeatResult::Failed(format!("Unexpected response: {}", response.trim()))
        }
    }

    /// Get recommended action based on current status
    pub fn get_recommended_action(&self) -> HeartbeatAction {
        let failures = self.status.consecutive_failures;

        if failures == 0 {
            return HeartbeatAction::None;
        }

        // Progressive action based on failure count
        if failures >= self.settings.failed_heartbeats_before_action + 2 {
            HeartbeatAction::ForceReconnect
        } else if failures >= self.settings.failed_heartbeats_before_action + 1 {
            HeartbeatAction::NotifyUser
        } else if failures >= self.settings.failed_heartbeats_before_action {
            HeartbeatAction::ReconnectBackground
        } else if failures >= 1 {
            HeartbeatAction::SendKeepalive
        } else {
            HeartbeatAction::None
        }
    }

    /// Get current heartbeat status
    pub fn get_status(&self) -> &HeartbeatStatus {
        &self.status
    }

    /// Get mutable status reference
    pub fn get_status_mut(&mut self) -> &mut HeartbeatStatus {
        &mut self.status
    }

    /// Reset status (call after successful reconnection)
    pub fn reset(&mut self) {
        self.status = HeartbeatStatus::default();
        self.last_tcp_check = Instant::now();
        self.last_ssh_check = Instant::now();
        self.last_app_check = Instant::now();
    }

    /// Check if connection is healthy
    pub fn is_healthy(&self) -> bool {
        self.status.tcp_alive && self.status.ssh_alive
    }

    /// Get the minimum interval for the main loop sleep
    pub fn get_min_check_interval(&self) -> Duration {
        let min_secs = self.settings.ssh_keepalive_interval_secs
            .min(self.settings.app_heartbeat_interval_secs)
            .min(self.settings.tcp_keepalive_interval_secs);

        Duration::from_secs(min_secs as u64 / 2) // Check twice as often as fastest interval
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_heartbeat_action_progression() {
        let settings = HeartbeatSettings {
            failed_heartbeats_before_action: 3,
            ..Default::default()
        };
        let mut manager = HeartbeatManager::new(settings);

        // 0 failures
        assert_eq!(manager.get_recommended_action(), HeartbeatAction::None);

        // 1 failure
        manager.status.consecutive_failures = 1;
        assert_eq!(manager.get_recommended_action(), HeartbeatAction::SendKeepalive);

        // 3 failures (threshold)
        manager.status.consecutive_failures = 3;
        assert_eq!(manager.get_recommended_action(), HeartbeatAction::ReconnectBackground);

        // 4 failures
        manager.status.consecutive_failures = 4;
        assert_eq!(manager.get_recommended_action(), HeartbeatAction::NotifyUser);

        // 5 failures
        manager.status.consecutive_failures = 5;
        assert_eq!(manager.get_recommended_action(), HeartbeatAction::ForceReconnect);
    }

    #[test]
    fn test_reset() {
        let mut manager = HeartbeatManager::new(HeartbeatSettings::default());
        manager.status.consecutive_failures = 5;
        manager.status.tcp_alive = false;

        manager.reset();

        assert_eq!(manager.status.consecutive_failures, 0);
        assert!(manager.status.tcp_alive);
        assert!(manager.status.ssh_alive);
        assert!(manager.status.app_alive);
    }
}
