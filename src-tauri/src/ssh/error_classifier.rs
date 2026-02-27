//! SSH Error Classifier
//!
//! Classifies SSH errors into categories to determine appropriate retry behavior:
//! - Temporary: Network fluctuations, server busy - should retry
//! - Permanent: Authentication failures, host not found - should not retry
//! - RateLimited: Too many connections - retry with longer delay
//! - ResourceExhausted: Server resources exhausted - retry with backoff

use std::io;

/// Classification of SSH error types for retry decision making
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshErrorType {
    /// Temporary errors that may resolve on retry (network issues, timeouts)
    Temporary,
    /// Permanent errors that will not succeed on retry (auth failure, invalid host)
    Permanent,
    /// Rate limiting errors - should retry with extended delay
    RateLimited,
    /// Resource exhaustion - server overloaded, should retry with backoff
    ResourceExhausted,
}

/// SSH Error Classifier
///
/// Analyzes error messages and codes to classify errors into categories
/// that inform retry strategies.
pub struct SshErrorClassifier;

impl SshErrorClassifier {
    /// Classify an ssh2::Error based on its code and message
    pub fn classify(error: &ssh2::Error) -> SshErrorType {
        let code = error.code();
        let msg = error.to_string().to_lowercase();

        // Check for permanent error codes first
        match code {
            // Authentication failures - permanent
            ssh2::ErrorCode::Session(-18) => SshErrorType::Permanent, // SSH_AUTH_ERROR
            ssh2::ErrorCode::Session(-19) => SshErrorType::Permanent, // SSH_REQUEST_DENIED

            // Host key verification failures - permanent
            ssh2::ErrorCode::Session(-30) => SshErrorType::Permanent, // Known hosts check failure

            // Invalid parameters - permanent
            ssh2::ErrorCode::Session(-40) => SshErrorType::Permanent, // INVALID_REQUEST

            _ => {
                // Fall back to message-based classification
                Self::classify_from_string(&msg)
            }
        }
    }

    /// Classify a std::io::Error
    pub fn classify_io_error(error: &io::Error) -> SshErrorType {
        let msg = error.to_string().to_lowercase();

        match error.kind() {
            // Temporary network errors
            io::ErrorKind::WouldBlock |
            io::ErrorKind::TimedOut |
            io::ErrorKind::Interrupted |
            io::ErrorKind::ConnectionReset |
            io::ErrorKind::ConnectionAborted => SshErrorType::Temporary,

            // Permanent errors
            io::ErrorKind::NotFound |
            io::ErrorKind::PermissionDenied |
            io::ErrorKind::ConnectionRefused => SshErrorType::Permanent,

            // Other errors - check message content
            _ => Self::classify_from_string(&msg)
        }
    }

    /// Classify error based on string message content
    pub fn classify_from_string(error: &str) -> SshErrorType {
        let msg = error.to_lowercase();

        // Permanent error patterns
        let permanent_patterns = [
            "authentication failed",
            "auth failed",
            "permission denied",
            "access denied",
            "invalid user",
            "invalid password",
            "wrong password",
            "incorrect password",
            "key authentication failed",
            "publickey authentication failed",
            "host key verification failed",
            "host key mismatch",
            "no such host",
            "name or service not known",
            "nodename nor servname provided",
            "address not available",
            "network is unreachable",
            "connection refused",
            "protocol error",
            "invalid",
            "unsupported",
            "not supported",
            "disabled",
            "banned",
            "blocked",
            "blacklisted",
        ];

        // Rate limiting patterns
        let rate_limit_patterns = [
            "too many",
            "rate limit",
            "throttl",
            "slow down",
            "max sessions",
            "max connections",
            "connection limit",
            "try again later",
        ];

        // Resource exhaustion patterns
        let resource_patterns = [
            "resource",
            "out of memory",
            "oom",
            "overload",
            "busy",
            "try again",
            "temporarily unavailable",
            "service unavailable",
        ];

        // Check permanent patterns first (highest priority)
        for pattern in &permanent_patterns {
            if msg.contains(pattern) {
                return SshErrorType::Permanent;
            }
        }

        // Check rate limiting
        for pattern in &rate_limit_patterns {
            if msg.contains(pattern) {
                return SshErrorType::RateLimited;
            }
        }

        // Check resource exhaustion
        for pattern in &resource_patterns {
            if msg.contains(pattern) {
                return SshErrorType::ResourceExhausted;
            }
        }

        // Default to temporary for unknown errors
        SshErrorType::Temporary
    }

    /// Check if an error type should trigger a retry
    pub fn should_retry(error_type: SshErrorType) -> bool {
        match error_type {
            SshErrorType::Temporary => true,
            SshErrorType::Permanent => false,
            SshErrorType::RateLimited => true,
            SshErrorType::ResourceExhausted => true,
        }
    }

    /// Get a human-readable description of the error type
    pub fn describe(error_type: SshErrorType) -> &'static str {
        match error_type {
            SshErrorType::Temporary => "Temporary error - network issue or timeout",
            SshErrorType::Permanent => "Permanent error - authentication or configuration issue",
            SshErrorType::RateLimited => "Rate limited - server is limiting connections",
            SshErrorType::ResourceExhausted => "Resource exhausted - server is overloaded",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_permanent_auth() {
        let msg = "Authentication failed: invalid password";
        assert_eq!(SshErrorClassifier::classify_from_string(msg), SshErrorType::Permanent);
    }

    #[test]
    fn test_classify_permanent_host() {
        let msg = "Host key verification failed";
        assert_eq!(SshErrorClassifier::classify_from_string(msg), SshErrorType::Permanent);
    }

    #[test]
    fn test_classify_rate_limited() {
        let msg = "Too many connections from this IP";
        assert_eq!(SshErrorClassifier::classify_from_string(msg), SshErrorType::RateLimited);
    }

    #[test]
    fn test_classify_resource_exhausted() {
        let msg = "Server is busy, try again later";
        assert_eq!(SshErrorClassifier::classify_from_string(msg), SshErrorType::ResourceExhausted);
    }

    #[test]
    fn test_classify_temporary() {
        let msg = "Connection timed out";
        assert_eq!(SshErrorClassifier::classify_from_string(msg), SshErrorType::Temporary);
    }

    #[test]
    fn test_should_retry() {
        assert!(SshErrorClassifier::should_retry(SshErrorType::Temporary));
        assert!(!SshErrorClassifier::should_retry(SshErrorType::Permanent));
        assert!(SshErrorClassifier::should_retry(SshErrorType::RateLimited));
        assert!(SshErrorClassifier::should_retry(SshErrorType::ResourceExhausted));
    }
}
