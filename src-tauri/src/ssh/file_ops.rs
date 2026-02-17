use super::client::{AppState, ClientType};
use super::manager::SshCommand;
use crate::models::FileEntry;
use crate::models::Transfer;
use crate::ssh::client::TransferState;
use crate::ssh::execute_ssh_operation;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use tauri::{AppHandle, Emitter, State};

use crate::ssh::ProgressPayload;

#[derive(Clone, serde::Serialize)]
struct ErrorPayload {
    id: String,
    error: String,
}

fn to_wsl_path(distro: &str, path: &str) -> PathBuf {
    let clean_path = path.replace("/", "\\");
    let trimmed = clean_path.trim_start_matches('\\');
    PathBuf::from(format!("\\\\wsl$\\{}\\{}", distro, trimmed))
}

#[tauri::command]
pub async fn read_remote_file(
    state: State<'_, AppState>,
    id: String,
    path: String,
    max_bytes: Option<u64>,
) -> Result<String, String> {
    let client = {
        let clients = state.clients.lock().map_err(|e| e.to_string())?;
        clients.get(&id).ok_or("Session not found")?.clone()
    };

    match &client.client_type {
        ClientType::Ssh(sender) => {
            let sender = sender.clone();
            execute_ssh_operation(move || {
                let (tx, rx) = std::sync::mpsc::channel();
                sender
                    .send(SshCommand::SftpRead {
                        path,
                        max_len: max_bytes.map(|n| n as usize),
                        listener: tx,
                    })
                    .map_err(|e| format!("Failed to send command: {}", e))?;

                let data = rx
                    .recv()
                    .map_err(|_| "Failed to receive response from SSH Manager".to_string())??;

                String::from_utf8(data).map_err(|e| format!("UTF-8 Error: {}", e))
            })
            .await
        }
        ClientType::Wsl(distro) => {
            let distro = distro.clone();
            tokio::task::spawn_blocking(move || {
                let wsl_path = to_wsl_path(&distro, &path);
                let mut file = std::fs::File::open(wsl_path).map_err(|e| e.to_string())?;
                let mut buf = Vec::new();
                if let Some(max) = max_bytes {
                    let mut handle = file.take(max);
                    handle.read_to_end(&mut buf).map_err(|e| e.to_string())?;
                } else {
                    file.read_to_end(&mut buf).map_err(|e| e.to_string())?;
                }
                String::from_utf8(buf).map_err(|e| e.to_string())
            })
            .await
            .map_err(|e| format!("Task join error: {}", e))?
        }
    }
}

#[tauri::command]
pub async fn write_remote_file(
    state: State<'_, AppState>,
    id: String,
    path: String,
    content: String,
    mode: Option<String>,
) -> Result<(), String> {
    let client = {
        let clients = state.clients.lock().map_err(|e| e.to_string())?;
        clients.get(&id).ok_or("Session not found")?.clone()
    };

    match &client.client_type {
        ClientType::Ssh(sender) => {
            let sender = sender.clone();
            execute_ssh_operation(move || {
                let (tx, rx) = std::sync::mpsc::channel();

                // Convert content to bytes
                let content_bytes = content.into_bytes();

                sender
                    .send(SshCommand::SftpWrite {
                        path,
                        content: content_bytes,
                        mode,
                        listener: tx,
                    })
                    .map_err(|e| format!("Failed to send command: {}", e))?;

                rx.recv()
                    .map_err(|_| "Failed to receive response from SSH Manager".to_string())?
            })
            .await
        }
        ClientType::Wsl(distro) => {
            let distro = distro.clone();
            tokio::task::spawn_blocking(move || {
                let wsl_path = to_wsl_path(&distro, &path);
                let open_mode = mode.unwrap_or_else(|| "overwrite".to_string());

                let mut options = std::fs::OpenOptions::new();
                options.write(true).create(true);
                if open_mode == "append" {
                    options.append(true);
                } else {
                    options.truncate(true);
                }

                let mut file = options.open(wsl_path).map_err(|e| e.to_string())?;
                file.write_all(content.as_bytes())
                    .map_err(|e| e.to_string())?;
                Ok(())
            })
            .await
            .map_err(|e| format!("Task join error: {}", e))?
        }
    }
}

