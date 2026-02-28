// use super::connection::SessionSshPool; // Keep for now if referenced elsewhere, but we will remove usage
use super::manager::{SshCommand, SshManager};
use super::terminal::start_shell_thread;
use crate::models::{Connection as SshConnConfig, ConnectionTimeoutSettings};
use crate::ssh::{execute_ssh_operation, ShellMsg};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, State};
use uuid::Uuid;

#[derive(Clone)]
pub enum ClientType {
    Ssh(Sender<SshCommand>), // Changed from Arc<SessionSshPool>
    Wsl(String),             // Distro name
}

#[derive(Clone)]
pub struct SshClient {
    pub client_type: ClientType,            // SSH Manager Channel or WSL
    pub shell_tx: Option<Sender<ShellMsg>>, // Terminal message channel (to Manager or WSL)
    pub owner_cache: Arc<Mutex<HashMap<u32, String>>>, // UID cache (To be deprecated as Manager handles it internally, but keep for compatibility if needed)
    pub shutdown_signal: Arc<AtomicBool>,              // Shared signal
    pub os_info: Option<String>,                       // Remote OS information
}

use crate::models::Transfer;

pub struct TransferState {
    pub data: Mutex<Transfer>,
    pub cancel_flag: Arc<AtomicBool>,
}

pub struct AppState {
    pub clients: Mutex<HashMap<String, SshClient>>,
    pub transfers: Mutex<HashMap<String, Arc<TransferState>>>, // ID -> TransferState
    pub command_cancellations: Mutex<HashMap<String, Arc<AtomicBool>>>, // Command ID -> CancelFlag
}

impl AppState {
    pub fn new() -> Self {
        Self {
            clients: Mutex::new(HashMap::new()),
            transfers: Mutex::new(HashMap::new()),
            command_cancellations: Mutex::new(HashMap::new()),
        }
    }
}

#[tauri::command]
pub async fn test_connection(app: AppHandle, config: SshConnConfig) -> Result<String, String> {
    let mut populated_config = config.clone();

    if populated_config.auth_type.as_deref() == Some("key") {
        if let Some(key_id) = populated_config.ssh_key_id {
            match crate::db::get_ssh_key_by_id(&app, key_id) {
                Ok(Some(key)) => {
                    populated_config.key_content = Some(key.content);
                    populated_config.key_passphrase = key.passphrase;
                }
                Ok(None) => {
                    return Err(format!("SSH Key with ID {} not found", key_id));
                }
                Err(e) => {
                    return Err(format!("Failed to fetch SSH Key: {}", e));
                }
            }
        } else {
            // If key auth is selected but no ID provided, fail early
            return Err("SSH Key ID is missing needed for key authentication".to_string());
        }
    }

    execute_ssh_operation(move || {
        let session = super::connection::establish_connection_with_retry(&populated_config, None, None)?;
        // Disconnect immediately as we only wanted to test credentials/reachability
        let _ = session.session.disconnect(None, "Connection Test", None);
        Ok("Connection successful".to_string())
    })
    .await
}

#[tauri::command]
pub async fn connect(
    app: AppHandle,
    state: State<'_, AppState>,
    config: SshConnConfig,
    id: Option<String>,
) -> Result<String, String> {
    // Use OS type from connection config with fallback to Linux for backward compatibility
    let os_info = config
        .os_type
        .clone()
        .unwrap_or_else(|| "Linux".to_string());
    println!("Using OS type from config: {}", os_info);
    let id = id.unwrap_or_else(|| Uuid::new_v4().to_string());

    // Define shutdown_signal early
    let shutdown_signal = Arc::new(AtomicBool::new(false));

    let client_type = if config.host.starts_with("wsl://") {
        let distro = config.host.trim_start_matches("wsl://").to_string();
        ClientType::Wsl(distro)
    } else {
        // Create SSH connection in a blocking task

        // Populate key content if needed
        let mut populated_config = config.clone();
        if populated_config.auth_type.as_deref() == Some("key") {
            if let Some(key_id) = populated_config.ssh_key_id {
                match crate::db::get_ssh_key_by_id(&app, key_id) {
                    Ok(Some(key)) => {
                        populated_config.key_content = Some(key.content);
                        populated_config.key_passphrase = key.passphrase;
                    }
                    Ok(None) => {
                        return Err(format!("SSH Key with ID {} not found in database", key_id));
                    }
                    Err(e) => {
                        println!("Error fetching SSH Key: {}", e);
                        return Err(format!("Failed to fetch SSH Key: {}", e));
                    }
                }
            }
        }

        let config_clone = populated_config.clone();
        let shutdown_signal_clone = shutdown_signal.clone();

        // Get timeout settings from app settings
        let app_settings = crate::db::get_settings(app.clone()).ok();
        let timeout_settings: Option<ConnectionTimeoutSettings> =
            app_settings.as_ref().map(|s| s.connection_timeout.clone());
        let reconnect_settings: Option<crate::models::ReconnectSettings> =
            app_settings.as_ref().map(|s| s.reconnect.clone());
        // 从设置中获取最大后台会话数，默认为 6（比原来的 3 更大，减少阻塞）
        let max_background_sessions: usize = app_settings
            .as_ref()
            .map(|s| s.ssh_pool.max_background_sessions as usize)
            .unwrap_or(6);

        // Establish connection and spawn manager thread
        let sender = tokio::task::spawn_blocking(move || {
            let session = super::connection::establish_connection_with_retry(&config_clone, timeout_settings.as_ref(), reconnect_settings.as_ref())?;
            let pool = super::connection::SessionSshPool::with_reconnect_settings(config_clone.clone(), max_background_sessions, timeout_settings, reconnect_settings)
                .map_err(|e| e.to_string())?;

            let (tx, rx) = std::sync::mpsc::channel();
            let mut manager = SshManager::new(session, pool, rx, shutdown_signal_clone);

            std::thread::spawn(move || {
                manager.run();
            });

            Ok::<Sender<SshCommand>, String>(tx)
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))??;

        ClientType::Ssh(sender)
    };

    // Create mutable client reference for terminal initialization
    let mut client = SshClient {
        client_type,
        shell_tx: None, // Will be set by start_shell_thread
        owner_cache: Arc::new(Mutex::new(HashMap::new())),
        shutdown_signal,
        os_info: Some(os_info),
    };

    // Start shell thread (or init shell via manager)
    // Note: start_shell_thread for SSH now just returns a sender that wraps SshCommand::Shell*
    let shell_tx = start_shell_thread(app.clone(), &mut client, id.clone())
        .map_err(|e| format!("Failed to start shell thread: {}", e))?;

    // Update client with the shell transmitter
    client.shell_tx = Some(shell_tx);

    state
        .clients
        .lock()
        .map_err(|e| e.to_string())?
        .insert(id.clone(), client);

    Ok(id)
}

