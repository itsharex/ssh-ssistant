use crate::models::{AdaptiveParams, NetworkAdaptiveSettings, NetworkQuality, NetworkStatus};
use ssh2::Session;
use std::collections::VecDeque;
use std::io::Read;
use std::time::{Duration, Instant};

const MAX_LATENCY_HISTORY: usize = 10;

impl NetworkQuality {
    pub fn from_latency(latency_ms: u32) -> Self {
        if latency_ms < 50 {
            NetworkQuality::Excellent
        } else if latency_ms < 150 {
            NetworkQuality::Good
        } else if latency_ms < 300 {
            NetworkQuality::Fair
        } else {
            NetworkQuality::Poor
        }
    }
}

pub struct NetworkMonitor {
    settings: NetworkAdaptiveSettings,
    current_status: NetworkStatus,
    latency_history: VecDeque<u32>,
    last_check: Instant,
    bandwidth_samples: VecDeque<(u64, Instant)>,
}

impl NetworkMonitor {
    pub fn new(settings: NetworkAdaptiveSettings) -> Self {
        Self {
            settings,
            current_status: NetworkStatus::default(),
            latency_history: VecDeque::with_capacity(MAX_LATENCY_HISTORY),
            last_check: Instant::now(),
            bandwidth_samples: VecDeque::with_capacity(10),
        }
    }

    pub fn with_default_settings() -> Self {
        Self::new(NetworkAdaptiveSettings::default())
    }

    pub fn update_settings(&mut self, settings: NetworkAdaptiveSettings) {
        self.settings = settings;
    }

    pub fn get_settings(&self) -> &NetworkAdaptiveSettings {
        &self.settings
    }

    pub fn should_check(&self) -> bool {
        if !self.settings.enable_adaptive {
            return false;
        }
        self.last_check.elapsed() >= Duration::from_secs(self.settings.latency_check_interval_secs as u64)
    }

    pub fn measure_latency(&mut self, session: &Session) -> Result<u32, String> {
        let start = Instant::now();

        let mut channel = session.channel_session().map_err(|e| e.to_string())?;

        channel.exec("echo ping").map_err(|e| e.to_string())?;

        let mut buf = [0u8; 64];
        let mut total_read = 0;
        let timeout = Duration::from_secs(5);
        let read_start = Instant::now();

        loop {
            if read_start.elapsed() > timeout {
                let _ = channel.close();
                return Err("Latency measurement timeout".to_string());
            }

            match channel.read(&mut buf[total_read..]) {
                Ok(0) => break,
                Ok(n) => {
                    total_read += n;
                    if total_read >= buf.len() {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => {
                    let _ = channel.close();
                    return Err(e.to_string());
                }
            }
        }

        let _ = channel.wait_close();

        let latency_ms = start.elapsed().as_millis() as u32;
        self.update_latency_history(latency_ms);
        self.update_status();

        Ok(latency_ms)
    }

    pub fn estimate_bandwidth(&mut self, bytes_transferred: u64, duration: Duration) {
        if duration.as_millis() > 0 {
            let _duration_secs = duration.as_secs_f64();

            self.bandwidth_samples.push_back((bytes_transferred, Instant::now()));

            if self.bandwidth_samples.len() > 10 {
                self.bandwidth_samples.pop_front();
            }

            let avg_kbps = self.calculate_avg_bandwidth();
            self.current_status.bandwidth_kbps = Some(avg_kbps);
        }
    }

    fn calculate_avg_bandwidth(&self) -> u32 {
        if self.bandwidth_samples.is_empty() {
            return 0;
        }

        let total_bytes: u64 = self.bandwidth_samples.iter().map(|(b, _)| *b).sum();

        if let Some((_, first_time)) = self.bandwidth_samples.front() {
            let duration = first_time.elapsed().as_secs_f64();
            if duration > 0.0 {
                return ((total_bytes as f64 / duration) / 1024.0) as u32;
            }
        }

        0
    }

    pub fn get_status(&self) -> &NetworkStatus {
        &self.current_status
    }

    pub fn get_recommended_params(&self) -> AdaptiveParams {
        let quality = &self.current_status.quality;

        match quality {
            NetworkQuality::Excellent => AdaptiveParams {
                heartbeat_interval_secs: 10,
                sftp_buffer_size: 1024 * 1024, // 1MB
                command_timeout_secs: 60,
                keepalive_interval_secs: 10,
            },
            NetworkQuality::Good => AdaptiveParams {
                heartbeat_interval_secs: 15,
                sftp_buffer_size: 512 * 1024, // 512KB
                command_timeout_secs: 30,
                keepalive_interval_secs: 15,
            },
            NetworkQuality::Fair => AdaptiveParams {
                heartbeat_interval_secs: 20,
                sftp_buffer_size: 256 * 1024, // 256KB
                command_timeout_secs: 45,
                keepalive_interval_secs: 20,
            },
            NetworkQuality::Poor => AdaptiveParams {
                heartbeat_interval_secs: 30,
                sftp_buffer_size: 64 * 1024, // 64KB
                command_timeout_secs: 120,
                keepalive_interval_secs: 30,
            },
            NetworkQuality::Unknown => AdaptiveParams {
                heartbeat_interval_secs: 15,
                sftp_buffer_size: 512 * 1024, // 512KB - default
                command_timeout_secs: 30,
                keepalive_interval_secs: 15,
            },
        }
    }

    fn update_latency_history(&mut self, latency_ms: u32) {
        if self.latency_history.len() >= MAX_LATENCY_HISTORY {
            self.latency_history.pop_front();
        }
        self.latency_history.push_back(latency_ms);
        self.last_check = Instant::now();
    }

    fn calculate_avg_latency(&self) -> u32 {
        if self.latency_history.is_empty() {
            return 0;
        }
        let sum: u32 = self.latency_history.iter().sum();
        sum / self.latency_history.len() as u32
    }

    fn update_status(&mut self) {
        let avg_latency = self.calculate_avg_latency();
        self.current_status.latency_ms = avg_latency;
        self.current_status.quality = NetworkQuality::from_latency(avg_latency);
        self.current_status.last_update = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
    }

    pub fn reset(&mut self) {
        self.latency_history.clear();
        self.bandwidth_samples.clear();
        self.current_status = NetworkStatus::default();
        self.last_check = Instant::now();
    }

    pub fn is_enabled(&self) -> bool {
        self.settings.enable_adaptive
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_network_quality_from_latency() {
        assert_eq!(NetworkQuality::from_latency(30), NetworkQuality::Excellent);
        assert_eq!(NetworkQuality::from_latency(50), NetworkQuality::Good);
        assert_eq!(NetworkQuality::from_latency(100), NetworkQuality::Good);
        assert_eq!(NetworkQuality::from_latency(150), NetworkQuality::Fair);
        assert_eq!(NetworkQuality::from_latency(250), NetworkQuality::Fair);
        assert_eq!(NetworkQuality::from_latency(300), NetworkQuality::Poor);
        assert_eq!(NetworkQuality::from_latency(500), NetworkQuality::Poor);
    }

    #[test]
    fn test_adaptive_params() {
        let monitor = NetworkMonitor::with_default_settings();
        let params = monitor.get_recommended_params();
        assert!(params.sftp_buffer_size >= 64 * 1024);
        assert!(params.heartbeat_interval_secs >= 10);
    }
}
