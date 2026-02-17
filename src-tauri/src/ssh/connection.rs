use crate::models::Connection as SshConnConfig;
use crate::ssh::{
    ssh2_retry, CONNECTION_RETRY_BASE_DELAY, CONNECTION_RETRY_MAX_ATTEMPTS,
    DEFAULT_CONNECTION_TIMEOUT, JUMP_HOST_TIMEOUT, LOCAL_FORWARD_TIMEOUT,
};
use socket2::{Domain, Protocol, Socket, Type};
use ssh2::Session;
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use std::collections::HashMap;
use tauri::AppHandle;

/// 心跳检测结果缓存，避免频繁检测同一会话
struct HealthCheckCache {
    results: HashMap<usize, (bool, Instant)>,
    cache_duration: Duration,
}

impl HealthCheckCache {
    fn new() -> Self {
        Self {
            results: HashMap::new(),
            cache_duration: Duration::from_secs(5),
        }
    }

    fn get(&self, key: usize) -> Option<bool> {
        if let Some((result, timestamp)) = self.results.get(&key) {
            if timestamp.elapsed() < self.cache_duration {
                return Some(*result);
            }
        }
        None
    }

    fn insert(&mut self, key: usize, result: bool) {
        self.results.insert(key, (result, Instant::now()));
    }

    fn invalidate(&mut self, key: usize) {
        self.results.remove(&key);
    }

    fn cleanup_expired(&mut self) {
        let now = Instant::now();
        self.results.retain(|_, (_, timestamp)| {
            now.duration_since(*timestamp) < self.cache_duration
        });
    }
}

pub struct ForwardingThreadHandle {
    thread_handle: std::thread::JoinHandle<()>,
    shutdown_signal: Arc<AtomicBool>,
}

pub struct ManagedSession {
    pub session: Session,
    pub jump_session: Option<Session>,
    pub forward_listener: Option<TcpListener>,
    pub forwarding_handle: Option<ForwardingThreadHandle>,
}

impl Drop for ManagedSession {
    fn drop(&mut self) {
        // Shutdown forwarding thread if exists
        if let Some(handle) = &mut self.forwarding_handle {
            handle.shutdown_signal.store(true, Ordering::Relaxed);
            // Give the thread a moment to shutdown gracefully
            let handle = std::mem::replace(&mut handle.thread_handle, thread::spawn(|| {})); // Replace with empty thread to take ownership
            let _ = handle.join();
        }

        // Close SSH sessions
        if let Some(ref jump_sess) = self.jump_session {
            let _ = jump_sess.disconnect(None, "", None);
        }
        let _ = self.session.disconnect(None, "", None);

        // Close TCP listener
        if let Some(ref listener) = self.forward_listener {
            let _ = listener.set_nonblocking(true);
            let _ = TcpStream::connect(listener.local_addr().unwrap());
        }
    }
}

impl ForwardingThreadHandle {
    pub fn new(
        thread_handle: std::thread::JoinHandle<()>,
        shutdown_signal: Arc<AtomicBool>,
    ) -> Self {
        Self {
            thread_handle,
            shutdown_signal,
        }
    }
}

impl std::ops::Deref for ManagedSession {
    type Target = Session;
    fn deref(&self) -> &Self::Target {
        &self.session
    }
}

/// 会话级SSH连接池：1个主会话（终端专用）+ N个后台会话（文件操作、命令执行）
#[derive(Clone)]
pub struct SessionSshPool {
    config: SshConnConfig,
    main_session: Arc<Mutex<ManagedSession>>, // 主会话，专用于终端
    ai_session: Arc<Mutex<Option<Arc<Mutex<ManagedSession>>>>>, // Helper dedicated session for AI
    background_sessions: Arc<Mutex<Vec<Arc<Mutex<ManagedSession>>>>>, // 后台会话池
    max_background_sessions: usize,           // 最大后台会话数量
    next_bg_index: Arc<Mutex<usize>>,         // 轮询索引
    created_at: Arc<Mutex<Instant>>,          // 主会话建立时间，用于延迟后台连接
    health_cache: Arc<Mutex<HealthCheckCache>>, // 心跳检测结果缓存
    connection_stagger_count: Arc<Mutex<u32>>, // 连接交错计数器，用于指数退避
}

