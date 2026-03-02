//! Async SFTP operations with timeout control
//!
//! This module provides async wrappers around SFTP operations with
//! timeout support, cancellation handling, and retry logic.

use crate::ssh::transfer::types::{TransferError, TransferSettings};
use ssh2::Sftp;
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::timeout;

/// Async SFTP operations wrapper
pub struct AsyncSftp<'a> {
    /// SFTP channel reference
    sftp: &'a mut Sftp,
    /// Transfer settings
    settings: &'a TransferSettings,
}

impl<'a> AsyncSftp<'a> {
    /// Create a new async SFTP wrapper
    pub fn new(sftp: &'a mut Sftp, settings: &'a TransferSettings) -> Self {
        Self { sftp, settings }
    }

    /// Download a file with timeout and progress tracking
    pub async fn download_with_timeout<F>(
        &mut self,
        remote_path: &str,
        local_path: &Path,
        progress_callback: F,
        cancel_flag: &Arc<AtomicBool>,
    ) -> Result<u64, TransferError>
    where
        F: Fn(u64, u64) + Send + Sync + 'static,
    {
        let operation_timeout = self.settings.operation_timeout();
        let no_progress_timeout = self.settings.no_progress_timeout();
        let chunk_size = self.settings.default_chunk_size;

        timeout(operation_timeout, async {
            self.download_internal(
                remote_path,
                local_path,
                chunk_size,
                progress_callback,
                cancel_flag,
                no_progress_timeout,
            )
            .await
        })
        .await
        .map_err(|_| TransferError::Timeout(format!(
            "Download operation timed out after {:?}",
            operation_timeout
        )))?
    }

    /// Internal download implementation with progress tracking
    async fn download_internal<F>(
        &mut self,
        remote_path: &str,
        local_path: &Path,
        chunk_size: usize,
        progress_callback: F,
        cancel_flag: &Arc<AtomicBool>,
        no_progress_timeout: Duration,
    ) -> Result<u64, TransferError>
    where
        F: Fn(u64, u64) + Send + Sync + 'static,
    {
        // Open remote file
        let mut remote_file = self
            .sftp
            .open(remote_path)
            .map_err(|e| TransferError::InvalidPath(format!("Failed to open remote file: {}", e)))?;

        // Get file size
        let metadata = remote_file
            .stat()
            .map_err(|e| TransferError::Unknown(format!("Failed to stat remote file: {}", e)))?;
        let file_size = metadata.size.unwrap_or(0) as u64;

        // Create local file
        let mut local_file =
            File::create(local_path).map_err(|e| TransferError::DiskFull(format!("Failed to create local file: {}", e)))?;

        // Download loop with progress tracking
        let mut buffer = vec![0u8; chunk_size];
        let mut total_transferred = 0u64;
        let mut last_progress_time = Instant::now();

        loop {
            // Check for cancellation
            if cancel_flag.load(Ordering::Relaxed) {
                return Err(TransferError::Cancelled);
            }

            // Check for no-progress timeout
            if last_progress_time.elapsed() > no_progress_timeout {
                return Err(TransferError::Timeout(format!(
                    "No progress for {:?}",
                    no_progress_timeout
                )));
            }

            // Read chunk from remote file
            let bytes_read = self
                .read_with_retry(&mut remote_file, &mut buffer)
                .map_err(|e| {
                    if e.is_connection_error() {
                        TransferError::ConnectionLost
                    } else {
                        e
                    }
                })?;

            if bytes_read == 0 {
                // EOF reached
                break;
            }

            // Write to local file
            local_file
                .write_all(&buffer[..bytes_read])
                .map_err(|e| TransferError::DiskFull(format!("Failed to write to local file: {}", e)))?;

            total_transferred += bytes_read as u64;
            last_progress_time = Instant::now();

            // Report progress
            progress_callback(total_transferred, file_size);
        }

        // Ensure all data is flushed
        local_file
            .flush()
            .map_err(|e| TransferError::DiskFull(format!("Failed to flush local file: {}", e)))?;

        Ok(total_transferred)
    }

