<script setup lang="ts">
import { ref, onMounted, onUnmounted, computed, watch, nextTick, shallowRef, triggerRef } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { ArrowUp, RefreshCw, Upload, FilePlus, FolderPlus, Briefcase, Copy, Terminal as TerminalIcon } from 'lucide-vue-next';
import { open, save, ask } from '@tauri-apps/plugin-dialog';
import { readDir, mkdir, stat } from '@tauri-apps/plugin-fs';
import type { FileEntry, FileManagerViewMode } from '../types';
import { useSessionStore } from '../stores/sessions'; // Import session store
import { useNotificationStore } from '../stores/notifications';
import { useTransferStore } from '../stores/transfers';
import { useSettingsStore } from '../stores/settings';
import TransferList from './TransferList.vue';
import VirtualFileList from './VirtualFileList.vue';
import { useI18n } from '../composables/useI18n';
import { getPathUtils } from '../composables/usePath';
import { parseFileError, getErrorMessage } from '../composables/useFileError';
// import draggable from 'vuedraggable'; // Removed

type ColumnKey = 'name' | 'size' | 'date' | 'owner';

interface TreeNode {
    entry: FileEntry;
    path: string;
    depth: number;
    parentPath: string | null;
    childrenLoaded: boolean;
    loading: boolean;
}

function showTreeContextMenu(e: MouseEvent, node: TreeNode) {
    e.preventDefault();

    const next = new Set(selectedTreePaths.value);
    if (!next.has(node.path)) {
        next.clear();
        next.add(node.path);
        selectedTreePaths.value = next;
    }

    const { x, y } = calculateContextMenuPosition(e.clientX, e.clientY);
    contextMenu.value = { show: true, x, y, file: node.entry, treePath: node.path, isTree: true, isBackground: false };
    updateContextMenuPosition();
}

function calculateContextMenuPosition(clientX: number, clientY: number) {
    // Return raw coordinates and let updateContextMenuPosition handle boundaries
    // after the menu is rendered and we know its actual size
    return { x: clientX, y: clientY };
}

async function updateContextMenuPosition() {
    await nextTick();
    if (!contextMenuRef.value) return;

    const rect = contextMenuRef.value.getBoundingClientRect();
    let { x, y } = contextMenu.value;

    // Adjust if overflowing
    if (rect.bottom > window.innerHeight) {
        y = window.innerHeight - rect.height - 10; // 10px padding
    }
    if (rect.right > window.innerWidth) {
        x = window.innerWidth - rect.width - 10;
    }

    // Ensure not negative (top/left)
    x = Math.max(0, x);
    y = Math.max(0, y);

    contextMenu.value.x = x;
    contextMenu.value.y = y;
}

function handleContainerContextMenu(e: MouseEvent) {
    // 妫€鏌ョ偣鍑荤殑鏄惁鏄儗鏅尯鍩燂紙涓嶆槸鏂囦欢椤癸級
    const target = e.target as HTMLElement;
    const fileItem = target.closest('[data-file-item]');

    if (!fileItem) {
        // 鐐瑰嚮鐨勬槸鑳屾櫙鍖哄煙
        e.preventDefault();
        showBackgroundContextMenu(e);
    }
    // 濡傛灉鐐瑰嚮鐨勬槸鏂囦欢椤癸紝璁╂枃浠堕」鑷繁澶勭悊鍙抽敭鑿滃崟
}

function showBackgroundContextMenu(e: MouseEvent) {
    e.preventDefault();
    // If clicking on background, clear file selection unless Ctrl/Shift is held (handled by other listeners? usually background click clears selection)
    // For now, just show menu.
    // Maybe clear selection?
    if (!e.ctrlKey && !e.shiftKey) {
        selectedFiles.value.clear();
        selectedTreePaths.value.clear();
    }

    const { x, y } = calculateContextMenuPosition(e.clientX, e.clientY);
    contextMenu.value = { show: true, x, y, file: null, treePath: null, isTree: viewMode.value === 'tree', isBackground: true };
    updateContextMenuPosition();
}

const props = defineProps<{ sessionId: string }>();
const emit = defineEmits<{
    (e: 'openFileEditor', filePath: string, fileName: string): void;
    (e: 'switchToTerminalPath', sessionId: string, path: string): void;
}>();
const { t } = useI18n();
const settingsStore = useSettingsStore();
const sessionStore = useSessionStore(); // Init session store
const pathUtils = computed(() => {
    const session = sessionStore.sessions.find(s => s.id === props.sessionId);
    return getPathUtils(session?.os);
});
const notificationStore = useNotificationStore();
const viewMode = computed<FileManagerViewMode>(() => settingsStore.fileManager.viewMode);
const currentPath = ref('.');
const files = shallowRef<FileEntry[]>([]);
const contextMenu = ref<{ show: boolean, x: number, y: number, file: FileEntry | null, treePath: string | null, isTree: boolean, isBackground: boolean }>({ show: false, x: 0, y: 0, file: null, treePath: null, isTree: false, isBackground: false });
const contextMenuRef = ref<HTMLElement | null>(null);
const isEditingPath = ref(false);
const pathInput = ref('');
const renamingPath = ref<string | null>(null);
const renameInput = ref('');
const renamingType = ref<'rename' | 'create_file' | 'create_folder'>('rename');
const isConfirmingRename = ref(false);
const containerRef = ref<HTMLElement | null>(null);
const selectedFiles = ref<Set<string>>(new Set());
const lastSelectedIndex = ref<number>(-1);
const transferStore = useTransferStore();
const treeRootPath = ref<string>('.');
const treeNodes = shallowRef<Map<string, TreeNode>>(new Map());
const childrenMap = shallowRef<Map<string | null, string[]>>(new Map());
const expandedPaths = ref<Set<string>>(new Set());
const selectedTreePaths = ref<Set<string>>(new Set());
const columnWidths = ref<Record<ColumnKey, number>>({
    name: 260,
    size: 100,
    date: 200,
    owner: 120,
});
const resizingColumn = ref<ColumnKey | null>(null);
const resizeStartX = ref(0);
const resizeStartWidth = ref(0);

const isOpeningFile = ref(false);
const unlistenDrop = ref<UnlistenFn | null>(null);

// Path suggestions
const suggestions = ref<string[]>([]);
const showSuggestions = ref(false);

// 防抖和取消机制
let loadFilesAbortController: AbortController | null = null;
let loadFilesDebounceTimer: ReturnType<typeof setTimeout> | null = null;
const isLoadingFiles = ref(false);
const activeSuggestionIndex = ref(-1);
let debounceTimer: ReturnType<typeof setTimeout> | null = null;

function handlePathInput() {
    if (debounceTimer) clearTimeout(debounceTimer);
    debounceTimer = setTimeout(() => {
        fetchSuggestions(pathInput.value);
    }, 300);
}

async function fetchSuggestions(input: string) {
    if (!input) {
        suggestions.value = [];
        showSuggestions.value = false;
        return;
    }

    let parentPath: string;
    let filterPrefix: string;

    // Normalize separators to / for consistency in logic, though pathUtils handles it
    const normalizedInput = input.replace(/\\/g, '/');

    if (normalizedInput.endsWith('/')) {
        parentPath = normalizedInput;
        filterPrefix = '';
    } else {
        // Use pathUtils to get dirname, but we need to be careful with partial paths
        // If input is "/var/lo", dirname is "/var"
        const lastSlashIndex = normalizedInput.lastIndexOf('/');
        if (lastSlashIndex === -1) {
            parentPath = '.'; // Or root?
            filterPrefix = normalizedInput;
        } else if (lastSlashIndex === 0) {
            parentPath = '/';
            filterPrefix = normalizedInput.substring(1);
        } else {
            parentPath = normalizedInput.substring(0, lastSlashIndex);
            filterPrefix = normalizedInput.substring(lastSlashIndex + 1);
        }
    }

    try {
        // If parentPath is empty, default to / or .
        const searchPath = parentPath || '/';
        const entries = await invoke<FileEntry[]>('list_files', { id: props.sessionId, path: searchPath });

        suggestions.value = entries
            .filter(e => e.isDir && e.name.startsWith(filterPrefix))
            .map(e => {
                const fullPath = searchPath.endsWith('/')
                    ? `${searchPath}${e.name}`
                    : `${searchPath}/${e.name}`;
                return fullPath;
            });

        showSuggestions.value = suggestions.value.length > 0;
        activeSuggestionIndex.value = -1;
    } catch (e) {
        // console.error("Failed to fetch suggestions", e);
        suggestions.value = [];
        showSuggestions.value = false;
    }
}

