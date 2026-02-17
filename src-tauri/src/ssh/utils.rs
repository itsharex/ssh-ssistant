use hex;
use sha2::{Digest, Sha256};
use ssh2::Session;
use std::io::{ErrorKind, Read};

use std::thread;
use std::time::Duration;
use tauri::AppHandle;

// Helper to retry ssh2 operations that might return EAGAIN/WouldBlock
// Maximum of 3 retries to prevent infinite loops on persistent errors
pub fn ssh2_retry<F, T>(mut f: F) -> Result<T, ssh2::Error>
where
    F: FnMut() -> Result<T, ssh2::Error>,
{
    const MAX_RETRIES: u32 = 3;
    for attempt in 0..=MAX_RETRIES {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) => {
                if e.code() == ssh2::ErrorCode::Session(-37) && attempt < MAX_RETRIES {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                return Err(e);
            }
        }
    }
    unreachable!("Loop always returns")
}

// 异步执行SSH操作，避免阻塞主线程
pub async fn execute_ssh_operation<F, T>(operation: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || operation())
        .await
        .map_err(|e| {
            // 转换 JoinError 为适当的错误类型
            format!("Task join error: {}", e)
        })?
}

// Get SFTP buffer size from settings
pub fn get_sftp_buffer_size(app: Option<&AppHandle>) -> usize {
    if let Some(app_handle) = app {
        if let Ok(settings) = crate::db::get_settings(app_handle.clone()) {
            return (settings.file_manager.sftp_buffer_size * 1024) as usize; // Convert KB to bytes
        }
    }
    // Default to 512KB if settings not available
    512 * 1024
}

pub fn get_remote_file_hash(sess: &Session, path: &str) -> Result<Option<String>, String> {
    let mut channel = ssh2_retry(|| sess.channel_session())
        .map_err(|e| format!("Failed to create channel: {}", e))?;
    // Try sha256sum first
    let cmd = format!("sha256sum '{}'", path);
    ssh2_retry(|| channel.exec(&cmd)).map_err(|e| format!("Failed to execute command: {}", e))?;

    let mut s = String::new();
    let mut buf = [0u8; 1024];
    let start_time = std::time::Instant::now();
    let timeout = Duration::from_secs(10);

    loop {
        if start_time.elapsed() > timeout {
            return Err("Command timeout".to_string());
        }

        match channel.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => s.push_str(&String::from_utf8_lossy(&buf[..n])),
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    ssh2_retry(|| channel.wait_close())
        .map_err(|e| format!("Failed to wait for channel close: {}", e))?;

    if channel.exit_status().unwrap_or(-1) == 0 {
        let parts: Vec<&str> = s.split_whitespace().collect();
        if let Some(hash) = parts.get(0) {
            return Ok(Some(hash.to_string()));
        }
    }

    // Fallback to md5sum
    let mut channel = ssh2_retry(|| sess.channel_session())
        .map_err(|e| format!("Failed to create channel for md5sum: {}", e))?;
    let cmd = format!("md5sum '{}'", path);
    ssh2_retry(|| channel.exec(&cmd))
        .map_err(|e| format!("Failed to execute md5sum command: {}", e))?;

    let mut s = String::new();
    let mut buf = [0u8; 1024];
    let start_time = std::time::Instant::now();

    loop {
        if start_time.elapsed() > timeout {
            return Err("Command timeout".to_string());
        }

        match channel.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => s.push_str(&String::from_utf8_lossy(&buf[..n])),
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    ssh2_retry(|| channel.wait_close())
        .map_err(|e| format!("Failed to wait for channel close: {}", e))?;

    if channel.exit_status().unwrap_or(-1) == 0 {
        let parts: Vec<&str> = s.split_whitespace().collect();
        if let Some(hash) = parts.get(0) {
            return Ok(Some(hash.to_string()));
        }
    }

    Ok(None)
}

pub fn compute_local_file_hash(path: &std::path::Path, limit: u64) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    let mut read = 0u64;

    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }

        let to_hash = if read + (n as u64) > limit {
            (limit - read) as usize
        } else {
            n
        };

        hasher.update(&buf[..to_hash]);
        read += to_hash as u64;

        if read >= limit {
            break;
        }
    }

    Ok(hex::encode(hasher.finalize()))
}

pub fn get_dir_size(path: &std::path::Path) -> u64 {
    let mut size = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_dir() {
                    size += get_dir_size(&entry.path());
                } else {
                    size += meta.len();
                }
            }
        }
    }
    size
}
