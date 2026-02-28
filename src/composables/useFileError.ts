/**
 * 文件操作错误类型
 */
export type FileErrorType = 'network' | 'permission' | 'not_found' | 'session' | 'timeout' | 'unknown';

/**
 * 结构化文件操作错误
 */
export interface FileOperationError {
    errorType: FileErrorType;
    message: string;
    retryable: boolean;
    originalError?: string;
}

/**
 * 解析文件操作错误消息，返回结构化错误信息
 */
export function parseFileError(error: unknown): FileOperationError {
    const msg = error instanceof Error ? error.message : String(error);
    const msgLower = msg.toLowerCase();

    let errorType: FileErrorType;
    let retryable: boolean;

    if (
        msgLower.includes('permission denied') ||
        msgLower.includes('access denied') ||
        msgLower.includes('not authorized')
    ) {
        errorType = 'permission';
        retryable = false;
    } else if (
        msgLower.includes('not found') ||
        msgLower.includes('no such file') ||
        msgLower.includes('does not exist')
    ) {
        errorType = 'not_found';
        retryable = false;
    } else if (
        msgLower.includes('timeout') ||
        msgLower.includes('timed out')
    ) {
        errorType = 'timeout';
        retryable = true;
    } else if (
        msgLower.includes('connection reset') ||
        msgLower.includes('connection lost') ||
        msgLower.includes('network')
    ) {
        errorType = 'network';
        retryable = true;
    } else if (
        msgLower.includes('session') ||
        msgLower.includes('disconnected')
    ) {
        errorType = 'session';
        retryable = true;
    } else {
        errorType = 'unknown';
        retryable = false;
    }

    return {
        errorType,
        message: msg,
        retryable,
        originalError: msg,
    };
}

/**
 * 获取用户友好的错误消息
 */
export function getErrorMessage(error: FileOperationError, t?: (key: string) => string): string {
    const baseMessage = error.message;

    // 如果有翻译函数，尝试获取本地化的错误类型消息
    if (t) {
        const typeMessages: Record<FileErrorType, string> = {
            network: t('errors.networkError') || 'Network error',
            permission: t('errors.permissionDenied') || 'Permission denied',
            not_found: t('errors.notFound') || 'File or directory not found',
            session: t('errors.sessionError') || 'Session error',
            timeout: t('errors.timeout') || 'Operation timed out',
            unknown: t('errors.unknownError') || 'Unknown error',
        };

        return `${typeMessages[error.errorType]}: ${baseMessage}`;
    }

    return baseMessage;
}

/**
 * 判断错误是否可重试
 */
export function isRetryableError(error: unknown): boolean {
    const parsed = parseFileError(error);
    return parsed.retryable;
}