impl SessionSshPool {
    pub fn new(config: SshConnConfig, max_background_sessions: usize) -> Result<Self, String> {
        // 创建主会话
        let main_session = establish_connection_with_retry(&config)?;

        // Don't create background session immediately to save resources and avoid rate limits
        // It will be created on demand when get_background_session is called

        Ok(Self {
            config,
            main_session: Arc::new(Mutex::new(main_session)),
            ai_session: Arc::new(Mutex::new(None)),
            background_sessions: Arc::new(Mutex::new(Vec::new())),
            max_background_sessions,
            next_bg_index: Arc::new(Mutex::new(0)),
            created_at: Arc::new(Mutex::new(Instant::now())),
            health_cache: Arc::new(Mutex::new(HealthCheckCache::new())),
            connection_stagger_count: Arc::new(Mutex::new(0)),
        })
    }

    /// 获取AI助手专用会话（懒加载）
    pub fn get_ai_session(&self) -> Result<Arc<Mutex<ManagedSession>>, String> {
        let mut session_opt = self.ai_session.lock().map_err(|e| e.to_string())?;

        if let Some(session) = session_opt.as_ref() {
            return Ok(session.clone());
        }

        // Establish new
        let new_session = establish_connection_with_retry(&self.config)?;
        let shared_session = Arc::new(Mutex::new(new_session));
        *session_opt = Some(shared_session.clone());

        Ok(shared_session)
    }

    /// 获取后台会话（智能分配：优先空闲，繁忙则动态补齐）
    pub fn get_background_session(&self) -> Result<Arc<Mutex<ManagedSession>>, String> {
        let mut sessions = self.background_sessions.lock().map_err(|e| e.to_string())?;

        // 1. 尝试寻找当前没有被其它线程锁定的“空闲”会话
        for session in sessions.iter() {
            if let Ok(_guard) = session.try_lock() {
                // 能够立即拿到锁，说明它是空闲的
                return Ok(session.clone());
            }
        }

        // 2. 如果没有空闲会话，且还没达到上限，则创建一个新会话
        if sessions.len() < self.max_background_sessions {
            // Stagger new connections to avoid flooding the server
            if !sessions.is_empty() {
                thread::sleep(Duration::from_millis(100));
            }

            let new_session = establish_connection_with_retry(&self.config)?;
            let session_arc = Arc::new(Mutex::new(new_session));
            sessions.push(session_arc.clone());
            return Ok(session_arc);
        }

        // 3. 所有会话都在忙（被锁定），且已达上限，则退而求其次，轮询阻塞等待一个
        let mut index = self.next_bg_index.lock().map_err(|e| e.to_string())?;
        let session = sessions[*index % sessions.len()].clone();
        *index = (*index + 1) % sessions.len();
        drop(index);

        Ok(session)
    }

    /// 检查并清理断开的连接
    pub fn cleanup_disconnected(&self) {
        // 检查后台会话
        if let Ok(mut sessions) = self.background_sessions.lock() {
            sessions.retain(|session| {
                if let Ok(sess) = session.lock() {
                    // 核心修复：使用 ssh2_retry 处理 WouldBlock 错误
                    // 之前直接调用在非阻塞模式下会失败，导致连接被误杀
                    match ssh2_retry(|| sess.session.keepalive_send()) {
                        Ok(_) => true,   // 发送成功，保留连接
                        Err(_) => false, // 真的断开了，移除连接
                    }
                } else {
                    false
                }
            });

            // 确保至少有一个后台会话
            if sessions.is_empty() {
                if let Ok(new_session) = establish_connection_with_retry(&self.config) {
                    sessions.push(Arc::new(Mutex::new(new_session)));
                }
            }
        }

        // Check AI session
        if let Ok(mut ai_opt) = self.ai_session.lock() {
            let mut remove = false;
            if let Some(session_arc) = ai_opt.as_ref() {
                if let Ok(sess) = session_arc.lock() {
                    match ssh2_retry(|| sess.session.keepalive_send()) {
                        Ok(_) => {}
                        Err(_) => remove = true,
                    }
                } else {
                    remove = true;
                }
            }
            if remove {
                *ai_opt = None;
            }
        }

        // 检查主会话并发送keepalive (仅仅是发送心跳，不执行清理逻辑)
        if let Ok(main_sess) = self.main_session.lock() {
            // 同样使用 retry 机制忽略伪错误
            let _ = ssh2_retry(|| main_sess.session.keepalive_send());
        }
    }

