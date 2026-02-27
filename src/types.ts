export interface SshKey {
  id: number;
  name: string;
  content: string;
  passphrase?: string;
  createdAt: number;
}

export interface Connection {
  id?: number;
  name: string;
  host: string;
  port: number;
  username: string;
  password?: string;
  authType?: "password" | "key";
  sshKeyId?: number | null;
  // Jump host config
  jumpHost?: string;
  jumpPort?: number;
  jumpUsername?: string;
  jumpPassword?: string;
  groupId?: number | null;
  osType?: string; // Operating system type: "Linux", "Windows", "macOS", optional for backward compatibility
}

export interface ConnectionGroup {
  id?: number;
  name: string;
  parentId?: number | null;
  children?: (ConnectionGroup | Connection)[]; // For UI tree structure
}

export interface FileEntry {
  name: string;
  isDir: boolean;
  size: number;
  mtime: number;
  permissions: number;
  uid: number;
  owner: string;
}

export type ColumnKey = "name" | "size" | "date" | "owner";

export interface AIConfig {
  apiUrl: string;
  apiKey: string;
  modelName: string;
}

export type TerminalCursorStyle = "block" | "underline" | "bar";

export interface TerminalAppearanceSettings {
  fontSize: number;
  fontFamily: string;
  cursorStyle: TerminalCursorStyle;
  lineHeight: number;
}

export type FileManagerViewMode = "flat" | "tree";
export type FileManagerLayout = "left" | "bottom";

export interface FileManagerSettings {
  viewMode: FileManagerViewMode;
  layout: FileManagerLayout;
  sftpBufferSize: number; // SFTP buffer size in KB
}

export interface SshPoolSettings {
  maxBackgroundSessions: number; // 最大后台会话数量
  enableAutoCleanup: boolean; // 是否启用自动清理
  cleanupIntervalMinutes: number; // 清理间隔（分钟）
}

export interface ConnectionTimeoutSettings {
  connectionTimeoutSecs: number;
  jumpHostTimeoutSecs: number;
  localForwardTimeoutSecs: number;
  commandTimeoutSecs: number;
  sftpOperationTimeoutSecs: number;
}

export interface ReconnectSettings {
  maxReconnectAttempts: number;      // Maximum reconnection attempts, default 5
  initialDelayMs: number;            // Initial delay in ms, default 1000
  maxDelayMs: number;                // Maximum delay in ms, default 30000
  backoffMultiplier: number;         // Backoff multiplier, default 2.0
  enableAutoReconnect: boolean;      // Enable auto reconnect, default true
}

export interface HeartbeatSettings {
  tcpKeepaliveIntervalSecs: number;      // TCP keepalive interval, default 60
  sshKeepaliveIntervalSecs: number;      // SSH keepalive interval, default 15
  appHeartbeatIntervalSecs: number;      // Application layer heartbeat interval, default 30
  heartbeatTimeoutSecs: number;          // Heartbeat timeout, default 5
  failedHeartbeatsBeforeAction: number;  // Failed heartbeats before action, default 3
}

export interface PoolHealthSettings {
  healthCheckIntervalSecs: number;     // Health check interval, default 60
  sessionWarmupCount: number;          // Session warmup count, default 1
  maxSessionAgeMinutes: number;        // Max session age in minutes, default 60
  unhealthyThreshold: number;          // Unhealthy failure threshold, default 3
}

export type NetworkQuality = "Excellent" | "Good" | "Fair" | "Poor" | "Unknown";

export interface NetworkAdaptiveSettings {
  enableAdaptive: boolean;             // Enable adaptive mode, default true
  latencyCheckIntervalSecs: number;    // Latency check interval, default 30
  highLatencyThresholdMs: number;      // High latency threshold, default 300
  lowBandwidthThresholdKbps: number;   // Low bandwidth threshold, default 100
}

export interface NetworkStatus {
  latencyMs: number;                   // Current latency in ms
  bandwidthKbps?: number;              // Estimated bandwidth in KB/s
  quality: NetworkQuality;             // Network quality level
  lastUpdate: number;                  // Last update timestamp
}

export interface AdaptiveParams {
  heartbeatIntervalSecs: number;
  sftpBufferSize: number;
  commandTimeoutSecs: number;
  keepaliveIntervalSecs: number;
}

export interface Settings {
  theme: "light" | "dark";
  language: "en" | "zh";
  ai: AIConfig;
  terminalAppearance: TerminalAppearanceSettings;
  fileManager: FileManagerSettings;
  sshPool: SshPoolSettings;
  connectionTimeout: ConnectionTimeoutSettings;
  reconnect: ReconnectSettings;
  heartbeat: HeartbeatSettings;
  poolHealth: PoolHealthSettings;
  networkAdaptive: NetworkAdaptiveSettings;
}

export interface Workspace {
  path: string;
  name: string;
  context: string;
  fileTree: string;
  isIndexed: boolean;
}

export interface Session {
  id: string; // UUID from backend
  connectionId: number;
  connectionName: string;
  status: "connected" | "disconnected" | "connecting";
  activeTab: "terminal" | "files" | "ai";
  currentPath: string;
  files: FileEntry[];
  connectedAt: number;
  activeWorkspace?: Workspace;
  os?: string;
}

export type ConnectionStatus =
  | "connecting"
  | "connected"
  | "authenticating"
  | "ready"
  | "degraded"
  | "reconnecting"
  | "disconnected"
  | "error";

export interface ConnectionMetrics {
  uptimeSecs: number;
  bytesSent: number;
  bytesReceived: number;
  latencyMs: number;
  reconnectCount: number;
  lastError?: string;
}

export interface ConnectionStatusEvent {
  sessionId: string;
  status: ConnectionStatus;
  timestamp: number;
  details?: string;
  metrics?: ConnectionMetrics;
}

export interface ReconnectEvent {
  sessionId: string;
  attempt: number;
  maxAttempts: number;
  delayMs: number;
}