function selectSuggestion(path: string) {
    pathInput.value = path;
    showSuggestions.value = false;
    // Focus back to input if needed, but we probably want to let user continue typing or press enter
    // Maybe add a trailing slash if it's a directory (which it is)
    if (!pathInput.value.endsWith('/')) {
        pathInput.value += '/';
    }
    // Trigger fetch again for the new path
    fetchSuggestions(pathInput.value);
}

function handlePathInputKeydown(e: KeyboardEvent) {
    if (showSuggestions.value && suggestions.value.length > 0) {
        if (e.key === 'ArrowDown') {
            e.preventDefault();
            activeSuggestionIndex.value = (activeSuggestionIndex.value + 1) % suggestions.value.length;
            // Scroll into view if needed
            nextTick(() => {
                const list = document.getElementById('path-suggestions-list');
                const item = list?.children[activeSuggestionIndex.value] as HTMLElement;
                if (item && list) {
                    if (item.offsetTop + item.offsetHeight > list.scrollTop + list.offsetHeight) {
                        list.scrollTop = item.offsetTop + item.offsetHeight - list.offsetHeight;
                    } else if (item.offsetTop < list.scrollTop) {
                        list.scrollTop = item.offsetTop;
                    }
                }
            });
            return;
        } else if (e.key === 'ArrowUp') {
            e.preventDefault();
            activeSuggestionIndex.value = (activeSuggestionIndex.value - 1 + suggestions.value.length) % suggestions.value.length;
            nextTick(() => {
                const list = document.getElementById('path-suggestions-list');
                const item = list?.children[activeSuggestionIndex.value] as HTMLElement;
                if (item && list) {
                    if (item.offsetTop < list.scrollTop) {
                        list.scrollTop = item.offsetTop;
                    } else if (item.offsetTop + item.offsetHeight > list.scrollTop + list.offsetHeight) {
                        list.scrollTop = item.offsetTop + item.offsetHeight - list.offsetHeight;
                    }
                }
            });
            return;
        } else if (e.key === 'Enter') {
            if (activeSuggestionIndex.value !== -1) {
                e.preventDefault();
                selectSuggestion(suggestions.value[activeSuggestionIndex.value]);
                return;
            }
        } else if (e.key === 'Escape') {
            showSuggestions.value = false;
            return;
        } else if (e.key === 'Tab') {
            if (activeSuggestionIndex.value !== -1) {
                e.preventDefault();
                selectSuggestion(suggestions.value[activeSuggestionIndex.value]);
            } else if (suggestions.value.length > 0) {
                // Auto-complete first suggestion if tab pressed
                e.preventDefault();
                selectSuggestion(suggestions.value[0]);
            }
            return;
        }
    }

    if (e.key === 'Enter') {
        handlePathSubmit();
        showSuggestions.value = false;
    }
}

function handlePathInputBlur() {
    // Delay hiding to allow click event on suggestion to fire
    setTimeout(() => {
        showSuggestions.value = false;
    }, 200);
}


const visibleTreeNodes = computed<TreeNode[]>(() => {
    const result: TreeNode[] = [];
    const nodes = treeNodes.value;
    const children = childrenMap.value;

    const collectChildren = (parentPath: string | null, depth: number) => {
        const childPaths = children.get(parentPath) || [];
        const childNodes: TreeNode[] = [];

        for (const childPath of childPaths) {
            const node = nodes.get(childPath);
            if (node) {
                childNodes.push({ ...node, depth });
            }
        }

        childNodes.sort((a, b) => {
            if (a.entry.isDir === b.entry.isDir) {
                return a.entry.name.localeCompare(b.entry.name);
            }
            return a.entry.isDir ? -1 : 1;
        });

        for (const child of childNodes) {
            result.push(child);
            if (child.entry.isDir && expandedPaths.value.has(child.path)) {
                collectChildren(child.path, depth + 1);
            }
        }
    };

    collectChildren(treeRootPath.value === '.' ? null : treeRootPath.value, 0);
    return result;
});

function onDragStart(event: DragEvent, element: FileEntry | TreeNode) {
    let entry: FileEntry;
    let path: string;

    if ('entry' in element) { // TreeNode
        entry = (element as TreeNode).entry;
        path = (element as TreeNode).path;
    } else { // FileEntry
        entry = element as FileEntry;
        path = pathUtils.value.join(currentPath.value, entry.name);
    }

    const data = {
        path: path,
        isDir: entry.isDir
    };

    if (event.dataTransfer) {
        event.dataTransfer.setData('application/json', JSON.stringify(data));
        event.dataTransfer.effectAllowed = 'copy';
    }
}

async function loadFiles(path: string) {
    // 取消之前的防抖定时器
    if (loadFilesDebounceTimer) {
        clearTimeout(loadFilesDebounceTimer);
        loadFilesDebounceTimer = null;
    }

    // 防抖处理：100ms 内的重复调用将被合并
    loadFilesDebounceTimer = setTimeout(async () => {
        // 取消之前进行中的请求
        if (loadFilesAbortController) {
            loadFilesAbortController.abort();
        }

        loadFilesAbortController = new AbortController();
        const currentController = loadFilesAbortController;
        isLoadingFiles.value = true;

        try {
            console.log('Loading files for path:', path);
            const loadedFiles = await invoke<FileEntry[]>('list_files', { id: props.sessionId, path });

            // 检查请求是否已被取消
            if (currentController.signal.aborted) {
                return;
            }

            // Add parent directory entry ".." when not in root
            const filesWithParent = path !== '.' ? [{
                name: '..',
                size: 0,
                mtime: 0,
                isDir: true,
                permissions: 755,
                uid: 0,
                owner: '-'
            }, ...loadedFiles] : loadedFiles;

            files.value = filesWithParent;
            triggerRef(files);
            currentPath.value = path;
            // Display actual path instead of "."
            pathInput.value = path === '.' ? '/' : path;
            console.log('Set currentPath to:', path, 'displayed as:', pathInput.value);
            selectedFiles.value.clear();
            lastSelectedIndex.value = -1;

            if (viewMode.value === 'tree') {
                treeRootPath.value = path;
                treeNodes.value = new Map();
                childrenMap.value = new Map();
                expandedPaths.value = new Set();
                selectedTreePaths.value = new Set();
                const parentPath = path === '.' ? null : path;
                const newChildrenMap = new Map<string | null, string[]>();
                const childPaths: string[] = [];

                // Add parent directory entry ".." when not in root
                if (path !== '/' && path !== '.') {
                    const parentEntry: FileEntry = {
                        name: '..',
                        size: 0,
                        mtime: 0,
                        isDir: true,
                        permissions: 755,
                        uid: 0,
                        owner: '-'
                    };
                    const parentDirPath = pathUtils.value.dirname(path);
                    treeNodes.value.set(parentDirPath, {
                        entry: parentEntry,
                        path: parentDirPath,
                        depth: 0,
                        parentPath,
                        childrenLoaded: false,
                        loading: false,
                    });
                    childPaths.push(parentDirPath);
                }

                for (const entry of files.value) {
                    // Skip the ".." entry as it's already added above
                    if (entry.name === '..') continue;

                    const fullPath = pathUtils.value.join(path, entry.name);
                    treeNodes.value.set(fullPath, {
                        entry,
                        path: fullPath,
                        depth: 0,
                        parentPath,
                        childrenLoaded: false,
                        loading: false,
                    });
                    childPaths.push(fullPath);
                }

                newChildrenMap.set(parentPath, childPaths);
                childrenMap.value = newChildrenMap;
                triggerRef(treeNodes);
                triggerRef(childrenMap);
            }
        } catch (e) {
            // 忽略取消错误
            if (currentController.signal.aborted) {
                return;
            }
            console.error(e);
            const fileError = parseFileError(e);
            const errorMsg = getErrorMessage(fileError, t);
            notificationStore.error(`${t('fileManager.loadError') || 'Failed to load directory'}: ${errorMsg}`);
            files.value = [];
            triggerRef(files);
        } finally {
            if (!currentController.signal.aborted) {
                isLoadingFiles.value = false;
            }
        }
    }, 100);
}