    /// 心跳检测：检查所有连接的健康状态
    pub fn heartbeat_check(&self) -> Result<(), String> {
        let mut need_rebuild_main = false;

        // 检查主会话
        if let Ok(main_sess) = self.main_session.lock() {
            if !self.is_session_alive(&main_sess)? {
                need_rebuild_main = true;
            }
        }

        if need_rebuild_main {
            self.rebuild_main()?;
        }

        // Check AI session (lazy check, just invalidate if dead)
        // We handled it in cleanup_disconnected basically, but `is_session_alive` is stronger check.
        let mut reset_ai = false;
        if let Ok(ai_opt) = self.ai_session.lock() {
            if let Some(session_arc) = ai_opt.as_ref() {
                if let Ok(sess) = session_arc.lock() {
                    if !self.is_session_alive(&sess).unwrap_or(false) {
                        reset_ai = true;
                    }
                }
            }
        }
        if reset_ai {
            if let Ok(mut ai_opt) = self.ai_session.lock() {
                *ai_opt = None;
            }
        }

        // 检查后台会话
        self.cleanup_disconnected();

        Ok(())
    }

    /// 检查单个会话是否存活
    fn is_session_alive(&self, session: &ManagedSession) -> Result<bool, String> {
        // 尝试打开一个通道来检测连接状态
        // 核心修复：必须使用 ssh2_retry，否则非阻塞模式下这里大概率直接返回 Error(WouldBlock)
        match ssh2_retry(|| session.channel_session()) {
            Ok(mut channel) => {
                // 执行一个极轻量级命令 'true' (比 pwd 更轻)
                match ssh2_retry(|| channel.exec("true")) {
                    Ok(_) => {
                        let _ = channel.close();
                        Ok(true)
                    }
                    Err(_) => Ok(false),
                }
            }
            Err(_) => Ok(false),
        }
    }

    /// 关闭所有SSH连接
    pub fn close_all(&self) {
        // 关闭主会话
        if let Ok(mut main_sess) = self.main_session.lock() {
            // Close forwarding thread first
            if let Some(mut handle) = main_sess.forwarding_handle.take() {
                handle.shutdown_signal.store(true, Ordering::Relaxed);
                let thread_handle =
                    std::mem::replace(&mut handle.thread_handle, thread::spawn(|| {})); // Replace with empty thread
                let _ = thread_handle.join();
            }
            // Close sessions
            if let Some(ref jump_sess) = main_sess.jump_session {
                let _ = jump_sess.disconnect(None, "", None);
            }
            let _ = main_sess.session.disconnect(None, "", None);
            // Close listener
            if let Some(ref listener) = main_sess.forward_listener {
                let _ = listener.set_nonblocking(true);
                let _ = TcpStream::connect(listener.local_addr().unwrap());
            }
        }

        // Close AI session
        if let Ok(mut ai_opt) = self.ai_session.lock() {
            if let Some(session_arc) = ai_opt.take() {
                if let Ok(mut sess) = session_arc.lock() {
                    // Close forwarding thread first
                    if let Some(mut handle) = sess.forwarding_handle.take() {
                        handle.shutdown_signal.store(true, Ordering::Relaxed);
                        let thread_handle =
                            std::mem::replace(&mut handle.thread_handle, thread::spawn(|| {}));
                        let _ = thread_handle.join();
                    }
                    // Close sessions
                    if let Some(ref jump_sess) = sess.jump_session {
                        let _ = jump_sess.disconnect(None, "", None);
                    }
                    let _ = sess.session.disconnect(None, "", None);
                    // Close listener
                    if let Some(ref listener) = sess.forward_listener {
                        let _ = listener.set_nonblocking(true);
                        let _ = TcpStream::connect(listener.local_addr().unwrap());
                    }
                }
            }
        }

        // 关闭所有后台会话
        if let Ok(mut sessions) = self.background_sessions.lock() {
            for session in sessions.drain(..) {
                if let Ok(mut sess) = session.lock() {
                    // Close forwarding thread first
                    if let Some(mut handle) = sess.forwarding_handle.take() {
                        handle.shutdown_signal.store(true, Ordering::Relaxed);
                        let thread_handle =
                            std::mem::replace(&mut handle.thread_handle, thread::spawn(|| {})); // Replace with empty thread
                        let _ = thread_handle.join();
                    }
                    // Close sessions
                    if let Some(ref jump_sess) = sess.jump_session {
                        let _ = jump_sess.disconnect(None, "", None);
                    }
                    let _ = sess.session.disconnect(None, "", None);
                    // Close listener
                    if let Some(ref listener) = sess.forward_listener {
                        let _ = listener.set_nonblocking(true);
                        let _ = TcpStream::connect(listener.local_addr().unwrap());
                    }
                }
            }
        }
    }