    /// Upload a file with timeout and progress tracking
    pub async fn upload_with_timeout<F>(
        &mut self,
        local_path: &Path,
        remote_path: &str,
        progress_callback: F,
        cancel_flag: &Arc<AtomicBool>,
    ) -> Result<u64, TransferError>
    where
        F: Fn(u64, u64) + Send + Sync + 'static,
    {
        let operation_timeout = self.settings.operation_timeout();
        let no_progress_timeout = self.settings.no_progress_timeout();
        let chunk_size = self.settings.default_chunk_size;

        timeout(operation_timeout, async {
            self.upload_internal(
                local_path,
                remote_path,
                chunk_size,
                progress_callback,
                cancel_flag,
                no_progress_timeout,
            )
            .await
        })
        .await
        .map_err(|_| TransferError::Timeout(format!(
            "Upload operation timed out after {:?}",
            operation_timeout
        )))?
    }

    /// Internal upload implementation with progress tracking
    async fn upload_internal<F>(
        &mut self,
        local_path: &Path,
        remote_path: &str,
        chunk_size: usize,
        progress_callback: F,
        cancel_flag: &Arc<AtomicBool>,
        no_progress_timeout: Duration,
    ) -> Result<u64, TransferError>
    where
        F: Fn(u64, u64) + Send + Sync + 'static,
    {
        // Get local file size
        let file_size = std::fs::metadata(local_path)
            .map_err(|e| TransferError::InvalidPath(format!("Failed to stat local file: {}", e)))?
            .len();

        // Open local file
        let mut local_file =
            File::open(local_path).map_err(|e| TransferError::InvalidPath(format!("Failed to open local file: {}", e)))?;

        // Create remote file
        let mut remote_file = self
            .sftp
            .create(Path::new(remote_path))
            .map_err(|e| TransferError::PermissionDenied(format!("Failed to create remote file: {}", e)))?;

        // Upload loop with progress tracking
        let mut buffer = vec![0u8; chunk_size];
        let mut total_transferred = 0u64;
        let mut last_progress_time = Instant::now();

        loop {
            // Check for cancellation
            if cancel_flag.load(Ordering::Relaxed) {
                return Err(TransferError::Cancelled);
            }

            // Check for no-progress timeout
            if last_progress_time.elapsed() > no_progress_timeout {
                return Err(TransferError::Timeout(format!(
                    "No progress for {:?}",
                    no_progress_timeout
                )));
            }

            // Read chunk from local file
            let bytes_read = local_file
                .read(&mut buffer)
                .map_err(|e| TransferError::InvalidPath(format!("Failed to read from local file: {}", e)))?;

            if bytes_read == 0 {
                // EOF reached
                break;
            }

            // Write to remote file with retry
            self.write_with_retry(&mut remote_file, &buffer[..bytes_read])
                .map_err(|e| {
                    if e.is_connection_error() {
                        TransferError::ConnectionLost
                    } else {
                        e
                    }
                })?;

            total_transferred += bytes_read as u64;
            last_progress_time = Instant::now();

            // Report progress
            progress_callback(total_transferred, file_size);
        }

        Ok(total_transferred)
    }

    /// Resume download from a specific offset
    pub async fn resume_download<F>(
        &mut self,
        remote_path: &str,
        local_path: &Path,
        offset: u64,
        file_size: u64,
        progress_callback: F,
        cancel_flag: &Arc<AtomicBool>,
    ) -> Result<u64, TransferError>
    where
        F: Fn(u64, u64) + Send + Sync + 'static,
    {
        let operation_timeout = self.settings.operation_timeout();
        let no_progress_timeout = self.settings.no_progress_timeout();
        let chunk_size = self.settings.default_chunk_size;

        timeout(operation_timeout, async {
            self.resume_download_internal(
                remote_path,
                local_path,
                offset,
                file_size,
                chunk_size,
                progress_callback,
                cancel_flag,
                no_progress_timeout,
            )
            .await
        })
        .await
        .map_err(|_| {
            TransferError::Timeout(format!("Resume download timed out after {:?}", operation_timeout))
        })?
    }