async function toggleDirectory(node: TreeNode) {
    if (!node.entry.isDir) {
        await openTreeFile(node);
        return;
    }

    // Handle ".." parent directory
    if (node.entry.name === '..') {
        goUp();
        return;
    }

    const isExpanded = expandedPaths.value.has(node.path);
    if (isExpanded) {
        expandedPaths.value.delete(node.path);
        return;
    }

    expandedPaths.value.add(node.path);

    if (node.childrenLoaded || node.loading) {
        return;
    }

    const existing = treeNodes.value.get(node.path);
    if (!existing) return;
    existing.loading = true;
    treeNodes.value.set(node.path, existing);
    triggerRef(treeNodes);

    try {
        const children = await invoke<FileEntry[]>('list_files', { id: props.sessionId, path: node.path });
        const currentChildrenMap = new Map(childrenMap.value);
        const childPaths: string[] = [];

        for (const child of children) {
            const childPath = pathUtils.value.join(node.path, child.name);
            if (!treeNodes.value.has(childPath)) {
                treeNodes.value.set(childPath, {
                    entry: child,
                    path: childPath,
                    depth: node.depth + 1,
                    parentPath: node.path,
                    childrenLoaded: false,
                    loading: false,
                });
            }
            childPaths.push(childPath);
        }

        currentChildrenMap.set(node.path, childPaths);
        childrenMap.value = currentChildrenMap;
        existing.childrenLoaded = true;
        treeNodes.value.set(node.path, existing);
        triggerRef(treeNodes);
        triggerRef(childrenMap);
    } catch (e) {
        console.error(e);
        notificationStore.error(`${t('fileManager.treeLoadError') || 'Failed to load tree directory'}: ${getErrorMessage(parseFileError(e), t)}`);
    } finally {
        const updated = treeNodes.value.get(node.path);
        if (updated) {
            updated.loading = false;
            treeNodes.value.set(node.path, updated);
            triggerRef(treeNodes);
        }
    }
}

async function openTreeFile(node: TreeNode) {
    if (node.entry.isDir) {
        await toggleDirectory(node);
        return;
    }

    // Edit remote file - emit event to open in terminal tab area
    emit('openFileEditor', node.path, node.entry.name);
}

function handleTreeSelection(node: TreeNode) {
    closeContextMenu();
    const next = new Set(selectedTreePaths.value);
    if (next.has(node.path)) {
        next.delete(node.path);
    } else {
        next.clear();
        next.add(node.path);
    }
    selectedTreePaths.value = next;
}

function startResize(column: ColumnKey, event: MouseEvent) {
    resizingColumn.value = column;
    resizeStartX.value = event.clientX;
    resizeStartWidth.value = columnWidths.value[column];
}

let resizeAnimationFrame: number | null = null;

function handleMouseMove(event: MouseEvent) {
    if (!resizingColumn.value) return;

    if (!resizeAnimationFrame) {
        resizeAnimationFrame = requestAnimationFrame(() => {
            const delta = event.clientX - resizeStartX.value;
            const newWidth = Math.max(80, resizeStartWidth.value + delta);
            columnWidths.value[resizingColumn.value!] = newWidth;
            resizeAnimationFrame = null;
        });
    }
}

function stopResize() {
    resizingColumn.value = null;
}

// Handle native browser drag and drop for files
function handleNativeDragOver(event: DragEvent) {
    event.preventDefault();
    event.stopPropagation();

    // Only accept file drops, not connection drags
    if (event.dataTransfer && event.dataTransfer.types.includes('Files')) {
        event.dataTransfer.dropEffect = 'copy';
    }
}

function handleNativeDrop(event: DragEvent) {
    event.preventDefault();
    event.stopPropagation();
    // Handled by tauri://drag-drop listener
}

async function handleTauriFileDrop(paths: string[]) {
    for (const fullPath of paths) {
        const name = fullPath.split(/[\\/]/).pop() || 'uploaded';
        const remotePath = pathUtils.value.join(currentPath.value, name);

        try {
            // Check if it's a directory
            const fileStat = await stat(fullPath);

            if (fileStat.isDirectory) {
                try {
                    await invoke('create_directory', { id: props.sessionId, path: remotePath });
                } catch (e) {
                    // Ignore if directory already exists
                }
                await processDirectory(fullPath, remotePath);
            } else {
                const transferId = typeof crypto !== 'undefined' && crypto.randomUUID ? crypto.randomUUID() : Math.random().toString(36).substring(2);

                transferStore.addTransfer({
                    id: transferId,
                    type: 'upload',
                    name,
                    localPath: fullPath,
                    remotePath,
                    size: fileStat.size,
                    transferred: 0,
                    progress: 0,
                    status: 'pending',
                    sessionId: props.sessionId
                });
            }
        } catch (e) {
            console.error('Error processing dropped item:', name, e);
            notificationStore.error(`Failed to process ${name}: ${e}`);
        }
    }

    // loadFiles(currentPath.value); // Removed to prevent lock contention
}

onMounted(async () => {
    // Get the actual working directory for the user
    try {
        const workingDir = await invoke<string>('get_working_directory', { id: props.sessionId });
        loadFiles(workingDir || '/'); // Fallback to '/' if working directory is not available
    } catch (e) {
        console.error('Failed to get working directory, using root:', e);
        loadFiles('/'); // Fallback to root if there's an error
    }
    transferStore.initListeners();
    window.addEventListener('mousemove', handleMouseMove);
    window.addEventListener('mouseup', stopResize);
    window.addEventListener('keydown', handleKeyDown);

    unlistenDrop.value = await listen('tauri://drag-drop', (event) => {
        const payload = event.payload as { paths: string[], position: { x: number, y: number } };
        if (containerRef.value) {
            const rect = containerRef.value.getBoundingClientRect();
            // Check if drop is within the file manager container
            if (payload.position.x >= rect.left &&
                payload.position.x <= rect.right &&
                payload.position.y >= rect.top &&
                payload.position.y <= rect.bottom) {
                handleTauriFileDrop(payload.paths);
            }
        }
    });
});

onUnmounted(() => {
    window.removeEventListener('mousemove', handleMouseMove);
    window.removeEventListener('mouseup', stopResize);
    window.removeEventListener('keydown', handleKeyDown);
    if (unlistenDrop.value) {
        unlistenDrop.value();
    }
    // 清理防抖定时器
    if (loadFilesDebounceTimer) {
        clearTimeout(loadFilesDebounceTimer);
        loadFilesDebounceTimer = null;
    }
    // 取消进行中的请求
    if (loadFilesAbortController) {
        loadFilesAbortController.abort();
        loadFilesAbortController = null;
    }
});

watch(viewMode, (mode) => {
    if (mode === 'tree') {
        loadFiles(currentPath.value);
    }
});