    fn rebuild_main(&self) -> Result<(), String> {
        // 在锁之外建立连接，避免阻塞其他持有锁的操作
        let new_session = establish_connection_with_retry(&self.config)?;

        {
            let mut main_sess = self.main_session.lock().map_err(|e| e.to_string())?;
            *main_sess = new_session;

            // Reset creation time
            if let Ok(mut t) = self.created_at.lock() {
                *t = Instant::now();
            }
        }
        Ok(())
    }

    /// 重建所有连接
    pub fn rebuild_all(&self) -> Result<(), String> {
        // 重建主会话
        self.rebuild_main()?;

        // 清空后台会话，它们会按需懒加载
        {
            let mut sessions = self.background_sessions.lock().map_err(|e| e.to_string())?;
            sessions.clear();
        }

        // Reset AI session
        {
            let mut ai_opt = self.ai_session.lock().map_err(|e| e.to_string())?;
            *ai_opt = None;
        }

        // remove eager warmup
        /*
        // 预热一个后台会话
        if let Ok(initial_bg_session) = establish_connection_with_retry(&self.config) {
            let mut sessions = self.background_sessions.lock().map_err(|e| e.to_string())?;
            sessions.push(Arc::new(Mutex::new(initial_bg_session)));
        }
        */

        Ok(())
    }
}

pub fn establish_connection_with_retry(config: &SshConnConfig) -> Result<ManagedSession, String> {
    for attempt in 1..=CONNECTION_RETRY_MAX_ATTEMPTS {
        match establish_connection_internal(config) {
            Ok(session) => return Ok(session),
            Err(e) => {
                if attempt == CONNECTION_RETRY_MAX_ATTEMPTS {
                    return Err(format!(
                        "Failed to establish connection after {} attempts: {}",
                        CONNECTION_RETRY_MAX_ATTEMPTS, e
                    ));
                }

                let delay = CONNECTION_RETRY_BASE_DELAY * 2_u32.pow(attempt - 1);
                thread::sleep(delay);
            }
        }
    }
    unreachable!()
}