#[tauri::command]
pub async fn disconnect(state: State<'_, AppState>, id: String) -> Result<(), String> {
    // Get client to disconnect
    let client = {
        let mut clients = state.clients.lock().map_err(|e| e.to_string())?;
        clients.remove(&id)
    };

    if let Some(client) = client {
        // 1. 发送停止信号
        client.shutdown_signal.store(true, Ordering::Relaxed);

        // 2. 关闭 Shell / Manager
        if let Some(tx) = client.shell_tx {
            let _ = tx.send(ShellMsg::Exit);
        }

        // 3. 关闭连接
        match &client.client_type {
            ClientType::Ssh(sender) => {
                let _ = sender.send(SshCommand::Shutdown);
            }
            ClientType::Wsl(_) => {}
        }
    }

    Ok(())
}

#[tauri::command]
pub async fn cleanup_and_reconnect(state: State<'_, AppState>, id: String) -> Result<(), String> {
    // Reconnect logic is harder with single connection actor model
    // Usually implies disconnect and connect from UI.
    // Or we need to ask Manager to Reconnect?
    // Given the architecture change, "cleanup_and_reconnect" might need to fully re-establish the manager.
    // For now, let's implement it as "disconnect" (if we could trigger UI to reconnect).
    // Or better: Use existing config to spawn new manager and replace in state.

    // BUT we don't have the config implementation easily accessible here in this function signature without DB lookup or caching config in SshClient.
    // `SshClient` doesn't store config.
    // Let's keep it as TODO or simple error for now, or just return Ok and rely on UI to handle disconnection?
    // The original implementation fetched connection from DB but we don't have ConnectionID here easily unless we parse ID?
    // Actually `cleanup_and_reconnect` was used for broken pipe.

    // For V1 Actor Model, if connection dies, likely the Manager thread dies.
    // We should probably just let the user "Connect" again.

    // Let's just remove the client so UI shows disconnected.
    let _ = disconnect(state, id).await;

    Ok(())
}

#[tauri::command]
pub async fn cancel_transfer(
    state: State<'_, AppState>,
    transfer_id: String,
) -> Result<(), String> {
    if let Some(transfer_state) = state
        .transfers
        .lock()
        .map_err(|e| e.to_string())?
        .get(&transfer_id)
    {
        transfer_state.cancel_flag.store(true, Ordering::Relaxed);

        // Update status immediately if possible
        let mut data = transfer_state.data.lock().map_err(|e| e.to_string())?;
        if data.status == "running" || data.status == "pending" {
            data.status = "cancelled".to_string();
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn cancel_command_execution(
    state: State<'_, AppState>,
    command_id: String,
) -> Result<(), String> {
    let cancellations = state
        .command_cancellations
        .lock()
        .map_err(|e| e.to_string())?;
    if let Some(cancel_flag) = cancellations.get(&command_id) {
        cancel_flag.store(true, Ordering::Relaxed);
    }
    Ok(())
}

#[tauri::command]
pub async fn get_os_info(state: State<'_, AppState>, id: String) -> Result<String, String> {
    let clients = state.clients.lock().map_err(|e| e.to_string())?;
    let client = clients.get(&id).ok_or("Session not found")?;
    Ok(client.os_info.clone().unwrap_or("Unknown".to_string()))
}
