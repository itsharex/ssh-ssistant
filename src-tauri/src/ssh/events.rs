use tauri::{AppHandle, Emitter};
use crate::models::{ConnectionStatusEvent, ConnectionStatus, ConnectionMetrics};

pub const EVENT_CONNECTION_STATUS: &str = "connection:status";
pub const EVENT_CONNECTION_ERROR: &str = "connection:error";
pub const EVENT_CONNECTION_RECONNECT: &str = "connection:reconnect";

/// Connection event emitter for sending connection status updates to the frontend
pub struct ConnectionEventEmitter {
    app_handle: AppHandle,
}

impl ConnectionEventEmitter {
    pub fn new(app_handle: AppHandle) -> Self {
        Self { app_handle }
    }

    /// Emit a connection status change event
    pub fn emit_status_change(
        &self,
        session_id: &str,
        status: ConnectionStatus,
        details: Option<&str>,
    ) {
        let event = ConnectionStatusEvent {
            session_id: session_id.to_string(),
            status,
            timestamp: chrono::Utc::now().timestamp_millis(),
            details: details.map(|s| s.to_string()),
            metrics: None,
        };

        if let Err(e) = self.app_handle.emit(EVENT_CONNECTION_STATUS, &event) {
            eprintln!("Failed to emit connection status event: {}", e);
        }
    }

    /// Emit a connection status change event with metrics
    pub fn emit_status_change_with_metrics(
        &self,
        session_id: &str,
        status: ConnectionStatus,
        details: Option<&str>,
        metrics: ConnectionMetrics,
    ) {
        let event = ConnectionStatusEvent {
            session_id: session_id.to_string(),
            status,
            timestamp: chrono::Utc::now().timestamp_millis(),
            details: details.map(|s| s.to_string()),
            metrics: Some(metrics),
        };

        if let Err(e) = self.app_handle.emit(EVENT_CONNECTION_STATUS, &event) {
            eprintln!("Failed to emit connection status event: {}", e);
        }
    }

    /// Emit a connection error event
    pub fn emit_error(&self, session_id: &str, error: &str) {
        let event = ConnectionStatusEvent {
            session_id: session_id.to_string(),
            status: ConnectionStatus::Error,
            timestamp: chrono::Utc::now().timestamp_millis(),
            details: Some(error.to_string()),
            metrics: None,
        };

        if let Err(e) = self.app_handle.emit(EVENT_CONNECTION_ERROR, &event) {
            eprintln!("Failed to emit connection error event: {}", e);
        }
    }

    /// Emit a reconnection attempt event
    pub fn emit_reconnect_attempt(
        &self,
        session_id: &str,
        attempt: u32,
        max_attempts: u32,
        delay_ms: u32,
    ) {
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct ReconnectEvent {
            session_id: String,
            attempt: u32,
            max_attempts: u32,
            delay_ms: u32,
        }

        let event = ReconnectEvent {
            session_id: session_id.to_string(),
            attempt,
            max_attempts,
            delay_ms,
        };

        if let Err(e) = self.app_handle.emit(EVENT_CONNECTION_RECONNECT, &event) {
            eprintln!("Failed to emit reconnect attempt event: {}", e);
        }
    }
}

/// Helper functions for creating ConnectionMetrics
impl ConnectionMetrics {
    pub fn new() -> Self {
        Self {
            uptime_secs: 0,
            bytes_sent: 0,
            bytes_received: 0,
            latency_ms: 0,
            reconnect_count: 0,
            last_error: None,
        }
    }

    pub fn with_uptime(mut self, uptime_secs: u64) -> Self {
        self.uptime_secs = uptime_secs;
        self
    }

    pub fn with_bytes(mut self, sent: u64, received: u64) -> Self {
        self.bytes_sent = sent;
        self.bytes_received = received;
        self
    }

    pub fn with_latency(mut self, latency_ms: u32) -> Self {
        self.latency_ms = latency_ms;
        self
    }

    pub fn with_reconnect_count(mut self, count: u32) -> Self {
        self.reconnect_count = count;
        self
    }

    pub fn with_last_error(mut self, error: Option<String>) -> Self {
        self.last_error = error;
        self
    }
}

impl Default for ConnectionMetrics {
    fn default() -> Self {
        Self::new()
    }
}