fn establish_connection_internal(config: &SshConnConfig) -> Result<ManagedSession, String> {
    let mut sess = Session::new().map_err(|e| e.to_string())?;
    let mut jump_session_holder = None;
    let mut listener_holder = None;
    let mut forwarding_handle = None;

    if let Some(jump_host) = &config.jump_host {
        if !jump_host.trim().is_empty() {
            // Jump Host Logic
            let jump_port = config.jump_port.unwrap_or(22);
            let jump_addr = format!("{}:{}", jump_host, jump_port);

            // Connect to jump host with longer timeout
            let jump_tcp = connect_with_timeout(&jump_addr, JUMP_HOST_TIMEOUT)
                .map_err(|e| format!("Jump host connection failed: {}", e))?;

            let mut jump_sess = Session::new().map_err(|e| e.to_string())?;
            jump_sess.set_tcp_stream(jump_tcp);
            jump_sess
                .handshake()
                .map_err(|e| format!("Jump handshake failed: {}", e))?;

            jump_sess
                .userauth_password(
                    config.jump_username.as_deref().unwrap_or(""),
                    config.jump_password.as_deref().unwrap_or(""),
                )
                .map_err(|e| format!("Jump auth failed: {}", e))?;

            // 核心修复：跳板机也需要 Keepalive！
            jump_sess.set_keepalive(true, 15);

            // Enable non-blocking mode for the jump session
            jump_sess.set_blocking(false);

            // Local Port Forwarding Pattern
            let listener = TcpListener::bind("127.0.0.1:0")
                .map_err(|e| format!("Failed to bind local port: {}", e))?;

            listener
                .set_nonblocking(true)
                .map_err(|e| format!("Failed to set listener non-blocking: {}", e))?;

            let local_port = listener
                .local_addr()
                .map_err(|e| format!("Failed to get local port: {}", e))?
                .port();

            // Create shutdown signal for forwarding thread
            let shutdown_signal = Arc::new(AtomicBool::new(false));

            // 2. Start port forwarding thread
            let jump_sess_clone = jump_sess.clone();
            let target_host = config.host.clone();
            let target_port = config.port;
            let listener_clone = listener
                .try_clone()
                .map_err(|e| format!("Failed to clone listener: {}", e))?;
            let shutdown_signal_clone = shutdown_signal.clone();

            let thread_handle = thread::spawn(move || {
                // 优化：只接受一个连接。因为这是一对一的映射。
                let start = std::time::Instant::now();
                let mut accepted = false;

                while !shutdown_signal_clone.load(Ordering::Relaxed) && !accepted {
                    if start.elapsed().as_secs() > 10 {
                        break;
                    }

                    match listener_clone.accept() {
                        Ok((mut local_stream, _)) => {
                            accepted = true;
                            let jump_sess_inner = jump_sess_clone.clone();
                            let host = target_host.clone();
                            let port = target_port;
                            let shutdown_inner = shutdown_signal_clone.clone();

                            // Open direct-tcpip channel
                            let mut channel = loop {
                                match jump_sess_inner.channel_direct_tcpip(&host, port, None) {
                                    Ok(c) => break c,
                                    Err(e) if e.code() == ssh2::ErrorCode::Session(-37) => {
                                        // EAGAIN
                                        if shutdown_inner.load(Ordering::Relaxed) {
                                            return;
                                        }
                                        thread::sleep(Duration::from_millis(10));
                                        continue;
                                    }
                                    Err(e) => {
                                        eprintln!("Failed to establish SSH tunnel: {}", e);
                                        return;
                                    }
                                }
                            };

                            if let Err(_) = local_stream.set_nonblocking(true) {
                                return;
                            }

                            let mut buf = [0u8; 32768]; // 32KB buffer

                            while !shutdown_inner.load(Ordering::Relaxed) {
                                let mut has_data = false;

                                // Read from Local -> Write to Remote
                                match local_stream.read(&mut buf) {
                                    Ok(0) => break, // EOF
                                    Ok(n) => {
                                        has_data = true;
                                        let mut pos = 0;
                                        while pos < n {
                                            match channel.write(&buf[pos..n]) {
                                                Ok(written) => pos += written,
                                                Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                                                    thread::sleep(Duration::from_millis(1));
                                                }
                                                Err(_) => return, // Pipe broken
                                            }
                                        }
                                    }
                                    Err(ref e) if e.kind() == ErrorKind::WouldBlock => {}
                                    Err(_) => break,
                                }

                                // Read from Remote -> Write to Local
                                match channel.read(&mut buf) {
                                    Ok(0) => break, // EOF
                                    Ok(n) => {
                                        has_data = true;
                                        let mut pos = 0;
                                        while pos < n {
                                            match local_stream.write(&buf[pos..n]) {
                                                Ok(written) => pos += written,
                                                Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                                                    thread::sleep(Duration::from_millis(1));
                                                }
                                                Err(_) => return,
                                            }
                                        }
                                    }
                                    Err(ref e) if e.kind() == ErrorKind::WouldBlock => {}
                                    Err(_) => break,
                                }

                                if !has_data {
                                    thread::sleep(Duration::from_millis(2));
                                }
                            }
                        }
                        Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(100));
                        }
                        Err(_) => {
                            break;
                        }
                    }
                }
            });

            // 3. Connect to the local forwarded port
            let connect_addr = format!("127.0.0.1:{}", local_port);
            let tcp_stream =
                connect_with_timeout(&connect_addr, LOCAL_FORWARD_TIMEOUT).map_err(|e| {
                    format!(
                        "Failed to connect to local forwarded port {}: {}",
                        local_port, e
                    )
                })?;

            sess.set_tcp_stream(tcp_stream);

            // Store handles
            forwarding_handle = Some(ForwardingThreadHandle::new(thread_handle, shutdown_signal));
            jump_session_holder = Some(jump_sess);
            listener_holder = Some(listener);
        } else {
            // Direct connection
            let addr_str = format!("{}:{}", config.host, config.port);
            let tcp = connect_with_timeout(&addr_str, DEFAULT_CONNECTION_TIMEOUT)
                .map_err(|e| format!("Connection failed: {}", e))?;
            sess.set_tcp_stream(tcp);
        }
    } else {
        // Direct connection
        let addr_str = format!("{}:{}", config.host, config.port);
        let tcp = connect_with_timeout(&addr_str, DEFAULT_CONNECTION_TIMEOUT)
            .map_err(|e| format!("Connection failed: {}", e))?;
        sess.set_tcp_stream(tcp);
    };

    sess.handshake()
        .map_err(|e| format!("Handshake failed: {}", e))?;

    // Implement TOFU (Trust On First Use) Host Key Verification
    verify_host_key(&sess, &config.host, config.port)?;

    if config.auth_type.as_deref() == Some("key") {
        if let Some(key_content) = &config.key_content {
            // Write key to a temporary file because ssh2 requires a file path for userauth_pubkey_file
            // We use std::env::temp_dir() and a random filename
            use ssh_key::PrivateKey;

            // Write private key to temp file
            let uuid = uuid::Uuid::new_v4();
            let temp_dir = std::env::temp_dir();
            let key_path = temp_dir.join(format!("ssh_key_{}", uuid));
            let pub_key_path = temp_dir.join(format!("ssh_key_{}.pub", uuid));

            std::fs::write(&key_path, key_content).map_err(|e| {
                format!(
                    "Failed to write temporary key file (check permissions/disk space): {}",
                    e
                )
            })?;

            // Check for PPK format issues before parsing
            if key_content.contains("PuTTY-User-Key-File") {
                let _ = std::fs::remove_file(&key_path);
                return Err("Putty (PPK) format is not supported. Please convert your private key to OpenSSH format (PEM) using PuTTYgen or ssh-keygen.".to_string());
            }

            // Derive and write public key
            let public_key_content = PrivateKey::from_openssh(key_content)
                .and_then(|pk| pk.public_key().to_openssh())
                .map_err(|e| {
                    let _ = std::fs::remove_file(&key_path);
                    format!(
                        "Failed to parse private key. Ensure it is in OpenSSH format. Details: {}",
                        e
                    )
                })?;

            std::fs::write(&pub_key_path, &public_key_content).map_err(|e| {
                let _ = std::fs::remove_file(&key_path);
                format!("Failed to write temporary public key file: {}", e)
            })?;

            let passphrase = config.key_passphrase.as_deref();

            // Try to authenticate with the explicit public key path
            let auth_res = sess.userauth_pubkey_file(
                &config.username,
                Some(&pub_key_path),
                &key_path,
                passphrase,
            );

            // Wipe and delete the temp files immediately
            let _ = std::fs::remove_file(&key_path);
            let _ = std::fs::remove_file(&pub_key_path);

            auth_res.map_err(|e| {
                let hint = if passphrase.is_some() {
                    "Verify your passphrase is correct."
                } else {
                    "Ensure the public key is added to the server's ~/.ssh/authorized_keys."
                };
                format!("Key authentication failed: {}. Hint: {}", e, hint)
            })?;
        } else {
            return Err("Auth type is 'key' but no key content provided".to_string());
        }
    } else {
        // Default to password
        sess.userauth_password(&config.username, config.password.as_deref().unwrap_or(""))
            .map_err(|e| format!("Password authentication failed: {}", e))?;
    }

    // Enable keepalive for the main session
    sess.set_keepalive(true, 15);

    // Set non-blocking mode for concurrency
    sess.set_blocking(false);

    Ok(ManagedSession {
        session: sess,
        jump_session: jump_session_holder,
        forward_listener: listener_holder,
        forwarding_handle,
    })
}

