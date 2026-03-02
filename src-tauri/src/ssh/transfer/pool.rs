//! Transfer connection pool implementation
//!
//! This module provides a dedicated connection pool for file transfers,
//! isolated from the main session pool to prevent conflicts with heartbeat
//! and other session operations.

use crate::models::Connection as SshConnConfig;
use crate::ssh::connection::{establish_connection_with_retry, ManagedSession};
use crate::ssh::transfer::types::TransferSettings;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

/// A single transfer connection with SFTP channel
pub struct TransferConnection {
    /// The managed SSH session (contains the session and jump session)
    pub managed_session: ManagedSession,
    /// SFTP channel created from the session
    sftp: Option<ssh2::Sftp>,
    /// When this connection was last used
    last_used: Instant,
    /// Whether this connection is currently in use
    in_use: AtomicBool,
    /// Unique identifier for this connection
    id: usize,
}

impl TransferConnection {
    /// Create a new transfer connection from a managed session
    fn new(managed_session: ManagedSession, id: usize) -> Result<Self, String> {
        let sftp = managed_session
            .session
            .sftp()
            .map_err(|e| format!("Failed to create SFTP channel: {}", e))?;

        Ok(Self {
            managed_session,
            sftp: Some(sftp),
            last_used: Instant::now(),
            in_use: AtomicBool::new(false),
            id,
        })
    }

    /// Mark this connection as in use
    pub fn acquire(&self) -> bool {
        !self.in_use.swap(true, Ordering::Acquire)
    }

    /// Release this connection
    pub fn release(&self) {
        self.in_use.store(false, Ordering::Release);
    }

    /// Update last used time
    pub fn touch(&self) {
        // Note: This would need interior mutability in real implementation
        // For now, we track this at the pool level
    }

    /// Check if this connection is idle (not in use)
    pub fn is_idle(&self) -> bool {
        !self.in_use.load(Ordering::Relaxed)
    }

    /// Get a reference to the SFTP channel, creating it if needed
    pub fn sftp(&mut self) -> Result<&mut ssh2::Sftp, String> {
        if self.sftp.is_none() {
            self.sftp = Some(
                self.managed_session
                    .session
                    .sftp()
                    .map_err(|e| format!("Failed to create SFTP channel: {}", e))?,
            );
        }
        Ok(self.sftp.as_mut().expect("SFTP should be Some"))
    }

    /// Check if the session is still alive
    pub fn is_alive(&self) -> bool {
        // Try to send a keepalive to check if the session is still alive
        self.managed_session.session.keepalive_send().is_ok()
    }
}

/// Transfer connection pool, isolated from main session pool
pub struct TransferPool {
    /// Connections organized by client ID
    pools: HashMap<String, PoolEntry>,
    /// Connection settings
    config: SshConnConfig,
    /// Transfer settings
    settings: TransferSettings,
    /// Next connection ID
    next_id: usize,
}

/// Entry in the transfer pool for a specific client
struct PoolEntry {
    /// Available connections (wrapped in Arc<Mutex>> for shared access)
    connections: Vec<Arc<Mutex<TransferConnection>>>,
    /// Last access time for the pool entry
    last_access: Instant,
    /// Connection index for ID generation
    next_index: usize,
}

impl TransferPool {
    /// Create a new transfer pool
    pub fn new(config: SshConnConfig, settings: TransferSettings) -> Self {
        Self {
            pools: HashMap::new(),
            config,
            settings,
            next_id: 0,
        }
    }

    /// Acquire a connection for the given client ID
    pub async fn acquire(&mut self, client_id: &str) -> Result<Arc<Mutex<TransferConnection>>, String> {
        // Get or create pool entry
        let needs_new_connection = {
            let entry = self.pools.entry(client_id.to_string()).or_insert_with(|| {
                PoolEntry {
                    connections: Vec::new(),
                    last_access: Instant::now(),
                    next_index: 0,
                }
            });

            entry.last_access = Instant::now();

            // Try to find an idle connection
            for conn in &entry.connections {
                let guard = conn.lock().await;
                if guard.acquire() {
                    return Ok(conn.clone());
                }
            }

            // Check if we can create a new connection
            entry.connections.len() < self.settings.max_transfer_connections
        };

        if needs_new_connection {
            // Get the next index
            let next_index = {
                let entry = self.pools.get_mut(client_id).expect("entry should exist");
                let idx = entry.next_index;
                entry.next_index += 1;
                idx
            };

            let new_conn = self.create_connection(next_index).await?;

            // Mark as in use
            new_conn.acquire();

            let conn_arc = Arc::new(Mutex::new(new_conn));

            // Add to pool
            {
                let entry = self.pools.get_mut(client_id).expect("entry should exist");
                entry.connections.push(conn_arc.clone());
            }

            return Ok(conn_arc);
        }

        // All connections busy, wait and retry
        // In a real implementation, this would use a proper wait/notify mechanism
        // For now, we'll return an error
        Err(format!(
            "No available transfer connections for client {} (max: {})",
            client_id, self.settings.max_transfer_connections
        ))
    }

