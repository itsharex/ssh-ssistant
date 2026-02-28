import { defineStore } from 'pinia';
import { ref } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useNotificationStore } from './notifications';

export type TransferStatus = 'pending' | 'running' | 'paused' | 'completed' | 'error' | 'cancelled';

export interface TransferItem {
    id: string;
    type: 'upload' | 'download';
    name: string;
    localPath: string;
    remotePath: string;
    size: number;
    transferred: number;
    progress: number; // 0-100
    status: TransferStatus;
    error?: string;
    speed?: string;
    sessionId: string;
    isDirectory?: boolean;
    childFiles?: number;
    completedFiles?: number;
    // Helper to track if this item is syncing with backend (temp ID)
    isTemp?: boolean;
}

export const useTransferStore = defineStore('transfers', () => {
    const items = ref<TransferItem[]>([]);
    const active = ref(false);
    const maxConcurrent = 3;

    // Listen for progress events
    let unlisten: (() => void) | null = null;

    const directoryProgress = new Map<string, {
        totalFiles: number;
        completedFiles: number;
        totalSize: number;
        transferredSize: number;
        isPaused: boolean;
        pausedFiles: Set<string>;
    }>();

    async function initListeners() {
        // First sync with backend to restore state
        await syncWithBackend();

        if (unlisten) return;

        const unlistenProgress = await listen('transfer-progress', (event: any) => {
            const payload = event.payload as { id: string, transferred: number, total: number };
            const item = items.value.find(i => i.id === payload.id);
            if (item) {
                // Update item state
                item.transferred = payload.transferred;
                item.size = payload.total;
                item.progress = payload.total > 0 ? Math.round((payload.transferred / payload.total) * 100) : 0;

                if (item.status !== 'cancelled' && item.status !== 'error') {
                    item.status = payload.transferred >= payload.total && payload.total > 0 ? 'completed' : 'running';
                }

                updateDirectoryProgress(item.id, payload.transferred, payload.total);
            }
        });

        const unlistenError = await listen('transfer-error', (event: any) => {
            const payload = event.payload as { id: string, error: string };
            const item = items.value.find(i => i.id === payload.id);
            if (item) {
                item.status = 'error';
                item.error = payload.error;
            }
        });

        unlisten = () => {
            unlistenProgress();
            unlistenError();
        };
    }

    async function syncWithBackend() {
        try {
            const transfers = await invoke<any[]>('get_transfers');

            // 获取后端 ID 集合
            const backendIds = new Set(transfers.map(t => t.id));

            // 保留 pending 和本地临时状态的项（这些可能还未同步到后端）
            const localOnlyItems = items.value.filter(i =>
                i.status === 'pending' ||
                (i.isTemp && !backendIds.has(i.id))
            );

            // 映射后端数据
            const mappedItems: TransferItem[] = transfers.map(t => ({
                id: t.id,
                type: t.transfer_type as 'upload' | 'download',
                name: t.name,
                localPath: t.local_path,
                remotePath: t.remote_path,
                size: t.total_size,
                transferred: t.transferred,
                progress: t.total_size > 0 ? Math.round((t.transferred / t.total_size) * 100) : 0,
                status: t.status as TransferStatus,
                error: t.error || undefined,
                sessionId: t.session_id,
                isDirectory: t.is_directory,
                childFiles: t.child_files,
                completedFiles: t.completed_files,
            }));

            // 合并：后端状态优先，但保留本地独有项
            items.value = [...mappedItems, ...localOnlyItems];

            // 清理 directoryProgress 中已完成的项
            const validIds = new Set(items.value.map(i => i.id));
            for (const id of directoryProgress.keys()) {
                if (!validIds.has(id)) {
                    directoryProgress.delete(id);
                }
            }

            // 验证状态一致性
            validateStateConsistency();

        } catch (e) {
            console.error("Failed to sync transfers:", e);
            const notificationStore = useNotificationStore();
            notificationStore.warning(`Transfer sync failed. Some transfers may not be visible.`);
        }
    }

    function updateDirectoryProgress(fileTransferId: string, transferred: number, _total: number) {
        const directoryItem = items.value.find(item =>
            item.isDirectory && item.remotePath && fileTransferId.startsWith(item.remotePath)
        );

        if (directoryItem) {
            const progress = directoryProgress.get(directoryItem.id);
            if (progress) {
                progress.transferredSize += transferred;
                directoryItem.transferred = progress.transferredSize;
                directoryItem.progress = progress.totalSize > 0 ? Math.round((progress.transferredSize / progress.totalSize) * 100) : 0;
            }
        }
    }

    function processQueue() {
        const runningCount = items.value.filter(i => i.status === 'running').length;
        if (runningCount >= maxConcurrent) return;

        const nextItem = items.value.find(i => i.status === 'pending');
        if (nextItem) {
            void startTransfer(nextItem.id);
        }
    }

    function addTransfer(item: TransferItem) {
        // Mark as temp so we know to replace ID later
        item.isTemp = true;
        items.value.unshift(item);
        if (!item.isDirectory) {
            processQueue();
        }
    }

    function addDirectoryTransfer(remotePath: string, localPath: string, sessionId: string) {
        const dirName = remotePath.split('/').pop() || 'directory';
        const transferId = 'temp-' + (typeof crypto !== 'undefined' && crypto.randomUUID ? crypto.randomUUID() : Math.random().toString(36).substring(2));

        const directoryItem: TransferItem = {
            id: transferId,
            type: 'download',
            name: dirName,
            localPath,
            remotePath,
            size: 0,
            transferred: 0,
            progress: 0,
            status: 'pending',
            sessionId,
            isDirectory: true,
            childFiles: 0,
            completedFiles: 0,
            isTemp: true
        };

        items.value.unshift(directoryItem);
        return transferId;
    }

    function updateDirectoryStats(directoryId: string, totalFiles: number, totalSize: number) {
        const item = items.value.find(i => i.id === directoryId);
        if (item && item.isDirectory) {
            item.childFiles = totalFiles;
            item.size = totalSize;

            directoryProgress.set(directoryId, {
                totalFiles,
                completedFiles: 0,
                totalSize,
                transferredSize: 0,
                isPaused: false,
                pausedFiles: new Set()
            });
        }
    }

    function incrementDirectoryCompleted(directoryId: string) {
        const progress = directoryProgress.get(directoryId);
        const item = items.value.find(i => i.id === directoryId);

        if (progress && item && item.isDirectory) {
            progress.completedFiles++;
            item.completedFiles = progress.completedFiles;

            if (progress.completedFiles >= progress.totalFiles) {
                item.status = 'completed';
                item.progress = 100;
                item.transferred = progress.totalSize;
            }
        }
    }

    // 状态一致性验证函数
    function validateStateConsistency() {
        // 检查 directoryProgress 和 items 的一致性
        const directoryItems = items.value.filter(i => i.isDirectory);

        for (const item of directoryItems) {
            if (!directoryProgress.has(item.id) && item.status === 'running') {
                // 重建丢失的进度跟踪
                directoryProgress.set(item.id, {
                    totalFiles: item.childFiles || 0,
                    completedFiles: item.completedFiles || 0,
                    totalSize: item.size,
                    transferredSize: item.transferred,
                    isPaused: false,
                    pausedFiles: new Set()
                });
            }
        }
    }

    async function startTransfer(tempId: string) {
        const item = items.value.find(i => i.id === tempId);
        if (!item) return;

        if (item.status === 'completed') return;

        item.status = 'running';
        item.error = undefined;
        active.value = true;

        try {
            // let realId: string; // Not needed

            if (item.type === 'upload') {
                await invoke('upload_file', {
                    id: item.sessionId,
                    localPath: item.localPath,
                    remotePath: item.remotePath,
                    transferId: item.id // Pass generated ID
                });
            } else {
                await invoke('download_file', {
                    id: item.sessionId,
                    remotePath: item.remotePath,
                    localPath: item.localPath,
                    transferId: item.id // Pass generated ID
                });
            }

            // Update the temporary ID with the real backend ID
            // item.id = realId; // No longer needed as we use the generated ID
            item.isTemp = false;

            // Check if it was cancelled during start
            if ((item.status as string) === 'cancelled') {
                await invoke('cancel_transfer', { transferId: item.id });
                return;
            }

        } catch (e: any) {
            if ((item.status as string) === 'cancelled') return;

            console.error(e);
            item.status = 'error';
            item.error = e.toString();
        } finally {
            processQueue();
            active.value = items.value.some(i => i.status === 'running');
        }
    }

    async function pauseTransfer(id: string) {
        const item = items.value.find(i => i.id === id);
        if (!item) return;

        if (item.isDirectory) {
            // ... directory logic (unchanged mostly, but ensure cancellation hits backend)
            const progress = directoryProgress.get(id);
            if (progress) {
                progress.isPaused = true;
                item.status = 'paused';
                const runningChildFiles = items.value.filter(i =>
                    !i.isDirectory &&
                    i.status === 'running' &&
                    i.remotePath && i.remotePath.startsWith(item.remotePath!)
                );
                for (const childFile of runningChildFiles) {
                    try {
                        cancelTransfer(childFile.id); // This will invoke cancel_transfer
                        childFile.status = 'paused';
                        progress.pausedFiles.add(childFile.id);
                    } catch (e) {
                        // ignore
                    }
                }
            }
            return;
        }

        if (item.status !== 'running') return;

        try {
            await invoke('cancel_transfer', { transferId: id });
            item.status = 'paused';
        } catch (e) {
            console.error("Failed to pause", e);
        }
    }

    function resumeTransfer(id: string) {
        const item = items.value.find(i => i.id === id);
        if (!item) return;

        if (item.isDirectory) {
            // ... directory logic
            const progress = directoryProgress.get(id);
            if (progress && progress.isPaused) {
                progress.isPaused = false;
                item.status = 'running';
                for (const childFileId of progress.pausedFiles) {
                    // For child files, they should be simple transfers.
                    // We need to re-submit them as new transfers because backend doesn't support resumeID yet?
                    // Or if backend cancel just stops it, we need to restart.
                    // Backend 'resume' param is ignored currently.
                    // The simplest is to treat them as new transfers?
                    // But we want to 'resume' from offset. Backend download_file accepts offset? 
                    // No, download_file overwrites or appends? Code says 'fs::create' so OVERWRITE.
                    // Backend does NOT support resume yet.
                    // So we just restart.

                    // We need to queue them again.
                    const child = items.value.find(c => c.id === childFileId);
                    if (child) {
                        child.status = 'pending';
                    }
                }
                progress.pausedFiles.clear();
                processQueue();
            }
            return;
        }

        // Single file resume
        // Create new transfer actually, as backend doesn't support resume state persistence fully
        // But we want to reuse the item?
        // We can set status to pending and processQueue will pick it up?
        item.status = 'pending';
        processQueue();
    }

    async function cancelTransfer(id: string) {
        const item = items.value.find(i => i.id === id);
        if (!item) return;

        if (item.isDirectory) {
            // ... directory logic
            const progress = directoryProgress.get(id);
            if (progress) {
                progress.isPaused = false;
                item.status = 'cancelled';
                const childFiles = items.value.filter(i =>
                    !i.isDirectory &&
                    i.remotePath && i.remotePath.startsWith(item.remotePath!)
                );
                for (const childFile of childFiles) {
                    if (childFile.status === 'running') {
                        invoke('cancel_transfer', { transferId: childFile.id }).catch(() => { });
                    }
                    childFile.status = 'cancelled';
                }
                progress.pausedFiles.clear();
            }
            return;
        }

        if (item.status === 'running' || item.status === 'paused') {
            await invoke('cancel_transfer', { transferId: id });
        }
        item.status = 'cancelled';
    }

    function clearHistory(sessionId?: string) {
        items.value = items.value.filter(i => {
            if (sessionId && i.sessionId !== sessionId) return true; // Keep items from other sessions
            return ['running', 'pending', 'paused'].includes(i.status);
        });
    }

    async function batchPause(sessionId?: string) {
        const runningItems = items.value.filter(i => {
            if (sessionId && i.sessionId !== sessionId) return false;
            return i.status === 'running';
        });
        await Promise.all(runningItems.map(item => pauseTransfer(item.id)));
    }

    function batchResume(sessionId?: string) {
        const pausedItems = items.value.filter(i => {
            if (sessionId && i.sessionId !== sessionId) return false;
            return ['paused', 'error', 'cancelled'].includes(i.status);
        });
        pausedItems.forEach(item => resumeTransfer(item.id));
    }

    async function batchCancel(sessionId?: string) {
        const activeItems = items.value.filter(i => {
            if (sessionId && i.sessionId !== sessionId) return false;
            return ['running', 'paused', 'pending'].includes(i.status);
        });
        await Promise.all(activeItems.map(item => cancelTransfer(item.id)));
    }

    async function batchDelete(sessionId?: string) {
        const deletableItems = items.value.filter(i => {
            if (sessionId && i.sessionId !== sessionId) return false;
            return ['completed', 'cancelled', 'error', 'paused'].includes(i.status);
        });
        await Promise.all(deletableItems.map(item => removeTransfer(item.id)));
    }

    async function removeTransfer(id: string) {
        await invoke('remove_transfer', { id });
        const idx = items.value.findIndex(i => i.id === id);
        if (idx !== -1) items.value.splice(idx, 1);
    }

    return {
        items,
        addTransfer,
        addDirectoryTransfer,
        updateDirectoryStats,
        incrementDirectoryCompleted,
        pauseTransfer,
        resumeTransfer,
        cancelTransfer,
        clearHistory,
        batchPause,
        batchResume,
        batchCancel,
        batchDelete,
        removeTransfer,
        initListeners,
        processQueue,
        syncWithBackend
    };
});