fn verify_host_key(session: &Session, host: &str, port: u16) -> Result<(), String> {
    use ssh2::{CheckResult, HashType, KnownHostFileKind};

    let mut known_hosts = session
        .known_hosts()
        .map_err(|e| format!("Failed to init known hosts: {}", e))?;

    // Try to find the known_hosts file
    let ssh_dir = dirs::home_dir()
        .ok_or("Could not find home directory")?
        .join(".ssh");

    if !ssh_dir.exists() {
        std::fs::create_dir_all(&ssh_dir)
            .map_err(|e| format!("Failed to create .ssh directory: {}", e))?;
    }

    let known_hosts_path = ssh_dir.join("known_hosts");
    if !known_hosts_path.exists() {
        std::fs::File::create(&known_hosts_path)
            .map_err(|e| format!("Failed to create known_hosts file: {}", e))?;
    }

    // Load existing known_hosts
    known_hosts
        .read_file(&known_hosts_path, KnownHostFileKind::OpenSSH)
        .map_err(|e| format!("Failed to read known_hosts file: {}", e))?;

    let (key, key_type) = session.host_key().ok_or("Failed to get remote host key")?;

    match known_hosts.check_port(host, port, key) {
        CheckResult::Match => Ok(()),
        CheckResult::NotFound => {
            // TOFU: Trust On First Use - Auto Accept
            println!(
                "Host key not found for {}:{}. Auto-accepting...",
                host, port
            );

            // Add to in-memory known hosts
            known_hosts
                .add(host, key, "", key_type.into())
                .map_err(|e| format!("Failed to add host key: {}", e))?;

            // Write back to file
            known_hosts
                .write_file(&known_hosts_path, KnownHostFileKind::OpenSSH)
                .map_err(|e| format!("Failed to write known_hosts file: {}", e))?;

            Ok(())
        }
        CheckResult::Mismatch => {
            // Strictly reject mismatch
            // Get formatted fingerprint for error message
            let fingerprint = session
                .host_key_hash(HashType::Sha1)
                .map(|h| {
                    h.iter()
                        .map(|b| format!("{:02x}", b))
                        .collect::<Vec<String>>()
                        .join(":")
                })
                .unwrap_or_else(|| "unknown".to_string());

            Err(format!(
                "Host key verification failed! The remote host identification has changed. \
                This could mean that someone is eavesdropping on you (Man-in-the-Middle attack), \
                or that the host key has legitimately changed. \
                Host: {}:{} \
                Fingerprint: {} \
                Please verify the host key.",
                host, port, fingerprint
            ))
        }
        CheckResult::Failure => Err("Host key verification failed with internal error".to_string()),
    }
}