async function navigate(entry: FileEntry) {
    if (entry.isDir) {
        if (entry.name === '..') {
            // Go to parent directory
            goUp();
        } else {
            // Calculate the correct path for navigation
            const newPath = pathUtils.value.join(currentPath.value, entry.name);
            loadFiles(newPath);
        }
    } else {
        // Edit remote file - emit event to open in terminal tab area
        const filePath = pathUtils.value.join(currentPath.value, entry.name);
        emit('openFileEditor', filePath, entry.name);
    }
}

function goUp() {
    if (currentPath.value === '/' || currentPath.value === '.') {
        return;
    }

    const parentPath = pathUtils.value.dirname(currentPath.value);
    console.log('Going up from', currentPath.value, 'to', parentPath);
    loadFiles(parentPath);
}

function refresh() {
    loadFiles(currentPath.value);
}

function handlePathSubmit() {
    if (pathInput.value) {
        // If input is empty, set to root path
        let targetPath = pathInput.value.trim();
        if (targetPath === '') {
            targetPath = '/';
            pathInput.value = '/';
        }

        // Ensure it starts with /
        if (!targetPath.startsWith('/')) {
            targetPath = '/' + targetPath;
        }

        if (targetPath !== currentPath.value) {
            loadFiles(targetPath);
        }
    }
    isEditingPath.value = false;
}

function handleSelection(event: MouseEvent, file: FileEntry, index: number) {
    closeContextMenu();
    if (event.ctrlKey || event.metaKey) {
        // Toggle selection
        if (selectedFiles.value.has(file.name)) {
            selectedFiles.value.delete(file.name);
        } else {
            selectedFiles.value.add(file.name);
            lastSelectedIndex.value = index;
        }
    } else if (event.shiftKey && lastSelectedIndex.value !== -1) {
        // Range selection
        const start = Math.min(lastSelectedIndex.value, index);
        const end = Math.max(lastSelectedIndex.value, index);
        selectedFiles.value.clear();
        for (let i = start; i <= end; i++) {
            if (files.value[i]) {
                selectedFiles.value.add(files.value[i].name);
            }
        }
    } else {
        // Single selection
        selectedFiles.value.clear();
        selectedFiles.value.add(file.name);
        lastSelectedIndex.value = index;
    }
}

function showContextMenu(e: MouseEvent, file: FileEntry) {
    e.preventDefault();
    // If the file is not in selection, select it exclusively
    if (!selectedFiles.value.has(file.name)) {
        selectedFiles.value.clear();
        selectedFiles.value.add(file.name);
        const idx = files.value.findIndex(f => f.name === file.name);
        if (idx !== -1) lastSelectedIndex.value = idx;
    }

    const { x, y } = calculateContextMenuPosition(e.clientX, e.clientY);
    contextMenu.value = { show: true, x, y, file, treePath: null, isTree: false, isBackground: false };
    updateContextMenuPosition();
}

function closeContextMenu() {
    contextMenu.value.show = false;
}

async function handleUpload() {
    try {
        const selected = await open({
            multiple: false,
            title: 'Select file to upload'
        });
        if (selected && typeof selected === 'string') {
            const name = selected.split(/[\\/]/).pop() || 'uploaded_file';
            const remotePath = pathUtils.value.join(currentPath.value, name);

            const transferId = typeof crypto !== 'undefined' && crypto.randomUUID ? crypto.randomUUID() : Math.random().toString(36).substring(2);

            transferStore.addTransfer({
                id: transferId,
                type: 'upload',
                name,
                localPath: selected,
                remotePath,
                size: 0,
                transferred: 0,
                progress: 0,
                status: 'pending',
                sessionId: props.sessionId
            });

            // loadFiles(currentPath.value);
        }
    } catch (e) {
        console.error(e);
        notificationStore.error("Upload failed: " + e);
    }
}

async function processDirectory(localPath: string, remoteBasePath: string) {
    const entries = await readDir(localPath);
    for (const entry of entries) {
        const sep = localPath.includes('\\') ? '\\' : '/';
        const fullLocalPath = localPath.endsWith(sep) ? `${localPath}${entry.name}` : `${localPath}${sep}${entry.name}`;
        const fullRemotePath = pathUtils.value.join(remoteBasePath, entry.name);

        if (entry.isDirectory) {
            try {
                await invoke('create_directory', { id: props.sessionId, path: fullRemotePath });
            } catch (e) {
                // Ignore if directory already exists
            }
            await processDirectory(fullLocalPath, fullRemotePath);
        } else {
            const transferId = typeof crypto !== 'undefined' && crypto.randomUUID ? crypto.randomUUID() : Math.random().toString(36).substring(2);
            transferStore.addTransfer({
                id: transferId,
                type: 'upload',
                name: entry.name,
                localPath: fullLocalPath,
                remotePath: fullRemotePath,
                size: 0, // Backend will calculate
                transferred: 0,
                progress: 0,
                status: 'pending',
                sessionId: props.sessionId
            });
        }
    }
}

async function handleUploadDirectory() {
    try {
        const selected = await open({
            directory: true,
            multiple: false,
            title: 'Select directory to upload'
        });

        if (selected && typeof selected === 'string') {
            const name = selected.split(/[\\/]/).pop() || 'uploaded_dir';
            const remotePath = pathUtils.value.join(currentPath.value, name);

            try {
                await invoke('create_directory', { id: props.sessionId, path: remotePath });
            } catch (e) {
                // Ignore
            }

            await processDirectory(selected, remotePath);
            // loadFiles(currentPath.value);
        }
    } catch (e) {
        console.error(e);
        notificationStore.error("Upload directory failed: " + e);
    }
}

async function handleSetWorkspace() {
    let path = '';

    if (contextMenu.value.isBackground) {
        path = currentPath.value;
    } else if (contextMenu.value.file?.isDir) {
        path = contextMenu.value.isTree && contextMenu.value.treePath
            ? contextMenu.value.treePath
            : pathUtils.value.join(currentPath.value, contextMenu.value.file.name);
    } else {
        return;
    }

    try {
        await sessionStore.setSessionWorkspace(props.sessionId, path);
        useNotificationStore().success(`Workspace set to: ${path}`);
        // Switch to AI tab?
        sessionStore.setActiveTab('ai');
        //await message(`Workspace set to: ${path}`, { title: 'Success', kind: 'info' });

    } catch (e) {
        notificationStore.error(`Failed to set workspace: ${e}`);
    }
    closeContextMenu();
}

async function downloadDirectory(remoteDirPath: string, localDirPath: string, sessionId: string) {
    try {
        // Create local directory if it doesn't exist
        try {
            await mkdir(localDirPath, { recursive: true });
        } catch (e) {
            // Directory might already exist, continue
            console.log('Directory might already exist:', e);
        }

        // 鍒涘缓鐩綍浼犺緭椤?
        const directoryTransferId = transferStore.addDirectoryTransfer(remoteDirPath, localDirPath, sessionId);

        // 鎵弿鐩綍骞惰绠楃粺璁′俊鎭?
        const { totalFiles, totalSize } = await calculateDirectoryStats(remoteDirPath, sessionId);
        transferStore.updateDirectoryStats(directoryTransferId, totalFiles, totalSize);

        // 寮€濮嬩紶杈?
        const directoryItem = transferStore.items.find(item => item.id === directoryTransferId);
        if (directoryItem) {
            directoryItem.status = 'running';
        }

        // 閫掑綊涓嬭浇鎵€鏈夋枃浠?
        await downloadDirectoryRecursive(remoteDirPath, localDirPath, sessionId, directoryTransferId);

        // 鏍囪鐩綍浼犺緭涓哄畬鎴?
        if (directoryItem) {
            directoryItem.status = 'completed';
            directoryItem.progress = 100;
        }
    } catch (e) {
        console.error(`Failed to download directory ${remoteDirPath}:`, e);
        // 鏍囪鐩綍浼犺緭涓哄け璐?
        const directoryItem = transferStore.items.find(item => item.remotePath === remoteDirPath && item.isDirectory);
        if (directoryItem) {
            directoryItem.status = 'error';
            directoryItem.error = (e as Error).toString();
        }
        throw e;
    }
}