#[tauri::command]
pub async fn list_files(
    state: State<'_, AppState>,
    id: String,
    path: String,
) -> Result<Vec<FileEntry>, String> {
    let client = {
        let clients = state.clients.lock().map_err(|e| e.to_string())?;
        clients.get(&id).ok_or("Session not found")?.clone()
    };

    match &client.client_type {
        ClientType::Ssh(sender) => {
            let sender = sender.clone();
            execute_ssh_operation(move || {
                let (tx, rx) = std::sync::mpsc::channel();
                sender
                    .send(SshCommand::SftpLs { path, listener: tx })
                    .map_err(|e| format!("Failed to send command: {}", e))?;

                rx.recv()
                    .map_err(|_| "Failed to receive response from SSH Manager".to_string())?
            })
            .await
        }
        ClientType::Wsl(distro) => {
            let distro = distro.clone();
            tokio::task::spawn_blocking(move || {
                let wsl_path = to_wsl_path(&distro, &path);
                let entries = std::fs::read_dir(wsl_path).map_err(|e| e.to_string())?;
                let mut file_entries = Vec::new();
                for entry in entries {
                    let entry = entry.map_err(|e| e.to_string())?;
                    let meta = entry.metadata().map_err(|e| e.to_string())?;
                    let name = entry.file_name().to_string_lossy().to_string();

                    file_entries.push(FileEntry {
                        name,
                        is_dir: meta.is_dir(),
                        size: meta.len(),
                        mtime: meta
                            .modified()
                            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
                            .duration_since(std::time::SystemTime::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs() as i64,
                        permissions: 0o755,
                        uid: 0,
                        owner: "root".to_string(),
                    });
                }

                file_entries.sort_by(|a, b| {
                    if a.is_dir == b.is_dir {
                        a.name.cmp(&b.name)
                    } else {
                        b.is_dir.cmp(&a.is_dir)
                    }
                });
                Ok(file_entries)
            })
            .await
            .map_err(|e| format!("Task join error: {}", e))?
        }
    }
}

#[tauri::command]
pub async fn create_directory(
    state: State<'_, AppState>,
    id: String,
    path: String,
) -> Result<(), String> {
    let client = {
        let clients = state.clients.lock().map_err(|e| e.to_string())?;
        clients.get(&id).ok_or("Session not found")?.clone()
    };

    match &client.client_type {
        ClientType::Ssh(sender) => {
            let sender = sender.clone();
            execute_ssh_operation(move || {
                let (tx, rx) = std::sync::mpsc::channel();
                sender
                    .send(SshCommand::SftpMkdir { path, listener: tx })
                    .map_err(|e| format!("Failed to send command: {}", e))?;

                rx.recv()
                    .map_err(|_| "Failed to receive response from SSH Manager".to_string())?
            })
            .await
        }
        ClientType::Wsl(distro) => {
            let distro = distro.clone();
            tokio::task::spawn_blocking(move || {
                let wsl_path = to_wsl_path(&distro, &path);
                std::fs::create_dir(wsl_path).map_err(|e| e.to_string())
            })
            .await
            .map_err(|e| format!("Task join error: {}", e))?
        }
    }
}

#[tauri::command]
pub async fn create_file(
    state: State<'_, AppState>,
    id: String,
    path: String,
) -> Result<(), String> {
    let client = {
        let clients = state.clients.lock().map_err(|e| e.to_string())?;
        clients.get(&id).ok_or("Session not found")?.clone()
    };

    match &client.client_type {
        ClientType::Ssh(sender) => {
            let sender = sender.clone();
            execute_ssh_operation(move || {
                let (tx, rx) = std::sync::mpsc::channel();
                sender
                    .send(SshCommand::SftpCreate { path, listener: tx })
                    .map_err(|e| format!("Failed to send command: {}", e))?;

                rx.recv()
                    .map_err(|_| "Failed to receive response from SSH Manager".to_string())?
            })
            .await
        }
        ClientType::Wsl(distro) => {
            let distro = distro.clone();
            tokio::task::spawn_blocking(move || {
                let wsl_path = to_wsl_path(&distro, &path);
                std::fs::File::create(wsl_path).map_err(|e| e.to_string())?;
                Ok(())
            })
            .await
            .map_err(|e| format!("Task join error: {}", e))?
        }
    }
}

#[tauri::command]
pub async fn delete_item(
    state: State<'_, AppState>,
    id: String,
    path: String,
    is_dir: bool,
) -> Result<(), String> {
    let client = {
        let clients = state.clients.lock().map_err(|e| e.to_string())?;
        clients.get(&id).ok_or("Session not found")?.clone()
    };

    match &client.client_type {
        ClientType::Ssh(sender) => {
            let sender = sender.clone();
            execute_ssh_operation(move || {
                let (tx, rx) = std::sync::mpsc::channel();
                sender
                    .send(SshCommand::SftpDelete {
                        path,
                        is_dir,
                        listener: tx,
                    })
                    .map_err(|e| format!("Failed to send command: {}", e))?;

                rx.recv()
                    .map_err(|_| "Failed to receive response from SSH Manager".to_string())?
            })
            .await
        }
        ClientType::Wsl(distro) => {
            let distro = distro.clone();
            tokio::task::spawn_blocking(move || {
                let wsl_path = to_wsl_path(&distro, &path);
                if is_dir {
                    std::fs::remove_dir_all(wsl_path).map_err(|e| e.to_string())
                } else {
                    std::fs::remove_file(wsl_path).map_err(|e| e.to_string())
                }
            })
            .await
            .map_err(|e| format!("Task join error: {}", e))?
        }
    }
}

// rm_recursive helper removed as it's now handled by SshManager

#[tauri::command]
pub async fn rename_item(
    state: State<'_, AppState>,
    id: String,
    old_path: String,
    new_path: String,
) -> Result<(), String> {
    let client = {
        let clients = state.clients.lock().map_err(|e| e.to_string())?;
        clients.get(&id).ok_or("Session not found")?.clone()
    };

    match &client.client_type {
        ClientType::Ssh(sender) => {
            let sender = sender.clone();
            execute_ssh_operation(move || {
                let (tx, rx) = std::sync::mpsc::channel();
                sender
                    .send(SshCommand::SftpRename {
                        old_path,
                        new_path,
                        listener: tx,
                    })
                    .map_err(|e| format!("Failed to send command: {}", e))?;

                rx.recv()
                    .map_err(|_| "Failed to receive response from SSH Manager".to_string())?
            })
            .await
        }
        ClientType::Wsl(distro) => {
            let distro = distro.clone();
            tokio::task::spawn_blocking(move || {
                let wsl_old = to_wsl_path(&distro, &old_path);
                let wsl_new = to_wsl_path(&distro, &new_path);
                std::fs::rename(wsl_old, wsl_new).map_err(|e| e.to_string())
            })
            .await
            .map_err(|e| format!("Task join error: {}", e))?
        }
    }
}

#[tauri::command]
pub async fn change_file_permission(
    state: State<'_, AppState>,
    id: String,
    path: String,
    permission: u32,
) -> Result<(), String> {
    let client = {
        let clients = state.clients.lock().map_err(|e| e.to_string())?;
        clients.get(&id).ok_or("Session not found")?.clone()
    };

    match &client.client_type {
        ClientType::Ssh(sender) => {
            let sender = sender.clone();
            execute_ssh_operation(move || {
                let (tx, rx) = std::sync::mpsc::channel();
                sender
                    .send(SshCommand::SftpChmod {
                        path,
                        mode: permission,
                        listener: tx,
                    })
                    .map_err(|e| format!("Failed to send command: {}", e))?;

                rx.recv()
                    .map_err(|_| "Failed to receive response from SSH Manager".to_string())?
            })
            .await
        }
        ClientType::Wsl(distro) => {
            let distro = distro.clone();
            tokio::task::spawn_blocking(move || {
                // wsl -d distro chmod octal path
                let octal = format!("{:o}", permission);
                let output = std::process::Command::new("wsl")
                    .arg("-d")
                    .arg(&distro)
                    .arg("chmod")
                    .arg(octal)
                    .arg(&path)
                    .output()
                    .map_err(|e| e.to_string())?;
                if !output.status.success() {
                    return Err(String::from_utf8_lossy(&output.stderr).to_string());
                }
                Ok(())
            })
            .await
            .map_err(|e| format!("Task join error: {}", e))?
        }
    }
}

#[tauri::command]
pub async fn get_transfers(state: State<'_, AppState>) -> Result<Vec<Transfer>, String> {
    let transfers_map = state.transfers.lock().map_err(|e| e.to_string())?;
    let mut transfers = Vec::new();
    for state in transfers_map.values() {
        let transfer = state.data.lock().map_err(|e| e.to_string())?;
        transfers.push(transfer.clone());
    }
    // Sort by created_at DESC
    transfers.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(transfers)
}

#[tauri::command]
pub async fn remove_transfer(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let mut transfers = state.transfers.lock().map_err(|e| e.to_string())?;
    transfers.remove(&id);
    Ok(())
}

#[tauri::command]
pub async fn download_file(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    transfer_id: String,
    remote_path: String,
    local_path: String,
) -> Result<String, String> {
    let client = {
        let clients = state.clients.lock().map_err(|e| e.to_string())?;
        clients.get(&id).ok_or("Session not found")?.clone()
    };

    let cancel_flag = Arc::new(AtomicBool::new(false));

    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;

    let name = Path::new(&remote_path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let transfer = Transfer {
        id: transfer_id.clone(),
        session_id: id.clone(),
        name,
        local_path: local_path.clone(),
        remote_path: remote_path.clone(),
        transfer_type: "download".to_string(),
        status: "pending".to_string(),
        total_size: 0,
        transferred: 0,
        created_at: now,
        error: None,
    };

    let transfer_state = Arc::new(TransferState {
        data: Mutex::new(transfer),
        cancel_flag: cancel_flag.clone(),
    });

    {
        let mut transfers = state.transfers.lock().map_err(|e| e.to_string())?;
        transfers.insert(transfer_id.clone(), transfer_state.clone());
    }

    let t_id_ssh = transfer_id.clone();
    let t_id_wsl = transfer_id.clone();
    let transfer_state_ssh = transfer_state.clone();
    let transfer_state_wsl = transfer_state.clone();

    // Spawn the operation
    match &client.client_type {
        ClientType::Ssh(sender) => {
            let sender = sender.clone();
            let app_handle = app.clone();
            let cancel_flag = transfer_state_ssh.cancel_flag.clone();
            let transfer_id = t_id_ssh;

            // Set status to running
            {
                let mut data = transfer_state_ssh.data.lock().unwrap();
                data.status = "running".to_string();
            }

            let tid_spawn = transfer_id.clone();
            tokio::spawn(async move {
                let (tx, rx) = std::sync::mpsc::channel();
                let res = sender.send(SshCommand::SftpDownload {
                    remote_path,
                    local_path,
                    transfer_id: tid_spawn.clone(),
                    app_handle,
                    listener: tx,
                    cancel_flag,
                });

                if let Err(e) = res {
                    let _ = app.emit(
                        "transfer-error",
                        ErrorPayload {
                            id: tid_spawn,
                            error: e.to_string(),
                        },
                    );
                    return;
                }

                // Wait for completion
                match rx.recv() {
                    Ok(Ok(_)) => {
                        // Success handled by manager emitting events?
                        // The manager emits "completed" event progress.
                        // But we might want to update local state here?
                        // Manager updates UI via events.
                        // We should ensure transfer state is updated in AppState?
                        // The manager does NOT have access to AppState directly, only app handle.
                        // So the manager emits events, but AppState is not updated?
                        // Current `file_ops` updates `transfer_state.data`.
                        // IF we moved logic to Manager, Manager doesn't update `state.transfers`.
                        // THIS IS A GAP.
                        // The Manager sends events to Frontend.
                        // But Backend state `state.transfers` remains "running" or "pending"?

                        // FIX: We need to listen to our own events? Or just update it here after rx.recv()?
                        // When rx.recv() returns Ok(), it means it's done (success).
                        let mut data = transfer_state_ssh.data.lock().unwrap();
                        data.status = "completed".to_string();
                        data.transferred = data.total_size;
                    }
                    Ok(Err(e)) => {
                        let mut data = transfer_state_ssh.data.lock().unwrap();
                        data.status = "error".to_string();
                        data.error = Some(e.clone());
                        let _ = app.emit(
                            "transfer-error",
                            ErrorPayload {
                                id: tid_spawn.clone(),
                                error: e,
                            },
                        );
                    }
                    Err(_) => {
                        let mut data = transfer_state_ssh.data.lock().unwrap();
                        data.status = "error".to_string();
                        data.error = Some("Channel closed".to_string());
                    }
                }
            });
            // Return ID immediately
            Ok::<String, String>(transfer_id)
        }
        ClientType::Wsl(distro) => {
            // For WSL, similar logic
            let distro = distro.clone();
            tokio::task::spawn_blocking(move || {
                let current_transfer_id = t_id_wsl;
                {
                    let mut data = transfer_state_wsl.data.lock().unwrap();
                    data.status = "running".to_string();
                }

                let wsl_path = to_wsl_path(&distro, &remote_path);
                let mut remote = std::fs::File::open(wsl_path).map_err(|e| e.to_string())?;
                let mut local = std::fs::File::create(&local_path).map_err(|e| e.to_string())?;
                let metadata = remote.metadata().map_err(|e| e.to_string())?;
                let total_size = metadata.len();
                {
                    let mut data = transfer_state_wsl.data.lock().unwrap();
                    data.total_size = total_size;
                }

                let mut buffer = [0u8; 8192];
                let mut transferred = 0u64;
                let mut last_emit = std::time::Instant::now();

                loop {
                    if cancel_flag.load(Ordering::Relaxed) {
                        {
                            let mut data = transfer_state_wsl.data.lock().unwrap();
                            data.status = "cancelled".to_string();
                        }
                        return Err("Download cancelled".to_string());
                    }
                    let n = remote.read(&mut buffer).map_err(|e| e.to_string())?;
                    if n == 0 {
                        break;
                    }
                    local.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
                    transferred += n as u64;

                    {
                        let mut data = transfer_state_wsl.data.lock().unwrap();
                        data.transferred = transferred;
                    }

                    if last_emit.elapsed().as_millis() > 100 {
                        let _ = app.emit(
                            "transfer-progress",
                            ProgressPayload {
                                id: current_transfer_id.clone(),
                                transferred,
                                total: total_size,
                            },
                        );
                        last_emit = std::time::Instant::now();
                    }
                }

                {
                    let mut data = transfer_state_wsl.data.lock().unwrap();
                    data.status = "completed".to_string();
                    data.transferred = total_size;
                }
                let _ = app.emit(
                    "transfer-progress",
                    ProgressPayload {
                        id: current_transfer_id.clone(),
                        transferred: total_size,
                        total: total_size,
                    },
                );

                Ok(())
            });
            // WSL branch returns the JoinHandle, but we need to unify return type or just let it run.
            // We want to return Ok(transfer_id)
            // We need to detach or await? Original code awaited.
            // If we await, we block. The user wants background generation?
            // "frontend request download, backend generates ID"
            // Usually this implies async handling.
            // If we want to return ID, we must SPAWN the work.

            // To make it compatible with the previous pattern which awaited:
            // The previous pattern awaited the result. If we want to return ID immediately, we MUST spawn.
            // Let's spawn and verify error handling later (maybe via event or status update).
            return Ok(transfer_id);
        }
    };

    // Redundant block removed

    Ok(transfer_id)
}

#[tauri::command]
pub async fn upload_file(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    transfer_id: String,
    local_path: String,
    remote_path: String,
) -> Result<String, String> {
    let client = {
        let clients = state.clients.lock().map_err(|e| e.to_string())?;
        clients.get(&id).ok_or("Session not found")?.clone()
    };

    let cancel_flag = Arc::new(AtomicBool::new(false));

    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;

    let name = Path::new(&local_path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let transfer = Transfer {
        id: transfer_id.clone(),
        session_id: id.clone(),
        name,
        local_path: local_path.clone(),
        remote_path: remote_path.clone(),
        transfer_type: "upload".to_string(),
        status: "pending".to_string(),
        total_size: 0,
        transferred: 0,
        created_at: now,
        error: None,
    };

    let transfer_state = Arc::new(TransferState {
        data: Mutex::new(transfer),
        cancel_flag: cancel_flag.clone(),
    });

    {
        let mut transfers = state.transfers.lock().map_err(|e| e.to_string())?;
        transfers.insert(transfer_id.clone(), transfer_state.clone());
    }

    let t_id_ssh = transfer_id.clone();
    let t_id_wsl = transfer_id.clone();
    let transfer_state_ssh = transfer_state.clone();
    let transfer_state_wsl = transfer_state.clone();

    match &client.client_type {
        ClientType::Ssh(sender) => {
            let sender = sender.clone();
            let app_handle = app.clone();
            let cancel_flag = transfer_state_ssh.cancel_flag.clone();
            let transfer_id = t_id_ssh;

            // Set status to running
            {
                let mut data = transfer_state_ssh.data.lock().unwrap();
                data.status = "running".to_string();
            }

            let tid_spawn = transfer_id.clone();

            tokio::spawn(async move {
                let (tx, rx) = std::sync::mpsc::channel();
                let res = sender.send(SshCommand::SftpUpload {
                    local_path,
                    remote_path,
                    transfer_id: tid_spawn.clone(),
                    app_handle,
                    listener: tx,
                    cancel_flag,
                });

                if let Err(e) = res {
                    let _ = app.emit(
                        "transfer-error",
                        ErrorPayload {
                            id: tid_spawn,
                            error: e.to_string(),
                        },
                    );
                    return;
                }

                // Wait for completion
                match rx.recv() {
                    Ok(Ok(_)) => {
                        let mut data = transfer_state_ssh.data.lock().unwrap();
                        data.status = "completed".to_string();
                        data.transferred = data.total_size;
                    }
                    Ok(Err(e)) => {
                        let mut data = transfer_state_ssh.data.lock().unwrap();
                        data.status = "error".to_string();
                        data.error = Some(e.clone());
                        let _ = app.emit(
                            "transfer-error",
                            ErrorPayload {
                                id: tid_spawn.clone(),
                                error: e,
                            },
                        );
                    }
                    Err(_) => {
                        let mut data = transfer_state_ssh.data.lock().unwrap();
                        data.status = "error".to_string();
                        data.error = Some("Channel closed".to_string());
                    }
                }
            });
            // Return ID immediately
            Ok::<String, String>(transfer_id)
        }
        ClientType::Wsl(distro) => {
            let distro = distro.clone();
            tokio::task::spawn_blocking(move || {
                let current_transfer_id = t_id_wsl;
                let ts = transfer_state_wsl;
                {
                    let mut data = ts.data.lock().unwrap();
                    data.status = "running".to_string();
                }

                let wsl_path = to_wsl_path(&distro, &remote_path);

                if let Some(parent) = wsl_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }

                let mut local = std::fs::File::open(&local_path).map_err(|e| e.to_string())?;
                let metadata = local.metadata().map_err(|e| e.to_string())?;
                let total_size = metadata.len();
                {
                    let mut data = ts.data.lock().unwrap();
                    data.total_size = total_size;
                }

                let mut remote = std::fs::File::create(wsl_path).map_err(|e| e.to_string())?;

                let mut buffer = [0u8; 8192];
                let mut transferred = 0u64;
                let mut last_emit = std::time::Instant::now();

                loop {
                    if ts.cancel_flag.load(Ordering::Relaxed) {
                        {
                            let mut data = ts.data.lock().unwrap();
                            data.status = "cancelled".to_string();
                        }
                        return Err("Upload cancelled".to_string());
                    }
                    let n = local.read(&mut buffer).map_err(|e| e.to_string())?;
                    if n == 0 {
                        break;
                    }
                    remote.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
                    transferred += n as u64;

                    {
                        let mut data = ts.data.lock().unwrap();
                        data.transferred = transferred;
                    }

                    if last_emit.elapsed().as_millis() > 100 {
                        let _ = app.emit(
                            "transfer-progress",
                            ProgressPayload {
                                id: current_transfer_id.clone(),
                                transferred,
                                total: total_size,
                            },
                        );
                        last_emit = std::time::Instant::now();
                    }
                }

                {
                    let mut data = ts.data.lock().unwrap();
                    data.status = "completed".to_string();
                    data.transferred = total_size;
                }
                let _ = app.emit(
                    "transfer-progress",
                    ProgressPayload {
                        id: current_transfer_id.clone(),
                        transferred: total_size,
                        total: total_size,
                    },
                );

                Ok(())
            });
            // As with download, allow background processing
            return Ok(transfer_id);
        }
    };

    Ok(transfer_id)
}

#[tauri::command]
pub async fn download_file_with_progress(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    transfer_id: String,
    remote_path: String,
    local_path: String,
    _resume: bool,
) -> Result<String, String> {
    download_file(app, state, id, transfer_id, remote_path, local_path).await
}

#[tauri::command]
pub async fn upload_file_with_progress(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    transfer_id: String,
    local_path: String,
    remote_path: String,
    _resume: bool,
) -> Result<String, String> {
    upload_file(app, state, id, transfer_id, local_path, remote_path).await
}

#[tauri::command]
pub async fn search_remote_files(
    state: State<'_, AppState>,
    id: String,
    path: String,
    query: String,
) -> Result<Vec<FileEntry>, String> {
    let client = {
        let clients = state.clients.lock().map_err(|e| e.to_string())?;
        clients.get(&id).ok_or("Session not found")?.clone()
    };

    match &client.client_type {
        ClientType::Ssh(sender) => {
            let sender = sender.clone();
            execute_ssh_operation(move || {
                let (tx, rx) = std::sync::mpsc::channel();
                // Escape single quotes in path and query to prevent command injection
                let escaped_path = path.replace('\'', "'\\''");
                let escaped_query = query.replace('\'', "'\\''");
                let cmd = format!("find '{}' -name '*{}*'", escaped_path, escaped_query);

                sender
                    .send(SshCommand::Exec {
                        command: cmd,
                        listener: tx,
                        cancel_flag: None,
                        is_ai: false,
                    })
                    .map_err(|e| format!("Failed to send command: {}", e))?;

                let output = rx
                    .recv()
                    .map_err(|_| "Failed to receive response from SSH Manager".to_string())?
                    .map_err(|e| format!("Find command failed: {}", e))?;

                let mut entries = Vec::new();
                for line in output.lines() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    let path_buf = PathBuf::from(line);
                    let name = path_buf
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    entries.push(FileEntry {
                        name,
                        is_dir: false,
                        size: 0,
                        mtime: 0,
                        permissions: 0,
                        uid: 0,
                        owner: "".to_string(),
                    });
                }
                Ok(entries)
            })
            .await
        }
        ClientType::Wsl(distro) => {
            let distro = distro.clone();
            tokio::task::spawn_blocking(move || {
                let output = std::process::Command::new("wsl")
                    .arg("-d")
                    .arg(&distro)
                    .arg("find")
                    .arg(&path)
                    .arg("-name")
                    .arg(format!("*{}*", query))
                    .output()
                    .map_err(|e| e.to_string())?;

                let out_str = String::from_utf8_lossy(&output.stdout);
                let mut entries = Vec::new();
                for line in out_str.lines() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    let path_buf = PathBuf::from(line);
                    let name = path_buf
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    entries.push(FileEntry {
                        name,
                        is_dir: false,
                        size: 0,
                        mtime: 0,
                        permissions: 0,
                        uid: 0,
                        owner: "".to_string(),
                    });
                }
                Ok(entries)
            })
            .await
            .map_err(|e| format!("Task join error: {}", e))?
        }
    }
}

fn create_remote_dir_recursive(sftp: &ssh2::Sftp, path: &Path) -> Result<(), ssh2::Error> {
    if path.as_os_str().is_empty() {
        return Ok(());
    }
    // Try to stat the directory. If it fails, try to create parent then create it.
    if sftp.stat(path).is_err() {
        if let Some(parent) = path.parent() {
            create_remote_dir_recursive(sftp, parent)?;
        }
        sftp.mkdir(path, 0o755)?;
    }
    Ok(())
}