// 跨平台兼容的带超时和Keepalive的Socket连接函数
fn connect_with_timeout(addr_str: &str, timeout: Duration) -> Result<TcpStream, String> {
    let addrs = addr_str
        .to_socket_addrs()
        .map_err(|e| format!("Invalid address '{}': {}", addr_str, e))?
        .collect::<Vec<_>>();

    if addrs.is_empty() {
        return Err("No valid addresses found".to_string());
    }

    let addr = addrs[0];

    let domain = match addr {
        SocketAddr::V4(_) => Domain::IPV4,
        SocketAddr::V6(_) => Domain::IPV6,
    };

    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))
        .map_err(|e| format!("Failed to create socket: {}", e))?;

    // 设置 TCP_NODELAY
    if let Err(e) = socket.set_nodelay(true) {
        eprintln!("Warning: Failed to set TCP_NODELAY: {}", e);
    }

    // 设置 TCP Keepalive (底层 TCP 协议保活)
    let keepalive_conf = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(60))
        .with_interval(Duration::from_secs(10));

    #[cfg(not(target_os = "windows"))]
    let keepalive_conf = keepalive_conf.with_retries(3);

    if let Err(e) = socket.set_tcp_keepalive(&keepalive_conf) {
        // 如果高级设置失败，尝试基本的启用
        let _ = socket.set_keepalive(true);
        eprintln!("Warning: Failed to set detailed TCP Keepalive: {}", e);
    }

    // 连接
    if let Err(e) = socket.connect_timeout(&addr.into(), timeout) {
        return Err(format!("Failed to connect to '{}': {}", addr_str, e));
    }

    Ok(socket.into())
}