async function calculateDirectoryStats(remotePath: string, sessionId: string): Promise<{ totalFiles: number, totalSize: number }> {
    let totalFiles = 0;
    let totalSize = 0;

    async function scanDirectory(path: string) {
        const entries = await invoke<FileEntry[]>('list_files', { id: sessionId, path });

        for (const entry of entries) {
            const fullPath = pathUtils.value.join(path, entry.name);

            if (entry.isDir) {
                await scanDirectory(fullPath);
            } else {
                totalFiles++;
                totalSize += entry.size;
            }
        }
    }

    await scanDirectory(remotePath);
    return { totalFiles, totalSize };
}

async function downloadDirectoryRecursive(remoteDirPath: string, localDirPath: string, sessionId: string, directoryTransferId: string) {
    try {
        // 妫€鏌ョ洰褰曟槸鍚﹁鏆傚仠
        const directoryItem = transferStore.items.find(item => item.id === directoryTransferId);
        if (directoryItem && directoryItem.status === 'paused') {
            // 绛夊緟鎭㈠
            await waitForDirectoryResume(directoryTransferId);
        }

        if (directoryItem && (directoryItem.status === 'cancelled' || directoryItem.status === 'error')) {
            return; // 鍋滄涓嬭浇
        }

        // List remote directory contents
        const entries = await invoke<FileEntry[]>('list_files', { id: sessionId, path: remoteDirPath });

        for (const entry of entries) {
            // 鍐嶆妫€鏌ョ姸鎬?
            const currentDirectoryItem = transferStore.items.find(item => item.id === directoryTransferId);
            if (!currentDirectoryItem || currentDirectoryItem.status === 'cancelled' || currentDirectoryItem.status === 'error') {
                return; // 鍋滄涓嬭浇
            }

            const remotePath = pathUtils.value.join(remoteDirPath, entry.name);
            const localPath = `${localDirPath}/${entry.name}`; // Local path handling (keeping as is for now or use separate utils)

            if (entry.isDir) {
                // Create local subdirectory
                try {
                    await mkdir(localPath, { recursive: true });
                } catch (e) {
                    console.log('Subdirectory might already exist:', e);
                }

                // Recursively download subdirectory
                await downloadDirectoryRecursive(remotePath, localPath, sessionId, directoryTransferId);
            } else {
                // Download file
                const fileTransferId = crypto.randomUUID();

                transferStore.addTransfer({
                    id: fileTransferId,
                    type: 'download',
                    name: entry.name,
                    localPath,
                    remotePath,
                    size: entry.size,
                    transferred: 0,
                    progress: 0,
                    status: 'pending',
                    sessionId
                });

                // 绛夊緟鏂囦欢涓嬭浇瀹屾垚
                await waitForFileCompletion(fileTransferId);

                // 鏇存柊鐩綍瀹屾垚璁℃暟
                transferStore.incrementDirectoryCompleted(directoryTransferId);
            }
        }
    } catch (e) {
        console.error(`Failed to download directory contents ${remoteDirPath}:`, e);
        throw e;
    }
}

async function waitForDirectoryResume(directoryTransferId: string): Promise<void> {
    return new Promise((resolve) => {
        const checkResume = () => {
            const item = transferStore.items.find(i => i.id === directoryTransferId);
            if (!item || item.status === 'cancelled' || item.status === 'error') {
                resolve(); // 鍋滄绛夊緟
            } else if (item.status === 'running') {
                resolve(); // 鎭㈠浜?
            } else {
                // 缁х画绛夊緟
                setTimeout(checkResume, 500);
            }
        };

        checkResume();
    });
}

async function waitForFileCompletion(fileTransferId: string): Promise<void> {
    return new Promise((resolve, reject) => {
        const checkCompletion = () => {
            const item = transferStore.items.find(i => i.id === fileTransferId);
            if (!item) {
                reject(new Error('Transfer item not found'));
                return;
            }

            if (item.status === 'completed') {
                resolve();
            } else if (item.status === 'error' || item.status === 'cancelled') {
                reject(new Error(item.error || 'Transfer failed'));
            } else {
                // 缁х画绛夊緟
                setTimeout(checkCompletion, 500);
            }
        };

        checkCompletion();
    });
}

async function handleDownload(file?: FileEntry) {
    try {
        // Determine if this is batch download or single download
        const isTreeMode = contextMenu.value.isTree;
        const isMultiSelect = !isTreeMode && selectedFiles.value.size > 1;

        if (isMultiSelect) {
            // Batch download for multiple selected files
            const selectedDirectory = await open({
                directory: true,
                title: 'Select download directory for batch download'
            });

            if (selectedDirectory && typeof selectedDirectory === 'string') {
                const targets = Array.from(selectedFiles.value);
                for (const fileName of targets) {
                    const entry = files.value.find(f => f.name === fileName);
                    if (!entry) continue;

                    const remotePath = pathUtils.value.join(currentPath.value, entry.name);
                    const localPath = selectedDirectory.endsWith('/') || selectedDirectory.endsWith('\\')
                        ? `${selectedDirectory}${entry.name}`
                        : `${selectedDirectory}/${entry.name}`;

                    if (entry.isDir) {
                        // Download directory recursively
                        await downloadDirectory(remotePath, localPath, props.sessionId);
                    } else {
                        // Download single file
                        transferStore.addTransfer({
                            id: crypto.randomUUID(),
                            type: 'download',
                            name: entry.name,
                            localPath,
                            remotePath,
                            size: entry.size,
                            transferred: 0,
                            progress: 0,
                            status: 'pending',
                            sessionId: props.sessionId
                        });
                    }
                }
            }
        } else {
            // Single file or directory download
            const targetFile = file || contextMenu.value.file;
            if (!targetFile) return;

            if (targetFile.isDir) {
                // Directory download - ask for local directory
                const selectedDirectory = await open({
                    directory: true,
                    title: 'Select directory to save folder'
                });

                if (selectedDirectory && typeof selectedDirectory === 'string') {
                    const remotePath = contextMenu.value.isTree && contextMenu.value.treePath
                        ? contextMenu.value.treePath
                        : pathUtils.value.join(currentPath.value, targetFile.name);

                    const localPath = selectedDirectory.endsWith('/') || selectedDirectory.endsWith('\\')
                        ? `${selectedDirectory}${targetFile.name}`
                        : `${selectedDirectory}/${targetFile.name}`;

                    await downloadDirectory(remotePath, localPath, props.sessionId);
                }
            } else {
                // Single file download
                const savePath = await save({
                    defaultPath: targetFile.name,
                    title: 'Save file as'
                });

                if (savePath) {
                    const remotePath = contextMenu.value.isTree && contextMenu.value.treePath
                        ? contextMenu.value.treePath
                        : pathUtils.value.join(currentPath.value, targetFile.name);

                    transferStore.addTransfer({
                        id: crypto.randomUUID(),
                        type: 'download',
                        name: targetFile.name,
                        localPath: savePath,
                        remotePath,
                        size: targetFile.size,
                        transferred: 0,
                        progress: 0,
                        status: 'pending',
                        sessionId: props.sessionId
                    });
                }
            }
        }
    } catch (e) {
        console.error(e);
        notificationStore.error("Download failed: " + e);
    }
    closeContextMenu();
}

async function handleChangePermissions(file: FileEntry) {
    if (!file) return;
    const newPerms = prompt("Enter new permissions (e.g., 755, u+x):", file.permissions ? file.permissions.toString() : '');
    if (newPerms) {
        try {
            const remotePath = contextMenu.value.isTree && contextMenu.value.treePath
                ? contextMenu.value.treePath
                : pathUtils.value.join(currentPath.value, file.name);
            await invoke('change_file_permission', {
                id: props.sessionId,
                path: remotePath,
                perms: newPerms
            });
            await loadFiles(currentPath.value);
        } catch (e) {
            notificationStore.error("Failed to change permissions: " + e);
        }
    }
    closeContextMenu();
}