    /// Internal resume download implementation
    async fn resume_download_internal<F>(
        &mut self,
        remote_path: &str,
        local_path: &Path,
        offset: u64,
        file_size: u64,
        chunk_size: usize,
        progress_callback: F,
        cancel_flag: &Arc<AtomicBool>,
        no_progress_timeout: Duration,
    ) -> Result<u64, TransferError>
    where
        F: Fn(u64, u64) + Send + Sync + 'static,
    {
        // Open remote file and seek to offset
        let mut remote_file = self
            .sftp
            .open(remote_path)
            .map_err(|e| TransferError::InvalidPath(format!("Failed to open remote file: {}", e)))?;

        remote_file
            .seek(std::io::SeekFrom::Start(offset))
            .map_err(|e| TransferError::CannotResume(format!("Failed to seek in remote file: {}", e)))?;

        // Open local file in append mode
        let mut local_file =
            File::options()
                .write(true)
                .open(local_path)
                .map_err(|e| TransferError::DiskFull(format!("Failed to open local file: {}", e)))?;

        local_file
            .seek(std::io::SeekFrom::Start(offset))
            .map_err(|e| TransferError::CannotResume(format!("Failed to seek in local file: {}", e)))?;

        // Download loop starting from offset
        let mut buffer = vec![0u8; chunk_size];
        let mut total_transferred = offset;
        let mut last_progress_time = Instant::now();

        loop {
            // Check for cancellation
            if cancel_flag.load(Ordering::Relaxed) {
                return Err(TransferError::Cancelled);
            }

            // Check for no-progress timeout
            if last_progress_time.elapsed() > no_progress_timeout {
                return Err(TransferError::Timeout(format!(
                    "No progress for {:?}",
                    no_progress_timeout
                )));
            }

            // Read chunk from remote file
            let bytes_read = self
                .read_with_retry(&mut remote_file, &mut buffer)
                .map_err(|e| {
                    if e.is_connection_error() {
                        TransferError::ConnectionLost
                    } else {
                        e
                    }
                })?;

            if bytes_read == 0 {
                break;
            }

            // Write to local file
            local_file
                .write_all(&buffer[..bytes_read])
                .map_err(|e| TransferError::DiskFull(format!("Failed to write to local file: {}", e)))?;

            total_transferred += bytes_read as u64;
            last_progress_time = Instant::now();

            // Report progress
            progress_callback(total_transferred, file_size);
        }

        local_file
            .flush()
            .map_err(|e| TransferError::DiskFull(format!("Failed to flush local file: {}", e)))?;

        Ok(total_transferred - offset)
    }

    /// Resume upload from a specific offset
    pub async fn resume_upload<F>(
        &mut self,
        local_path: &Path,
        remote_path: &str,
        offset: u64,
        file_size: u64,
        progress_callback: F,
        cancel_flag: &Arc<AtomicBool>,
    ) -> Result<u64, TransferError>
    where
        F: Fn(u64, u64) + Send + Sync + 'static,
    {
        let operation_timeout = self.settings.operation_timeout();
        let no_progress_timeout = self.settings.no_progress_timeout();
        let chunk_size = self.settings.default_chunk_size;

        timeout(operation_timeout, async {
            self.resume_upload_internal(
                local_path,
                remote_path,
                offset,
                file_size,
                chunk_size,
                progress_callback,
                cancel_flag,
                no_progress_timeout,
            )
            .await
        })
        .await
        .map_err(|_| {
            TransferError::Timeout(format!("Resume upload timed out after {:?}", operation_timeout))
        })?
    }

