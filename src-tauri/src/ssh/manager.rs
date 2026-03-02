use super::connection::{ManagedSession, SessionSshPool};
use super::heartbeat::{HeartbeatAction, HeartbeatManager, HeartbeatResult};
use super::network_monitor::NetworkMonitor;
use super::ShellMsg;
use crate::models::{FileEntry, HeartbeatSettings, NetworkAdaptiveSettings};

use std::io::{ErrorKind, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Commands sent to the SSH Manager Actor
pub enum SshCommand {
    /// Open a shell channel
    ShellOpen {
        cols: u16,
        rows: u16,
        sender: Sender<ShellMsg>,
    },
    /// Write data to shell
    ShellWrite(Vec<u8>),
    /// Resize shell
    ShellResize { rows: u16, cols: u16 },
    /// Close shell
    ShellClose,
    /// Execute a single command
    Exec {
        command: String,
        listener: Sender<Result<String, String>>,
        cancel_flag: Option<Arc<AtomicBool>>,
        is_ai: bool,
    },
    /// List directory (SFTP)
    SftpLs {
        path: String,
        listener: Sender<Result<Vec<FileEntry>, String>>,
    },
    /// Read file (SFTP)
    SftpRead {
        path: String,
        max_len: Option<usize>, // Added max_len support
        listener: Sender<Result<Vec<u8>, String>>,
    },
    /// Write file (SFTP)
    SftpWrite {
        path: String,
        content: Vec<u8>,
        mode: Option<String>,
        listener: Sender<Result<(), String>>,
    },
    /// Create directory (SFTP)
    SftpMkdir {
        path: String,
        listener: Sender<Result<(), String>>,
    },
    /// Create file (SFTP) - Empty
    SftpCreate {
        path: String,
        listener: Sender<Result<(), String>>,
    },
    /// Change permissions (SFTP)
    SftpChmod {
        path: String,
        mode: u32,
        listener: Sender<Result<(), String>>,
    },
    /// Delete item (SFTP)
    SftpDelete {
        path: String,
        is_dir: bool,
        listener: Sender<Result<(), String>>,
    },
    /// Rename item (SFTP)
    SftpRename {
        old_path: String,
        new_path: String,
        listener: Sender<Result<(), String>>,
    },
    /// Download File (Streaming) - uses transfer_pool to avoid blocking general operations
    SftpDownload {
        remote_path: String,
        local_path: String,
        transfer_id: String,
        app_handle: tauri::AppHandle,
        listener: Sender<Result<(), String>>,
        cancel_flag: Arc<AtomicBool>,
    },
    /// Upload File (Streaming) - uses transfer_pool to avoid blocking general operations
    SftpUpload {
        local_path: String,
        remote_path: String,
        transfer_id: String,
        app_handle: tauri::AppHandle,
        listener: Sender<Result<(), String>>,
        cancel_flag: Arc<AtomicBool>,
    },

    /// Shutdown the manager
    Shutdown,
}

pub struct SshManager {
    session: ManagedSession, // Main session for shell
    pool: SessionSshPool,    // Pool for background tasks
    receiver: Receiver<SshCommand>,
    shutdown_signal: Arc<AtomicBool>, // Shared with client to force shutdown if needed

    // Active Channels
    shell_channel: Option<ssh2::Channel>,
    shell_sender: Option<Sender<ShellMsg>>,

    // Heartbeat Manager
    heartbeat_manager: HeartbeatManager,

    // Network Monitor
    network_monitor: Arc<Mutex<NetworkMonitor>>,
    latency_tx: Option<Sender<()>>,
}

impl SshManager {
    pub fn new(
        session: ManagedSession,
        pool: SessionSshPool,
        receiver: Receiver<SshCommand>,
        shutdown_signal: Arc<AtomicBool>,
    ) -> Self {
        Self::with_heartbeat_settings(
            session,
            pool,
            receiver,
            shutdown_signal,
            HeartbeatSettings::default(),
        )
    }

    pub fn with_heartbeat_settings(
        session: ManagedSession,
        pool: SessionSshPool,
        receiver: Receiver<SshCommand>,
        shutdown_signal: Arc<AtomicBool>,
        heartbeat_settings: HeartbeatSettings,
    ) -> Self {
        let heartbeat_manager = HeartbeatManager::with_shutdown(
            heartbeat_settings,
            shutdown_signal.clone(),
        );
        let network_monitor = Arc::new(Mutex::new(NetworkMonitor::with_default_settings()));

        let (latency_tx, latency_rx) = std::sync::mpsc::channel();
        let pool_clone = pool.clone();
        let monitor_clone = Arc::clone(&network_monitor);

        thread::spawn(move || {
            loop {
                if latency_rx.recv().is_err() {
                    break;
                }

                let should_check = {
                    if let Ok(monitor) = monitor_clone.lock() {
                        monitor.should_check()
                    } else {
                        false
                    }
                };

                if !should_check {
                    continue;
                }

                let session_mutex = match pool_clone.get_transfer_session() {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("[NetworkMonitor] Failed to get transfer session: {}", e);
                        continue;
                    }
                };

                let session_guard = match session_mutex.lock() {
                    Ok(s) => s,
                    Err(_) => continue,
                };

                if let Ok(mut monitor) = monitor_clone.lock() {
                    if let Err(e) = monitor.measure_latency(&session_guard.session) {
                        eprintln!("[NetworkMonitor] Failed to measure latency (worker): {}", e);
                    } else {
                        let status = monitor.get_status();
                        let params = monitor.get_recommended_params();
                        eprintln!(
                            "[NetworkMonitor] Latency: {}ms, Quality: {:?}, Recommended buffer: {}KB",
                            status.latency_ms,
                            status.quality,
                            params.sftp_buffer_size / 1024
                        );
                    }
                }
            }
        });

        Self {
            session,
            pool,
            receiver,
            shutdown_signal,
            shell_channel: None,
            shell_sender: None,
            heartbeat_manager,
            network_monitor,
            latency_tx: Some(latency_tx),
        }
    }

    /// Update heartbeat settings at runtime
    pub fn update_heartbeat_settings(&mut self, settings: HeartbeatSettings) {
        self.heartbeat_manager.update_settings(settings);
    }

    /// Update network adaptive settings at runtime
    pub fn update_network_adaptive_settings(&mut self, settings: NetworkAdaptiveSettings) {
        if let Ok(mut monitor) = self.network_monitor.lock() {
            monitor.update_settings(settings);
        }
    }

    /// Get current network status
    pub fn get_network_status(&self) -> crate::models::NetworkStatus {
        // Note: Return a cloned status to avoid lifetime issues
        self.network_monitor.lock().unwrap().get_status().clone()
    }

    /// Get recommended adaptive parameters
    pub fn get_adaptive_params(&self) -> crate::models::AdaptiveParams {
        self.network_monitor.lock().unwrap().get_recommended_params()
    }

    pub fn run(&mut self) {
        loop {
            // 1. Check for shutdown
            if self.shutdown_signal.load(Ordering::Relaxed) {
                break;
            }

            let mut activity = false;

            // 2. Process Incoming Commands (Batch process up to a limit to avoid starving I/O)
            // We use try_recv to avoid blocking, since we also need to poll SSH socket
            for _ in 0..10 {
                match self.receiver.try_recv() {
                    Ok(cmd) => {
                        self.handle_command(cmd);
                        activity = true;
                    }
                    Err(_) => break, // Empty or disconnected
                }
            }

            // 3. Poll Shell Channel Output
            // Correct logic attempt 2:
            // We can't easily `take` and match without putting back in every branch.
            // But `shell_channel` is `Option`.
            // Let's use `if let Some(channel) = &mut self.shell_channel`
            // But `read` requires `&mut Channel`.

            let mut shell_channel_closed = false;
            if let Some(channel) = &mut self.shell_channel {
                let mut buf = [0u8; 4096];
                match channel.read(&mut buf) {
                    Ok(0) => {
                        // EOF
                        let _ = channel.close();
                        if let Some(tx) = &self.shell_sender {
                            let _ = tx.send(ShellMsg::Exit);
                        }
                        shell_channel_closed = true;
                    }
                    Ok(n) => {
                        activity = true;
                        if let Some(tx) = &self.shell_sender {
                            let _ = tx.send(ShellMsg::Data(buf[..n].to_vec()));
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {
                        // wait
                        // thread::sleep(Duration::from_millis(5)); // sleep at end of loop
                    }
                    Err(e) => {
                        eprintln!("Shell error: {}", e);
                        let _ = channel.close();
                        if let Some(tx) = &self.shell_sender {
                            let _ = tx.send(ShellMsg::Exit);
                        }
                        shell_channel_closed = true;
                    }
                }
            }
            if shell_channel_closed {
                self.shell_channel = None;
                self.shell_sender = None;
            }

            // 4. Perform Layered Heartbeat Check
            let heartbeat_result = self.heartbeat_manager.perform_heartbeat(&self.session);

            // 4.5 Trigger Network Latency Check asynchronously (worker thread)
            if let Some(tx) = &self.latency_tx {
                let _ = tx.send(());
            }

            // 5. Handle Heartbeat Result
            match heartbeat_result {
                HeartbeatResult::Success => {
                    // Connection is healthy, also check pool
                    let _ = self.pool.heartbeat_check();
                }
                HeartbeatResult::Timeout => {
                    // Log timeout but don't take action yet
                    let status = self.heartbeat_manager.get_status();
                    if status.consecutive_failures > 0 {
                        eprintln!(
                            "[Heartbeat] Timeout detected (failures: {})",
                            status.consecutive_failures
                        );
                    }
                }
                HeartbeatResult::Failed(msg) => {
                    eprintln!("[Heartbeat] Check failed: {}", msg);
                }
                HeartbeatResult::SessionDead => {
                    eprintln!("[Heartbeat] Session appears dead");
                }
            }

            // 6. Take Action Based on Heartbeat Status
            let action = self.heartbeat_manager.get_recommended_action();
            match action {
                HeartbeatAction::None => {
                    // All good
                }
                HeartbeatAction::SendKeepalive => {
                    // Send immediate keepalive
                    let _ = crate::ssh::utils::ssh2_retry(|| self.session.keepalive_send());
                }
                HeartbeatAction::ReconnectBackground => {
                    eprintln!("[Heartbeat] Attempting background reconnection...");
                    // Try to rebuild pool connections silently
                    if let Err(e) = self.pool.rebuild_all() {
                        eprintln!("[Heartbeat] Background reconnect failed: {}", e);
                    } else {
                        // Reset heartbeat status on successful reconnect
                        self.heartbeat_manager.reset();
                    }
                }
                HeartbeatAction::NotifyUser => {
                    // In a real implementation, this would emit an event to the frontend
                    eprintln!(
                        "[Heartbeat] Connection unstable - user notification recommended"
                    );
                    // Still try to reconnect
                    if let Err(e) = self.pool.rebuild_all() {
                        eprintln!("[Heartbeat] Reconnect attempt failed: {}", e);
                    }
                }
                HeartbeatAction::ForceReconnect => {
                    eprintln!("[Heartbeat] Force reconnecting...");
                    // Force rebuild all connections
                    let _ = self.pool.rebuild_all();
                    // Reset heartbeat status
                    self.heartbeat_manager.reset();
                }
            }

            // 7. Sleep if idle - use dynamic sleep based on heartbeat settings
            if !activity {
                let sleep_duration = self.heartbeat_manager.get_min_check_interval()
                    .min(Duration::from_millis(100)); // Cap at 100ms for responsiveness
                thread::sleep(sleep_duration);
            }
        }

        // Cleanup
        if let Some(mut channel) = self.shell_channel.take() {
            let _ = channel.close();
        }
        let _ = self.session.disconnect(None, "Shutdown", None);
        self.pool.close_all();
    }

    fn handle_command(&mut self, cmd: SshCommand) {
        match cmd {
            SshCommand::Shutdown => {
                self.shutdown_signal.store(true, Ordering::Relaxed);
            }
            SshCommand::ShellOpen { cols, rows, sender } => {
                // If shell exists, close it
                if let Some(mut c) = self.shell_channel.take() {
                    let _ = c.close();
                }

                // Create new channel using the main session
                match crate::ssh::utils::ssh2_retry(|| self.session.channel_session()) {
                    Ok(mut channel) => {
                        // Non-blocking is already set on session
                        // Standard setup
                        if let Err(e) = crate::ssh::utils::ssh2_retry(|| {
                            channel.request_pty(
                                "xterm",
                                None,
                                Some((cols.into(), rows.into(), 0, 0)),
                            )
                        }) {
                            eprintln!("Failed to request PTY: {}", e);
                            return;
                        }
                        if let Err(e) = crate::ssh::utils::ssh2_retry(|| channel.shell()) {
                            eprintln!("Failed to start shell: {}", e);
                            return;
                        }
                        self.shell_channel = Some(channel);
                        self.shell_sender = Some(sender);
                    }
                    Err(e) => eprintln!("Failed to create shell channel: {}", e),
                }
            }
            SshCommand::ShellWrite(data) => {
                if let Some(channel) = &mut self.shell_channel {
                    let _ = channel.write_all(&data);
                }
            }
            SshCommand::ShellResize { rows, cols } => {
                if let Some(channel) = &mut self.shell_channel {
                    let _ = channel.request_pty_size(cols.into(), rows.into(), None, None);
                }
            }
            SshCommand::ShellClose => {
                if let Some(mut channel) = self.shell_channel.take() {
                    let _ = channel.close();
                }
                self.shell_sender = None;
            }
            SshCommand::Exec {
                command,
                listener,
                cancel_flag,
                is_ai,
            } => {
                let pool = self.pool.clone();
                thread::spawn(move || {
                    let res = Self::bg_exec(pool, &command, cancel_flag.as_ref(), is_ai);
                    let _ = listener.send(res);
                });
            }
            SshCommand::SftpLs { path, listener } => {
                let pool = self.pool.clone();
                thread::spawn(move || {
                    let res = Self::bg_sftp_ls(pool, &path);
                    let _ = listener.send(res);
                });
            }
            SshCommand::SftpRead {
                path,
                max_len,
                listener,
            } => {
                let pool = self.pool.clone();
                thread::spawn(move || {
                    let res = Self::bg_sftp_read(pool, &path, max_len);
                    let _ = listener.send(res);
                });
            }
            SshCommand::SftpWrite {
                path,
                content,
                mode,
                listener,
            } => {
                let pool = self.pool.clone();
                thread::spawn(move || {
                    let res = Self::bg_sftp_write(pool, &path, &content, mode.as_deref());
                    let _ = listener.send(res);
                });
            }
            SshCommand::SftpMkdir { path, listener } => {
                let pool = self.pool.clone();
                thread::spawn(move || {
                    let res = Self::bg_sftp_simple(pool, &path, |sftp, p| {
                        sftp.mkdir(p, 0o755).map_err(|e| e.to_string())
                    });
                    let _ = listener.send(res);
                });
            }
            SshCommand::SftpCreate { path, listener } => {
                let pool = self.pool.clone();
                thread::spawn(move || {
                    let res = Self::bg_sftp_simple(pool, &path, |sftp, p| {
                        sftp.create(p).map_err(|e| e.to_string()).map(|_| ())
                    });
                    let _ = listener.send(res);
                });
            }
            SshCommand::SftpChmod {
                path,
                mode,
                listener,
            } => {
                let pool = self.pool.clone();
                thread::spawn(move || {
                    let res = Self::bg_sftp_simple(pool, &path, move |sftp, p| {
                        sftp.setstat(
                            p,
                            ssh2::FileStat {
                                perm: Some(mode),
                                size: None,
                                uid: None,
                                gid: None,
                                atime: None,
                                mtime: None,
                            },
                        )
                        .map_err(|e| e.to_string())
                    });
                    let _ = listener.send(res);
                });
            }
            SshCommand::SftpDelete {
                path,
                is_dir,
                listener,
            } => {
                let pool = self.pool.clone();
                thread::spawn(move || {
                    let res = Self::bg_sftp_delete(pool, &path, is_dir);
                    let _ = listener.send(res);
                });
            }
            SshCommand::SftpRename {
                old_path,
                new_path,
                listener,
            } => {
                let pool = self.pool.clone();
                thread::spawn(move || {
                    let res = Self::bg_sftp_rename(pool, &old_path, &new_path);
                    let _ = listener.send(res);
                });
            }
            SshCommand::SftpDownload {
                remote_path,
                local_path,
                transfer_id,
                app_handle,
                listener,
                cancel_flag,
            } => {
                eprintln!("[DEBUG] SSH Manager: SftpDownload command received: transfer_id={}, remote={}", transfer_id, remote_path);
                // 使用传输专用会话池，避免阻塞普通SFTP操作
                let pool = self.pool.clone();
                thread::spawn(move || {
                    eprintln!("[DEBUG] SftpDownload thread started: transfer_id={}", transfer_id);
                    let res = Self::bg_sftp_download_with_pool(
                        pool,
                        &remote_path,
                        &local_path,
                        &transfer_id,
                        &app_handle,
                        &cancel_flag,
                    );
                    eprintln!("[DEBUG] SftpDownload thread finished: transfer_id={}, result={:?}", transfer_id, res);
                    let _ = listener.send(res);
                });
            }
            SshCommand::SftpUpload {
                local_path,
                remote_path,
                transfer_id,
                app_handle,
                listener,
                cancel_flag,
            } => {
                eprintln!("[DEBUG] SSH Manager: SftpUpload command received: transfer_id={}, remote={}", transfer_id, remote_path);
                // 使用传输专用会话池，避免阻塞普通SFTP操作
                let pool = self.pool.clone();
                thread::spawn(move || {
                    eprintln!("[DEBUG] SftpUpload thread started: transfer_id={}", transfer_id);
                    let res = Self::bg_sftp_upload_with_pool(
                        pool,
                        &local_path,
                        &remote_path,
                        &transfer_id,
                        &app_handle,
                        &cancel_flag,
                    );
                    eprintln!("[DEBUG] SftpUpload thread finished: transfer_id={}, result={:?}", transfer_id, res);
                    let _ = listener.send(res);
                });
            }
        }
    }

    // --- Static Background Helper Functions ---

    fn bg_exec(
        pool: SessionSshPool,
        command: &str,
        cancel_flag: Option<&Arc<AtomicBool>>,
        is_ai: bool,
    ) -> Result<String, String> {
        let session_mutex = if is_ai {
            pool.get_ai_session()?
        } else {
            pool.get_background_session()?
        };
        let session = session_mutex.lock().map_err(|e| e.to_string())?;

        let mut channel = crate::ssh::utils::ssh2_retry(|| session.channel_session())
            .map_err(|e| e.to_string())?;

        crate::ssh::utils::ssh2_retry(|| channel.exec(command)).map_err(|e| e.to_string())?;

        let mut s = String::new();
        let mut buf = [0u8; 4096];

        loop {
            // Check cancellation
            if let Some(flag) = cancel_flag {
                if flag.load(Ordering::Relaxed) {
                    let _ = channel.close();
                    return Err("Command cancelled".to_string());
                }
            }

            match channel.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = String::from_utf8_lossy(&buf[..n]);
                    s.push_str(&chunk);
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(e) => return Err(e.to_string()),
            }
        }

        crate::ssh::utils::ssh2_retry(|| channel.wait_close()).ok();
        Ok(s)
    }

    fn bg_get_sftp(session: &ssh2::Session) -> Result<ssh2::Sftp, String> {
        crate::ssh::utils::ssh2_retry(|| session.sftp()).map_err(|e| e.to_string())
    }

    fn bg_sftp_ls(pool: SessionSshPool, path: &str) -> Result<Vec<FileEntry>, String> {
        let session_mutex = pool.get_background_session()?;
        let session = session_mutex.lock().map_err(|e| e.to_string())?;
        let sftp = Self::bg_get_sftp(&session)?;

        let path_path = Path::new(path);
        let files =
            crate::ssh::utils::ssh2_retry(|| sftp.readdir(path_path)).map_err(|e| e.to_string())?;

        let mut entries = Vec::new();
        for (path_buf, stat) in files {
            if let Some(name) = path_buf.file_name() {
                if let Some(name_str) = name.to_str() {
                    if name_str == "." || name_str == ".." {
                        continue;
                    }
                    // Simplified owner resolution (no cache/exec for now to avoid complexity)
                    let owner = if stat.uid.unwrap_or(0) == 0 {
                        "root"
                    } else {
                        "-"
                    }
                    .to_string();

                    entries.push(FileEntry {
                        name: name_str.to_string(),
                        is_dir: stat.is_dir(),
                        size: stat.size.unwrap_or(0),
                        mtime: stat.mtime.unwrap_or(0) as i64,
                        permissions: stat.perm.unwrap_or(0),
                        uid: stat.uid.unwrap_or(0),
                        owner,
                    });
                }
            }
        }

        entries.sort_by(|a, b| {
            if a.is_dir == b.is_dir {
                a.name.cmp(&b.name)
            } else {
                b.is_dir.cmp(&a.is_dir)
            }
        });

        Ok(entries)
    }

    fn bg_sftp_read(
        pool: SessionSshPool,
        path: &str,
        max_len: Option<usize>,
    ) -> Result<Vec<u8>, String> {
        let session_mutex = pool.get_background_session()?;
        let session = session_mutex.lock().map_err(|e| e.to_string())?;
        let sftp = Self::bg_get_sftp(&session)?;

        let mut file = crate::ssh::utils::ssh2_retry(|| sftp.open(Path::new(path)))
            .map_err(|e| e.to_string())?;

        let mut buf = Vec::new();
        let mut temp_buf = [0u8; 8192];
        loop {
            if let Some(max) = max_len {
                if buf.len() >= max {
                    break;
                }
            }

            match file.read(&mut temp_buf) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&temp_buf[..n]);
                    if let Some(max) = max_len {
                        if buf.len() > max {
                            buf.truncate(max);
                            break;
                        }
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(buf)
    }

    fn bg_sftp_write(
        pool: SessionSshPool,
        path: &str,
        content: &[u8],
        mode: Option<&str>,
    ) -> Result<(), String> {
        let session_mutex = pool.get_background_session()?;
        let session = session_mutex.lock().map_err(|e| e.to_string())?;
        let sftp = Self::bg_get_sftp(&session)?;

        use ssh2::OpenFlags;
        let mut file = if mode == Some("append") {
            crate::ssh::utils::ssh2_retry(|| {
                sftp.open_mode(
                    Path::new(path),
                    OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::APPEND,
                    0o644,
                    ssh2::OpenType::File,
                )
            })
        } else {
            crate::ssh::utils::ssh2_retry(|| {
                sftp.open_mode(
                    Path::new(path),
                    OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE,
                    0o644,
                    ssh2::OpenType::File,
                )
            })
        }
        .map_err(|e| e.to_string())?;

        let mut pos = 0;
        while pos < content.len() {
            match file.write(&content[pos..]) {
                Ok(n) => pos += n,
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(())
    }

    fn bg_sftp_simple<F>(pool: SessionSshPool, path: &str, op: F) -> Result<(), String>
    where
        F: FnOnce(&ssh2::Sftp, &Path) -> Result<(), String>,
    {
        let session_mutex = pool.get_background_session()?;
        let session = session_mutex.lock().map_err(|e| e.to_string())?;
        let sftp = Self::bg_get_sftp(&session)?;
        op(&sftp, Path::new(path))
    }

    fn bg_sftp_delete(pool: SessionSshPool, path: &str, is_dir: bool) -> Result<(), String> {
        let session_mutex = pool.get_background_session()?;
        let session = session_mutex.lock().map_err(|e| e.to_string())?;
        let sftp = Self::bg_get_sftp(&session)?;

        if is_dir {
            Self::rm_recursive_internal(&sftp, Path::new(path))
        } else {
            crate::ssh::utils::ssh2_retry(|| sftp.unlink(Path::new(path)))
                .map_err(|e| e.to_string())
        }
    }

    fn rm_recursive_internal(sftp: &ssh2::Sftp, path: &Path) -> Result<(), String> {
        let files =
            crate::ssh::utils::ssh2_retry(|| sftp.readdir(path)).map_err(|e| e.to_string())?;

        for (child_path, stat) in files {
            if let Some(name) = child_path.file_name() {
                let name = name.to_string_lossy();
                if name == "." || name == ".." {
                    continue;
                }

                if stat.is_dir() {
                    Self::rm_recursive_internal(sftp, &child_path)?;
                } else {
                    crate::ssh::utils::ssh2_retry(|| sftp.unlink(&child_path))
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        crate::ssh::utils::ssh2_retry(|| sftp.rmdir(path)).map_err(|e| e.to_string())
    }

    fn bg_sftp_rename(pool: SessionSshPool, old: &str, new: &str) -> Result<(), String> {
        let session_mutex = pool.get_background_session()?;
        let session = session_mutex.lock().map_err(|e| e.to_string())?;
        let sftp = Self::bg_get_sftp(&session)?;

        crate::ssh::utils::ssh2_retry(|| sftp.rename(Path::new(old), Path::new(new), None))
            .map_err(|e| e.to_string())
    }

    // --- Transfer Functions using dedicated Transfer Pool ---
    // These functions use get_transfer_session() instead of get_background_session()
    // to avoid blocking regular SFTP operations (ls, read, etc.) during file transfers

    fn bg_sftp_download_with_pool(
        pool: SessionSshPool,
        remote_path: &str,
        local_path: &str,
        transfer_id: &str,
        app: &tauri::AppHandle,
        cancel_flag: &Arc<AtomicBool>,
    ) -> Result<(), String> {
        use crate::ssh::ProgressPayload;
        use tauri::Emitter;

        eprintln!("[DEBUG] bg_sftp_download_with_pool ENTER: transfer_id={}, remote={}", transfer_id, remote_path);

        // Timeout configuration (default 5 minutes)
        let sftp_timeout = Duration::from_secs(300); // 5 minutes default
        let no_progress_timeout = Duration::from_secs(30); // 30 seconds without progress

        // 关键修复：使用传输专用会话池，而不是后台会话池
        // 这样大文件传输不会阻塞目录浏览等普通操作
        let session_mutex = pool.get_transfer_session()?;
        eprintln!("[DEBUG] bg_sftp_download_with_pool: Got transfer session for transfer_id={}", transfer_id);

        // Hold the session lock for the entire transfer to ensure exclusive
        // access to this SSH session (prevents concurrent SFTP ops on same session).
        let session_guard = session_mutex.lock().map_err(|e| e.to_string())?;
        let sftp = Self::bg_get_sftp(&session_guard)?;

        let mut remote = crate::ssh::utils::ssh2_retry(|| sftp.open(Path::new(remote_path)))
            .map_err(|e| e.to_string())?;

        let file_stat =
            crate::ssh::utils::ssh2_retry(|| remote.stat()).map_err(|e| e.to_string())?;
        let total = file_stat.size.unwrap_or(0);

        let mut local = std::fs::File::create(local_path).map_err(|e| e.to_string())?;

        let mut buf = [0u8; 16384];
        let mut transferred = 0u64;
        let mut last_emit = Instant::now();

        // Timeout tracking
        let transfer_start = Instant::now();
        let mut last_progress_time = Instant::now();
        let mut would_block_count = 0u32;

        loop {
            if cancel_flag.load(Ordering::Relaxed) {
                return Err("Cancelled".to_string());
            }

            // Check overall timeout
            if transfer_start.elapsed() > sftp_timeout {
                return Err(format!(
                    "Download timeout after {}s",
                    sftp_timeout.as_secs()
                ));
            }

            // Check no-progress timeout
            if last_progress_time.elapsed() > no_progress_timeout {
                return Err(format!(
                    "No progress for {}s, connection may be dead",
                    no_progress_timeout.as_secs()
                ));
            }

            match remote.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    local.write_all(&buf[..n]).map_err(|e| e.to_string())?;
                    transferred += n as u64;
                    last_progress_time = Instant::now(); // Update progress time
                    would_block_count = 0; // Reset WouldBlock counter on success

                    if last_emit.elapsed().as_millis() > 100 {
                        let _ = app.emit(
                            "transfer-progress",
                            ProgressPayload {
                                id: transfer_id.to_string(),
                                transferred,
                                total,
                            },
                        );
                        last_emit = Instant::now();
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    would_block_count += 1;
                    if would_block_count > 100 {
                        return Err(format!(
                            "Too many WouldBlock errors ({}), connection may be dead",
                            would_block_count
                        ));
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                Err(e) => return Err(e.to_string()),
            }
        }

        let _ = app.emit(
            "transfer-progress",
            ProgressPayload {
                id: transfer_id.to_string(),
                transferred: total,
                total,
            },
        );
        Ok(())
    }

    fn bg_sftp_upload_with_pool(
        pool: SessionSshPool,
        local_path: &str,
        remote_path: &str,
        transfer_id: &str,
        app: &tauri::AppHandle,
        cancel_flag: &Arc<AtomicBool>,
    ) -> Result<(), String> {
        use crate::ssh::ProgressPayload;
        use tauri::Emitter;

        eprintln!("[DEBUG] bg_sftp_upload_with_pool ENTER: transfer_id={}, remote={}", transfer_id, remote_path);

        // Timeout configuration (default 5 minutes)
        let sftp_timeout = Duration::from_secs(300); // 5 minutes default
        let no_progress_timeout = Duration::from_secs(30); // 30 seconds without progress

        // 关键修复：使用传输专用会话池，而不是后台会话池
        // 这样大文件传输不会阻塞目录浏览等普通操作
        let session_mutex = pool.get_transfer_session()?;
        eprintln!("[DEBUG] bg_sftp_upload_with_pool: Got transfer session for transfer_id={}", transfer_id);

        // Hold the session lock for the entire transfer to ensure exclusive access
        let session_guard = session_mutex.lock().map_err(|e| e.to_string())?;
        let sftp = Self::bg_get_sftp(&session_guard)?;

        let mut local = std::fs::File::open(local_path).map_err(|e| e.to_string())?;
        let metadata = local.metadata().map_err(|e| e.to_string())?;
        let total = metadata.len();

        // Recursively create parent dirs if needed
        if let Some(parent) = Path::new(remote_path).parent() {
            if !parent.as_os_str().is_empty() {
                let _ = Self::create_remote_dir_recursive(&sftp, parent);
            }
        }

        let mut remote = crate::ssh::utils::ssh2_retry(|| sftp.create(Path::new(remote_path)))
            .map_err(|e| e.to_string())?;

        let buffer_size = crate::ssh::utils::get_sftp_buffer_size(Some(app));
        let mut buf = vec![0u8; buffer_size];
        let mut transferred = 0u64;
        let mut last_emit = Instant::now();

        // Timeout tracking
        let transfer_start = Instant::now();
        let mut last_progress_time = Instant::now();
        let mut would_block_count = 0u32;

        loop {
            if cancel_flag.load(Ordering::Relaxed) {
                return Err("Cancelled".to_string());
            }

            // Check overall timeout
            if transfer_start.elapsed() > sftp_timeout {
                return Err(format!(
                    "Upload timeout after {}s",
                    sftp_timeout.as_secs()
                ));
            }

            // Check no-progress timeout
            if last_progress_time.elapsed() > no_progress_timeout {
                return Err(format!(
                    "No progress for {}s, connection may be dead",
                    no_progress_timeout.as_secs()
                ));
            }

            let n = local.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }

            let mut pos = 0;
            while pos < n {
                match remote.write(&buf[pos..n]) {
                    Ok(written) => {
                        pos += written;
                        transferred += written as u64;
                        last_progress_time = Instant::now(); // Update progress time
                        would_block_count = 0; // Reset WouldBlock counter on success

                        if last_emit.elapsed().as_millis() > 100 {
                            let _ = app.emit(
                                "transfer-progress",
                                ProgressPayload {
                                    id: transfer_id.to_string(),
                                    transferred,
                                    total,
                                },
                            );
                            last_emit = Instant::now();
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {
                        would_block_count += 1;
                        if would_block_count > 100 {
                            return Err(format!(
                                "Too many WouldBlock errors ({}), connection may be dead",
                                would_block_count
                            ));
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => return Err(e.to_string()),
                }
            }
        }

        let _ = app.emit(
            "transfer-progress",
            ProgressPayload {
                id: transfer_id.to_string(),
                transferred: total,
                total,
            },
        );
        Ok(())
    }

    fn bg_sftp_download(
        pool: SessionSshPool,
        remote_path: &str,
        local_path: &str,
        transfer_id: &str,
        app: &tauri::AppHandle,
        cancel_flag: &Arc<AtomicBool>,
    ) -> Result<(), String> {
        // Delegate to the new transfer pool implementation
        Self::bg_sftp_download_with_pool(pool, remote_path, local_path, transfer_id, app, cancel_flag)
    }

    fn bg_sftp_upload(
        pool: SessionSshPool,
        local_path: &str,
        remote_path: &str,
        transfer_id: &str,
        app: &tauri::AppHandle,
        cancel_flag: &Arc<AtomicBool>,
    ) -> Result<(), String> {
        // Delegate to the new transfer pool implementation
        Self::bg_sftp_upload_with_pool(pool, local_path, remote_path, transfer_id, app, cancel_flag)
    }

    fn create_remote_dir_recursive(sftp: &ssh2::Sftp, path: &Path) -> Result<(), ssh2::Error> {
        if path.as_os_str().is_empty() {
            return Ok(());
        }
        // Try to stat the directory. If it fails, try to create parent then create it.
        if sftp.stat(path).is_err() {
            if let Some(parent) = path.parent() {
                let _ = Self::create_remote_dir_recursive(sftp, parent);
            }
            let _ = sftp.mkdir(path, 0o755);
        }
        Ok(())
    }
}