async function performDelete(skipConfirm: boolean) {
    if (!contextMenu.value.isTree && selectedFiles.value.size === 0) return;

    if (!skipConfirm) {
        const count = contextMenu.value.isTree ? 1 : selectedFiles.value.size;
        const yes = await ask(t('fileManager.deleteConfirm.message', { count }), {
            title: t('fileManager.deleteConfirm.title'),
            kind: 'warning'
        });
        if (!yes) return;
    }

    try {
        // Convert Set to Array for iteration
        if (contextMenu.value.isTree && contextMenu.value.treePath && contextMenu.value.file) {
            const path = contextMenu.value.treePath;
            const isDir = contextMenu.value.file.isDir;
            await invoke('delete_item', { id: props.sessionId, path, isDir });
        } else {
            const targets = Array.from(selectedFiles.value);
            for (const name of targets) {
                // Find file entry to get isDir
                const entry = files.value.find(f => f.name === name);
                if (!entry) continue;

                const remotePath = pathUtils.value.join(currentPath.value, name);
                await invoke('delete_item', { id: props.sessionId, path: remotePath, isDir: entry.isDir });
            }
        }
        await loadFiles(currentPath.value);
    } catch (e) {
        notificationStore.error("Delete failed: " + e);
    }
    closeContextMenu();
}

async function handleDelete(file?: FileEntry) {
    if (!contextMenu.value.isTree) {
        if (selectedFiles.value.size === 0 && file) {
            selectedFiles.value.add(file.name);
        }
    }
    await performDelete(false);
}

function handleKeyDown(e: KeyboardEvent) {
    if (isEditingPath.value) return;
    if (document.activeElement?.tagName === 'INPUT' || document.activeElement?.tagName === 'TEXTAREA') return;

    if (e.key === 'Delete') {
        performDelete(e.shiftKey);
    }
}

async function handleRename(file: FileEntry) {
    const path = contextMenu.value.isTree && contextMenu.value.treePath
        ? contextMenu.value.treePath
        : pathUtils.value.join(currentPath.value, file.name);

    startRename(file, path);
    closeContextMenu();
}

function startRename(file: FileEntry, path: string) {
    renamingPath.value = path;
    renameInput.value = file.name;
    renamingType.value = 'rename';
}

async function startCreate(isDir: boolean) {
    const tempName = '';
    let parentPath = currentPath.value;

    if (viewMode.value === 'tree') {
        // Determine parent path for tree view
        if (contextMenu.value.isBackground) {
            // Background click in tree view - use current tree root
            parentPath = treeRootPath.value;
        } else if (contextMenu.value.isTree && contextMenu.value.file?.isDir && contextMenu.value.treePath) {
            parentPath = contextMenu.value.treePath;
        } else if (selectedTreePaths.value.size === 1) {
            const path = Array.from(selectedTreePaths.value)[0];
            const node = treeNodes.value.get(path);
            if (node && node.entry.isDir) {
                parentPath = path;
            }
        } else {
            // Default to current tree root
            parentPath = treeRootPath.value;
        }

        // If parent is not root, ensure it's expanded
        if (parentPath !== treeRootPath.value && parentPath !== '.') {
            const node = treeNodes.value.get(parentPath);
            if (node) {
                if (!expandedPaths.value.has(parentPath)) {
                    await toggleDirectory(node);
                }
            }
        }
    }

    // For flat view, tempPath should match the path comparison logic in VirtualFileList
    // Use manual concatenation to ensure empty name results in a trailing slash (or unique path)
    // avoiding pathUtils.normalize stripping it.
    let tempPath: string;
    if (parentPath === '.') {
        tempPath = tempName;
    } else if (parentPath.endsWith('/')) {
        tempPath = parentPath + tempName;
    } else {
        tempPath = parentPath + '/' + tempName;
    }

    const tempEntry: FileEntry = {
        name: tempName,
        size: 0,
        mtime: Date.now() / 1000,
        isDir: isDir,
        permissions: isDir ? 755 : 644,
        uid: 0,
        owner: 'user'
    };

    if (viewMode.value === 'flat') {
        files.value = [tempEntry, ...files.value];
        triggerRef(files);
    } else {
        // Tree view insertion
        const node: TreeNode = {
            entry: tempEntry,
            path: tempPath,
            depth: parentPath === '.' ? 0 : (parentPath.split('/').length),
            parentPath: parentPath === '.' ? null : parentPath,
            childrenLoaded: false,
            loading: false
        };

        treeNodes.value.set(tempPath, node);

        const parentKey = parentPath === '.' ? null : parentPath;
        const children = childrenMap.value.get(parentKey) || [];
        childrenMap.value.set(parentKey, [tempPath, ...children]);

        triggerRef(treeNodes);
        triggerRef(childrenMap);

        // Ensure parent is expanded if it's the root or we just expanded it
        if (parentPath !== '.') {
            expandedPaths.value.add(parentPath);
        }
    }

    // Close any open context menu
    closeContextMenu();

    // Start renaming immediately
    renamingPath.value = tempPath;
    renameInput.value = '';
    renamingType.value = isDir ? 'create_folder' : 'create_file';

    // Focus the input field after next tick
    await nextTick();
}

async function confirmRename() {
    if (renamingPath.value === null || isConfirmingRename.value) return;

    isConfirmingRename.value = true;

    const newName = renameInput.value;
    if (!newName) {
        cancelRename();
        isConfirmingRename.value = false;
        return;
    }

    try {
        if (renamingType.value === 'rename') {
            const oldPath = renamingPath.value;
            const parts = oldPath.split('/');
            const oldName = parts.pop();
            if (newName === oldName) {
                cancelRename();
                isConfirmingRename.value = false;
                return;
            }

            const parent = parts.join('/');
            const newPath = pathUtils.value.join(parent, newName);

            await invoke('rename_item', { id: props.sessionId, oldPath, newPath });
        } else {
            // Create
            let parentPath = currentPath.value;
            let remotePath: string;

            // For tree view, get the correct parent path
            if (viewMode.value === 'tree' && renamingPath.value) {
                const parts = renamingPath.value.split('/');
                parts.pop(); // Remove empty name
                parentPath = parts.join('/') || '/'; // Ensure parentPath is not empty, default to root
            }

            remotePath = pathUtils.value.join(parentPath, newName);

            if (renamingType.value === 'create_folder') {
                await invoke('create_directory', { id: props.sessionId, path: remotePath });
            } else {
                await invoke('create_file', { id: props.sessionId, path: remotePath });
            }
        }
        await loadFiles(currentPath.value);
    } catch (e) {
        // Provide more user-friendly error message
        let errorMessage = `${renamingType.value === 'rename' ? 'Rename' : 'Create'} failed: ${e}`;
        if (e && typeof e === 'object' && 'toString' in e && e.toString().includes('SFTP(4)')) {
            errorMessage = `${renamingType.value === 'rename' ? 'Rename' : 'Create'} failed: Permission denied or invalid path`;
        }

        notificationStore.error(errorMessage);
        // If create failed, we should probably remove the temp entry
        if (renamingType.value !== 'rename') {
            if (viewMode.value === 'flat') {
                files.value = files.value.filter(f => f.name !== '');
                triggerRef(files);
            } else {
                // Remove from tree
                if (renamingPath.value) {
                    treeNodes.value.delete(renamingPath.value);
                    for (const [key, children] of childrenMap.value.entries()) {
                        if (children.includes(renamingPath.value)) {
                            childrenMap.value.set(key, children.filter(c => c !== renamingPath.value));
                            break;
                        }
                    }
                    triggerRef(treeNodes);
                    triggerRef(childrenMap);
                }
            }
        }
    } finally {
        cancelRename();
        isConfirmingRename.value = false;
    }
}