    /// Internal resume upload implementation
    async fn resume_upload_internal<F>(
        &mut self,
        local_path: &Path,
        remote_path: &str,
        offset: u64,
        file_size: u64,
        chunk_size: usize,
        progress_callback: F,
        cancel_flag: &Arc<AtomicBool>,
        no_progress_timeout: Duration,
    ) -> Result<u64, TransferError>
    where
        F: Fn(u64, u64) + Send + Sync + 'static,
    {
        // Open local file and seek to offset
        let mut local_file =
            File::open(local_path).map_err(|e| TransferError::InvalidPath(format!("Failed to open local file: {}", e)))?;

        local_file
            .seek(std::io::SeekFrom::Start(offset))
            .map_err(|e| TransferError::CannotResume(format!("Failed to seek in local file: {}", e)))?;

        // Open remote file and seek to offset
        let mut remote_file = self
            .sftp
            .open(remote_path)
            .map_err(|e| TransferError::PermissionDenied(format!("Failed to open remote file: {}", e)))?;

        remote_file
            .seek(std::io::SeekFrom::Start(offset))
            .map_err(|e| TransferError::CannotResume(format!("Failed to seek in remote file: {}", e)))?;

        // Upload loop starting from offset
        let mut buffer = vec![0u8; chunk_size];
        let mut total_transferred = offset;
        let mut last_progress_time = Instant::now();

        loop {
            // Check for cancellation
            if cancel_flag.load(Ordering::Relaxed) {
                return Err(TransferError::Cancelled);
            }

            // Check for no-progress timeout
            if last_progress_time.elapsed() > no_progress_timeout {
                return Err(TransferError::Timeout(format!(
                    "No progress for {:?}",
                    no_progress_timeout
                )));
            }

            // Read chunk from local file
            let bytes_read = local_file
                .read(&mut buffer)
                .map_err(|e| TransferError::InvalidPath(format!("Failed to read from local file: {}", e)))?;

            if bytes_read == 0 {
                break;
            }

            // Write to remote file with retry
            self.write_with_retry(&mut remote_file, &buffer[..bytes_read])
                .map_err(|e| {
                    if e.is_connection_error() {
                        TransferError::ConnectionLost
                    } else {
                        e
                    }
                })?;

            total_transferred += bytes_read as u64;
            last_progress_time = Instant::now();

            // Report progress
            progress_callback(total_transferred, file_size);
        }

        Ok(total_transferred - offset)
    }

    /// Read from file with retry logic
    fn read_with_retry(&self, file: &mut ssh2::File, buffer: &mut [u8]) -> Result<usize, TransferError> {
        let mut attempt = 0;
        let max_attempts = self.settings.max_retry_attempts as usize;
        let retry_delay = self.settings.retry_delay();

        loop {
            match file.read(buffer) {
                Ok(n) => return Ok(n),
                Err(e) => {
                    // WouldBlock is a temporary error that can be retried
                    if e.kind() == std::io::ErrorKind::WouldBlock && attempt < max_attempts {
                        attempt += 1;
                        std::thread::sleep(retry_delay);
                        continue;
                    }
                    return Err(TransferError::from(e));
                }
            }
        }
    }

    /// Write to file with retry logic
    fn write_with_retry(&self, file: &mut ssh2::File, data: &[u8]) -> Result<usize, TransferError> {
        let mut attempt = 0;
        let max_attempts = self.settings.max_retry_attempts as usize;
        let retry_delay = self.settings.retry_delay();

        let mut total_written = 0;

        while total_written < data.len() {
            match file.write(&data[total_written..]) {
                Ok(0) => {
                    // Write returned 0 bytes, this is an error
                    return Err(TransferError::Unknown("Write returned 0 bytes".to_string()));
                }
                Ok(n) => {
                    total_written += n;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if attempt < max_attempts {
                        attempt += 1;
                        std::thread::sleep(retry_delay);
                        continue;
                    }
                    return Err(TransferError::WouldBlock);
                }
                Err(e) => return Err(TransferError::from(e)),
            }
        }

        Ok(total_written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timeout_settings() {
        let settings = TransferSettings::default();
        assert_eq!(settings.operation_timeout(), Duration::from_secs(60));
        assert_eq!(settings.no_progress_timeout(), Duration::from_secs(30));
    }
}