// Helper to install public key
// Helper to install public key
pub fn install_public_key(session: &ssh2::Session, public_key: &str) -> Result<(), String> {
    // 1. Init SFTP
    let sftp = ssh2_retry(|| session.sftp()).map_err(|e| format!("SFTP init failed: {}", e))?;

    // 2. Ensure .ssh directory exists
    // We ignore error because it might simply exist
    // 0o700 is rwx------
    let _ = ssh2_retry(|| sftp.mkdir(std::path::Path::new(".ssh"), 0o700));

    // 3. Append to authorized_keys
    use ssh2::OpenFlags;

    // We strictly use forward slashes for remote paths to ensure compatibility with Linux servers
    let auth_keys_path = std::path::Path::new(".ssh/authorized_keys");

    let mut file = ssh2_retry(|| {
        sftp.open_mode(
            auth_keys_path,
            OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::APPEND,
            0o600,
            ssh2::OpenType::File,
        )
    })
    .map_err(|e| format!("Failed to open .ssh/authorized_keys: {}", e))?;

    // Append newline to ensure separation
    let content = format!("\n{}\n", public_key.trim());

    // Handle non-blocking IO writing
    let bytes = content.as_bytes();
    let mut pos = 0;
    while pos < bytes.len() {
        match file.write(&bytes[pos..]) {
            Ok(0) => return Err("Write returned 0 bytes".to_string()),
            Ok(n) => pos += n,
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(e) => return Err(format!("Failed to write key: {}", e)),
        }
    }

    Ok(())
}

#[tauri::command]
pub async fn install_ssh_key(
    app: AppHandle,
    connection_id: i64,
    key_id: i64,
) -> Result<(), String> {
    // 1. Get Connection and Key
    let connections = crate::db::get_connections(app.clone())?;
    let conn = connections
        .into_iter()
        .find(|c| c.id == Some(connection_id))
        .ok_or("Connection not found")?;

    let key = crate::db::get_ssh_key_by_id(&app, key_id)?.ok_or("SSH Key not found")?;

    // 2. Connect with Password (must have password)
    // If connection has no password, prompt? Backend command assumes password is in `conn`.
    if conn.password.is_none() {
        return Err("Connection must have a password to install SSH key".to_string());
    }

    // Force password auth for installation session
    let mut install_config = conn.clone();
    install_config.auth_type = Some("password".to_string());

    // Establish temporary connection
    let session_pool = tokio::task::spawn_blocking(move || {
        crate::ssh::connection::establish_connection_with_retry(&install_config)
    })
    .await
    .map_err(|e| e.to_string())??;

    // 3. Derive Public Key
    // We stored private key content. We need to parse it and get public key.
    // We can use ssh_key crate again.
    let public_key = {
        use ssh_key::PrivateKey;
        let priv_key = PrivateKey::from_openssh(&key.content)
            .map_err(|e| format!("Invalid private key in DB: {}", e))?;

        priv_key
            .public_key()
            .to_openssh()
            .map_err(|e| format!("Failed to derive public key: {}", e))?
    };

    // 4. Install
    // session_pool.session is the ssh2::Session
    // We need to run blocking operations on it.
    let sess = session_pool.session.clone();
    tokio::task::spawn_blocking(move || install_public_key(&sess, &public_key))
        .await
        .map_err(|e| e.to_string())??;

    // 5. Cleanup session (drop pool)
    // 5. Cleanup session (drop pool handled by Drop trait)
    // session_pool.close_all();

    // 6. Update Connection to use Key
    // We update the auth_type and ssh_key_id
    let mut updated_conn = conn;
    updated_conn.auth_type = Some("key".to_string());
    updated_conn.ssh_key_id = Some(key_id);

    crate::db::update_connection(app, updated_conn)?;

    Ok(())
}