function cancelRename() {
    if (renamingType.value !== 'rename') {
        // Remove temporary entry for create operations
        if (viewMode.value === 'flat') {
            files.value = files.value.filter(f => f.name !== '');
            triggerRef(files);
        } else {
            // Remove from tree
            if (renamingPath.value) {
                treeNodes.value.delete(renamingPath.value);
                for (const [key, children] of childrenMap.value.entries()) {
                    if (children.includes(renamingPath.value)) {
                        childrenMap.value.set(key, children.filter(c => c !== renamingPath.value));
                        break;
                    }
                }
                triggerRef(treeNodes);
                triggerRef(childrenMap);
            }
        }
    }
    renamingPath.value = null;
    renameInput.value = '';
}

async function createFolder() {
    startCreate(true);
}

async function createFile() {
    startCreate(false);
}

function copyPath(file: FileEntry) {
    const remotePath = contextMenu.value.isTree && contextMenu.value.treePath
        ? contextMenu.value.treePath
        : pathUtils.value.join(currentPath.value, file.name);
    navigator.clipboard.writeText(remotePath);
    closeContextMenu();
}

function copyCurrentPath() {
    navigator.clipboard.writeText(currentPath.value);
    closeContextMenu();
}

function copyName(file: FileEntry) {
    navigator.clipboard.writeText(file.name);
    closeContextMenu();
}

function handleSwitchToTerminalPath() {
    let path = '';
    if (contextMenu.value.isBackground) {
        path = currentPath.value;
    } else if (contextMenu.value.file) {
        path = contextMenu.value.isTree && contextMenu.value.treePath
            ? contextMenu.value.treePath
            : pathUtils.value.join(currentPath.value, contextMenu.value.file.name);
    }

    if (path) {
        emit('switchToTerminalPath', props.sessionId, path);
    }
    closeContextMenu();
}

function formatDate(timestamp: number) {
    return new Date(timestamp * 1000).toLocaleString();
}

function formatSize(size: number): string {
    if (!Number.isFinite(size) || size < 0) return '-';
    if (size === 0) return '0 B';
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    let value = size;
    let unitIndex = 0;
    while (value >= 1024 && unitIndex < units.length - 1) {
        value /= 1024;
        unitIndex++;
    }
    return `${value.toFixed(value >= 10 || unitIndex === 0 ? 0 : 1)} ${units[unitIndex]}`;
}
</script>