    /// Release a connection back to the pool
    pub fn release(&mut self, client_id: &str, conn: Arc<Mutex<TransferConnection>>) {
        // Only release if the pool entry exists
        if self.pools.contains_key(client_id) {
            let mut guard = conn.blocking_lock();
            guard.release();
        }
    }

    /// Clean up idle connections for all clients
    pub async fn cleanup_idle(&mut self) {
        let idle_timeout = self.settings.idle_timeout();
        let now = Instant::now();

        let mut clients_to_remove = Vec::new();

        for (client_id, entry) in &mut self.pools {
            // Remove entries that haven't been accessed for a long time
            if now.duration_since(entry.last_access) > idle_timeout * 2 {
                clients_to_remove.push(client_id.clone());
                continue;
            }

            // Clean up idle individual connections
            entry.connections.retain(|conn| {
                let guard = conn.blocking_lock();
                if guard.is_idle() {
                    // Check last_used time (we'd need to track this properly)
                    // For now, just check if idle
                    now.duration_since(guard.last_used) > idle_timeout
                } else {
                    true
                }
            });
        }

        // Remove stale client entries
        for client_id in clients_to_remove {
            self.pools.remove(&client_id);
        }
    }

    /// Get pool statistics
    pub fn stats(&self, client_id: &str) -> Option<PoolStats> {
        self.pools.get(client_id).map(|entry| {
            let idle_count = entry
                .connections
                .iter()
                .filter(|c| c.blocking_lock().is_idle())
                .count();

            PoolStats {
                total_connections: entry.connections.len(),
                idle_connections: idle_count,
                last_access: entry.last_access,
            }
        })
    }

    /// Create a new connection
    async fn create_connection(&mut self, index: usize) -> Result<TransferConnection, String> {
        // Use the connection establishment logic
        let config = self.config.clone();
        let managed_session = tokio::task::spawn_blocking(move || {
            establish_connection_with_retry(&config, None, None)
        })
        .await
        .map_err(|e| format!("Failed to join connection task: {}", e))?
        .map_err(|e| format!("Failed to establish connection: {}", e))?;

        TransferConnection::new(managed_session, index)
    }

    /// Remove all connections for a client
    pub fn remove_client(&mut self, client_id: &str) {
        self.pools.remove(client_id);
    }

    /// Get total number of connections across all clients
    pub fn total_connections(&self) -> usize {
        self.pools.values().map(|e| e.connections.len()).sum()
    }

    /// Get number of idle connections across all clients
    pub fn total_idle_connections(&self) -> usize {
        self.pools
            .values()
            .flat_map(|e| {
                e.connections.iter().filter(|c| {
                    c.blocking_lock().is_idle()
                })
            })
            .count()
    }
}

/// Pool statistics for monitoring
#[derive(Debug, Clone)]
pub struct PoolStats {
    pub total_connections: usize,
    pub idle_connections: usize,
    pub last_access: Instant,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_creation() {
        let config = SshConnConfig {
            id: None,
            name: "test".to_string(),
            host: "localhost".to_string(),
            port: 22,
            username: "test".to_string(),
            password: None,
            auth_type: None,
            ssh_key_id: None,
            jump_host: None,
            jump_port: None,
            jump_username: None,
            jump_password: None,
            group_id: None,
            os_type: None,
            key_content: None,
            key_passphrase: None,
        };

        let settings = TransferSettings::default();
        let pool = TransferPool::new(config, settings);

        assert_eq!(pool.total_connections(), 0);
        assert_eq!(pool.total_idle_connections(), 0);
    }

    #[test]
    fn test_connection_acquire_release() {
        let conn = TransferConnection {
            session: Session::new().unwrap(), // Note: This will fail in test without proper setup
            sftp: None,
            last_used: Instant::now(),
            in_use: AtomicBool::new(false),
            id: 0,
        };

        assert!(conn.is_idle());
        assert!(conn.acquire());
        assert!(!conn.is_idle());
        conn.release();
        assert!(conn.is_idle());
    }

    #[test]
    fn test_pool_stats() {
        let stats = PoolStats {
            total_connections: 5,
            idle_connections: 2,
            last_access: Instant::now(),
        };

        assert_eq!(stats.total_connections, 5);
        assert_eq!(stats.idle_connections, 2);
    }
}