<template>
    <div ref="containerRef" class="h-full bg-gray-900 text-white p-2 flex flex-col" @click="closeContextMenu">
        <!-- Toolbar -->
        <div class="flex flex-col space-y-2 mb-2 bg-gray-800 p-2 rounded">
            <!-- Path Bar -->
            <div class="flex items-center space-x-2">
                <button @click="goUp" class="p-1 hover:bg-gray-700 rounded text-gray-300"
                    :title="t('fileManager.toolbar.upLevel')">
                    <ArrowUp class="w-4 h-4" />
                </button>
                <div class="flex-1 relative">
                    <input v-model="pathInput" @input="handlePathInput" @keydown="handlePathInputKeydown"
                        @blur="handlePathInputBlur"
                        class="w-full bg-gray-900 border border-gray-700 rounded px-2 py-1 text-sm font-mono text-gray-300 focus:outline-none focus:border-blue-500"
                        :placeholder="t('fileManager.toolbar.pathPlaceholder')" />

                    <!-- Suggestions List -->
                    <div v-if="showSuggestions && suggestions.length > 0" id="path-suggestions-list"
                        class="absolute top-full left-0 w-full bg-gray-800 border border-gray-700 rounded shadow-lg z-50 max-h-60 overflow-y-auto mt-1">
                        <div v-for="(path, index) in suggestions" :key="path" @click="selectSuggestion(path)"
                            class="px-2 py-1 text-sm cursor-pointer hover:bg-gray-700 font-mono truncate"
                            :class="{ 'bg-gray-700 text-blue-400': index === activeSuggestionIndex, 'text-gray-300': index !== activeSuggestionIndex }">
                            {{ path }}
                        </div>
                    </div>
                </div>
                <button @click="refresh" class="p-1 hover:bg-gray-700 rounded text-gray-300"
                    :title="t('fileManager.toolbar.refresh')">
                    <RefreshCw class="w-4 h-4" />
                </button>
            </div>

            <!-- Action Buttons -->
            <div class="flex items-center space-x-2 border-t border-gray-700 pt-2">
                <button @click="createFile"
                    class="flex items-center space-x-1 px-2 py-1 text-xs bg-gray-700 hover:bg-gray-600 rounded"
                    :title="t('fileManager.toolbar.newFile')">
                    <FilePlus class="w-3 h-3" />
                    <span>{{ t('fileManager.toolbar.newFile') }}</span>
                </button>
                <button @click="createFolder"
                    class="flex items-center space-x-1 px-2 py-1 text-xs bg-gray-700 hover:bg-gray-600 rounded"
                    :title="t('fileManager.toolbar.newFolder')">
                    <FolderPlus class="w-3 h-3" />
                    <span>{{ t('fileManager.toolbar.newFolder') }}</span>
                </button>
                <div class="w-px h-4 bg-gray-700 mx-1"></div>
                <button @click="handleUpload"
                    class="flex items-center space-x-1 px-2 py-1 text-xs bg-blue-600 hover:bg-blue-500 rounded"
                    :title="t('fileManager.toolbar.uploadFile')">
                    <Upload class="w-3 h-3" />
                    <span>{{ t('fileManager.toolbar.uploadFile') }}</span>
                </button>
                <!-- Upload Directory placeholder -->
                <button @click="handleUploadDirectory"
                    class="flex items-center space-x-1 px-2 py-1 text-xs bg-blue-600 hover:bg-blue-500 rounded"
                    :title="t('fileManager.toolbar.uploadDirectory')">
                    <FolderPlus class="w-3 h-3" />
                    <span>{{ t('fileManager.toolbar.uploadDirectory') }}</span>
                </button>
            </div>
        </div>

        <!-- File List -->
        <div class="flex-1 overflow-y-auto border border-gray-800 rounded bg-gray-900/50"
            @dragover="handleNativeDragOver" @drop="handleNativeDrop" @contextmenu="handleContainerContextMenu">
            <!-- Header -->
            <div
                class="flex items-center p-2 text-xs text-gray-500 border-b border-gray-800 bg-gray-800/50 font-bold select-none">
                <div class="flex items-center px-2" :style="{ width: columnWidths.name + 'px' }">
                    <span>{{ t('fileManager.headers.name') }}</span>
                    <span class="w-1 h-6 ml-auto cursor-col-resize bg-gray-600 hover:bg-blue-500 transition-colors"
                        @mousedown.stop="startResize('name', $event)"></span>
                </div>
                <div class="flex items-center px-2" :style="{ width: columnWidths.size + 'px' }">
                    <span>{{ t('fileManager.headers.size') }}</span>
                    <span class="w-1 h-6 ml-auto cursor-col-resize bg-gray-600 hover:bg-blue-500 transition-colors"
                        @mousedown.stop="startResize('size', $event)"></span>
                </div>
                <div class="flex items-center px-2" :style="{ width: columnWidths.date + 'px' }">
                    <span>{{ t('fileManager.headers.dateModified') }}</span>
                    <span class="w-1 h-6 ml-auto cursor-col-resize bg-gray-600 hover:bg-blue-500 transition-colors"
                        @mousedown.stop="startResize('date', $event)"></span>
                </div>
                <div class="flex items-center px-2" :style="{ width: columnWidths.owner + 'px' }">
                    <span>{{ t('fileManager.headers.owner') }}</span>
                </div>
            </div>

            <!-- Flat View -->
            <template v-if="viewMode === 'flat'">
                <VirtualFileList :items="files" :view-mode="viewMode" :selected-files="selectedFiles"
                    :selected-tree-paths="selectedTreePaths" :column-widths="columnWidths"
                    :on-selection="handleSelection" :on-navigate="navigate" :on-context-menu="showContextMenu"
                    :on-drag-start="onDragStart" :expanded-paths="expandedPaths" :format-size="formatSize"
                    :format-date="formatDate" :renaming-path="renamingPath" v-model:rename-input="renameInput"
                    @confirm-rename="confirmRename" @cancel-rename="cancelRename" :current-path="currentPath" />
            </template>

            <!-- Tree View -->
            <template v-else>
                <VirtualFileList :items="visibleTreeNodes" :view-mode="viewMode" :selected-files="selectedFiles"
                    :selected-tree-paths="selectedTreePaths" :column-widths="columnWidths"
                    :on-selection="handleSelection" :on-navigate="navigate" :on-context-menu="showContextMenu"
                    :on-tree-selection="handleTreeSelection" :on-open-tree-file="openTreeFile"
                    :on-tree-context-menu="showTreeContextMenu" :on-toggle-directory="toggleDirectory"
                    :on-drag-start="onDragStart" :expanded-paths="expandedPaths" :format-size="formatSize"
                    :format-date="formatDate" :renaming-path="renamingPath" v-model:rename-input="renameInput"
                    @confirm-rename="confirmRename" @cancel-rename="cancelRename" :current-path="currentPath" />
            </template>

            <div v-if="files.length === 0" class="p-4 text-center text-gray-600 text-sm">
                {{ t('fileManager.emptyDirectory') }}
            </div>
        </div>

        <!-- Transfer List -->
        <TransferList />

        <!-- Opening File Indicator -->
        <div v-if="isOpeningFile"
            class="fixed bottom-4 right-4 bg-gray-800/90 text-gray-100 text-xs px-3 py-2 rounded shadow-lg border border-gray-700 z-50">
            姝ｅ湪鎵撳紑...
        </div>

        <!-- Context Menu -->
        <div v-if="contextMenu.show" ref="contextMenuRef"
            :style="{ top: contextMenu.y + 'px', left: contextMenu.x + 'px' }"
            class="fixed bg-gray-800 border border-gray-700 shadow-xl rounded z-50 py-1 min-w-[150px]">

            <template v-if="contextMenu.isBackground">
                <button @click.stop="refresh(); closeContextMenu()"
                    class="w-full text-left px-4 py-2 text-sm hover:bg-gray-700 flex items-center">
                    <RefreshCw class="w-4 h-4 mr-2 text-gray-400" />
                    {{ t('fileManager.toolbar.refresh') }}
                </button>
                <div class="border-t border-gray-700 my-1"></div>
                <button @click.stop="createFile(); closeContextMenu()"
                    class="w-full text-left px-4 py-2 text-sm hover:bg-gray-700 flex items-center">
                    <FilePlus class="w-4 h-4 mr-2 text-gray-400" />
                    {{ t('fileManager.toolbar.newFile') }}
                </button>
                <button @click.stop="createFolder(); closeContextMenu()"
                    class="w-full text-left px-4 py-2 text-sm hover:bg-gray-700 flex items-center">
                    <FolderPlus class="w-4 h-4 mr-2 text-gray-400" />
                    {{ t('fileManager.toolbar.newFolder') }}
                </button>
                <div class="border-t border-gray-700 my-1"></div>
                <button @click.stop="handleUpload(); closeContextMenu()"
                    class="w-full text-left px-4 py-2 text-sm hover:bg-gray-700 flex items-center">
                    <Upload class="w-4 h-4 mr-2 text-gray-400" />
                    {{ t('fileManager.toolbar.uploadFile') }}
                </button>
                <button @click.stop="handleUploadDirectory(); closeContextMenu()"
                    class="w-full text-left px-4 py-2 text-sm hover:bg-gray-700 flex items-center">
                    <FolderPlus class="w-4 h-4 mr-2 text-gray-400" />
                    {{ t('fileManager.toolbar.uploadDirectory') }}
                </button>
                <div class="border-t border-gray-700 my-1"></div>
                <button @click.stop="handleSetWorkspace()"
                    class="w-full text-left px-4 py-2 text-sm hover:bg-gray-700 flex items-center text-purple-400">
                    <Briefcase class="w-4 h-4 mr-2" />
                    Set as AI Workspace
                </button>
                <button @click.stop="copyCurrentPath()"
                    class="w-full text-left px-4 py-2 text-sm hover:bg-gray-700 flex items-center">
                    <Copy class="w-4 h-4 mr-2 text-gray-400" />
                    {{ t('fileManager.contextMenu.copyCurrentPath') }}
                </button>
                <div class="border-t border-gray-700 my-1"></div>
                <button @click.stop="handleSwitchToTerminalPath()"
                    class="w-full text-left px-4 py-2 text-sm hover:bg-gray-700 flex items-center">
                    <TerminalIcon class="w-4 h-4 mr-2 text-gray-400" />
                    {{ t('fileManager.contextMenu.switchToTerminalPath') || '在终端打开' }}
                </button>
            </template>

            <template v-else>
                <button @click.stop="handleDownload()"
                    class="w-full text-left px-4 py-2 text-sm hover:bg-gray-700 flex items-center">
                    <span class="flex-1">{{
                        (!contextMenu.isTree && selectedFiles.size > 1)
                            ? t('fileManager.contextMenu.batchDownload')
                            : t('fileManager.contextMenu.download')
                    }}</span>
                    <span v-if="!contextMenu.isTree && selectedFiles.size > 1" class="text-xs text-gray-400">({{
                        selectedFiles.size
                    }})</span>
                </button>
                <button @click.stop="handleRename(contextMenu.file!)"
                    class="w-full text-left px-4 py-2 text-sm hover:bg-gray-700">{{ t('fileManager.contextMenu.rename')
                    }}</button>
                <button @click.stop="handleDelete(contextMenu.file!)"
                    class="w-full text-left px-4 py-2 text-sm hover:bg-gray-700 text-red-400">
                    {{ t('fileManager.contextMenu.delete') }} {{ selectedFiles.size > 1 ? `(${selectedFiles.size})` : ''
                    }}
                </button>
                <div class="border-t border-gray-700 my-1"></div>
                <button v-if="contextMenu.file?.isDir" @click.stop="handleSetWorkspace()"
                    class="w-full text-left px-4 py-2 text-sm hover:bg-gray-700 flex items-center text-purple-400">
                    <Briefcase class="w-4 h-4 mr-2" />
                    Set as AI Workspace
                </button>
                <button @click.stop="copyPath(contextMenu.file!)"
                    class="w-full text-left px-4 py-2 text-sm hover:bg-gray-700">{{
                        t('fileManager.contextMenu.copyPath') }}</button>
                <button @click.stop="copyName(contextMenu.file!)"
                    class="w-full text-left px-4 py-2 text-sm hover:bg-gray-700">{{
                        t('fileManager.contextMenu.copyName') }}</button>
                <button @click.stop="handleChangePermissions(contextMenu.file!)"
                    class="w-full text-left px-4 py-2 text-sm hover:bg-gray-700">{{
                        t('fileManager.contextMenu.changePermissions')
                    }}</button>
                <div class="border-t border-gray-700 my-1"></div>
                <button v-if="contextMenu.file?.isDir" @click.stop="handleSwitchToTerminalPath()"
                    class="w-full text-left px-4 py-2 text-sm hover:bg-gray-700 flex items-center">
                    <TerminalIcon class="w-4 h-4 mr-2 text-gray-400" />
                    {{ t('fileManager.contextMenu.switchToTerminalPath') || '在终端打开' }}
                </button>
            </template>
        </div>
    </div>
</template>
