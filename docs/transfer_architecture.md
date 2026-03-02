# SSH 文件传输架构设计文档

## 1. 概述

本文档描述 SSH Assistant 文件传输模块的完整重构方案，解决当前传输卡住、连接冲突、无法断点续传等问题。

## 2. 当前架构问题

### 2.1 问题列表

| 问题 | 影响 | 根本原因 |
|------|------|----------|
| 传输卡住 | 用户体验差 | 无超时、无状态检测 |
| 心跳与传输冲突 | 传输中断 | 共用 Session |
| 无法断点续传 | 大文件传输失败 | 无状态持久化 |
| 阻塞式 IO | 资源浪费 | 线程 spawn 模式 |
| 难以监控 | 调试困难 | 状态分散 |

### 2.2 现有代码结构

```
src-tauri/src/ssh/
├── manager.rs          # SshManager: 会话+传输+心跳 混在一起
├── file_ops.rs         # 前端命令处理
├── connection.rs       # SessionSshPool: 连接池
├── heartbeat.rs        # HeartbeatManager
├── reconnect.rs        # ReconnectManager
└── utils.rs            # ssh2_retry 工具函数
```

## 3. 新架构设计

### 3.1 整体架构

```
┌─────────────────────────────────────────────────────────────────────────┐
│                          Frontend (Vue 3)                               │
│                    Transfer API (Tauri Commands)                        │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                         Command Handler Layer                           │
│                        (file_ops.rs - 简化版)                            │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                ┌───────────────────┴───────────────────┐
                ▼                                       ▼
┌───────────────────────────────┐   ┌───────────────────────────────────┐
│      SessionManager           │   │      TransferManager              │
│  (会话管理 - 现有改进)          │   │  (传输管理 - 新建)                │
├───────────────────────────────┤   ├───────────────────────────────────┤
│ - 连接池管理                   │   │ - 独立传输连接池                   │
│ - 心跳检测                     │   │ - 传输状态机                       │
│ - 重连机制                     │   │ - 断点续传                         │
│ - Shell 通道                   │   │ - 并发控制                         │
└───────────────────────────────┘   │ - 进度监控                         │
                                    │ - 健康检查                         │
                                    └───────────────────────────────────┘
                                                                    │
                                               ┌────────────────────┤
                                               ▼                    ▼
                                    ┌──────────────────┐  ┌──────────────────┐
                                    │  TransferState   │  │  TransferPool    │
                                    │  (状态持久化)     │  │  (连接池)         │
                                    └──────────────────┘  └──────────────────┘
                                               │
                                               ▼
                                    ┌──────────────────┐
                                    │   AsyncSftp      │
                                    │  (异步操作层)     │
                                    └──────────────────┘
                                               │
                                               ▼
                                    ┌──────────────────┐
                                    │  Tokio Runtime   │
                                    └──────────────────┘
```

### 3.2 模块职责

#### 3.2.1 SessionManager (改进现有)
- 管理交互式会话（Shell）
- 心跳检测
- 连接重连
- **不负责文件传输**

#### 3.2.2 TransferManager (新建)
- 管理所有文件传输任务
- 独立的传输连接池
- 传输状态机
- 断点续传逻辑
- 并发控制

#### 3.2.3 AsyncSftp (新建)
- 基于 tokio 的异步 SFTP 操作
- 超时控制
- 错误重试

#### 3.2.4 TransferState (新建)
- 传输状态持久化
- 断点信息保存

#### 3.2.5 TransferPool (新建)
- 传输专用连接池
- 与 SessionManager 的池隔离

## 4. 传输状态机

### 4.1 状态定义

```rust
pub enum TransferStatus {
    // 初始状态
    Pending,

    // 正在传输
    Connecting,      // 建立连接
    Transferring,    // 数据传输中
    Paused,          // 暂停

    // 完成状态
    Completed,       // 成功完成
    Failed,          // 失败
    Cancelled,       // 用户取消

    // 恢复状态
    Resuming,        // 断点续传中
}
```

### 4.2 状态转换

```
     ┌─────────┐
     │ Pending │
     └────┬────┘
          │ start()
          ▼
     ┌─────────────┐
     │ Connecting  │
     └────┬────────┘
          │ connected
          ▼
     ┌─────────────┐    pause()    ┌────────┐
     │Transferring │◄───────────────│ Paused │
     └────┬────────┘                └───┬────┘
          │                             │
          │ resume()                    │ cancel()
          ▼                             ▼
     ┌─────────────┐              ┌──────────┐
     │  Completed  │              │ Cancelled│
     └─────────────┘              └──────────┘
          ▲                             ▲
          │                             │
          └─────────┐  error()   ┌─────┘
                    └────────────┤
                                 ▼
                          ┌──────────┐
                          │  Failed  │
                          └─────┬────┘
                                │
                          retry()│
                                ▼
                          ┌─────────────┐
                          │  Resuming   │
                          └─────────────┘
```

## 5. 断点续传设计

### 5.1 断点信息结构

```rust
pub struct TransferCheckpoint {
    pub transfer_id: String,
    pub operation: TransferOp,  // Upload/Download

    // 文件信息
    pub local_path: PathBuf,
    pub remote_path: String,
    pub file_size: u64,

    // 进度信息
    pub transferred: u64,
    pub chunk_size: usize,

    // 校验信息
    pub use_checksum: bool,
    pub local_checksum: Option<String>,
    pub remote_checksum: Option<String>,

    // 时间信息
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

### 5.2 断点续传流程

```
┌─────────────────────────────────────────────────────────────────┐
│                        断点续传流程                               │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  1. 传输开始                                                      │
│     ├─ 创建 TransferCheckpoint                                   │
│     ├─ 保存到数据库                                              │
│     └─ 初始化 transferred = 0                                    │
│                                                                  │
│  2. 传输中 (每完成一个 chunk)                                     │
│     ├─ 更新 transferred                                          │
│     ├─ 更新 updated_at                                           │
│     └─ 定期保存到数据库 (每 10 秒或每 10MB)                       │
│                                                                  │
│  3. 传输失败/中断                                                 │
│     ├─ 保存当前状态到数据库                                       │
│     └─ 状态设为 Failed                                           │
│                                                                  │
│  4. 断点续传                                                      │
│     ├─ 从数据库读取断点信息                                       │
│     ├─ 验证文件完整性 (可选 checksum)                             │
│     ├─ 设置 remote 文件偏移量                                     │
│     └─ 从断点位置继续传输                                         │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

## 6. 异步 SFTP 操作层

### 6.1 设计目标

- 使用 tokio 异步运行时
- 每个操作带超时控制
- 自动重试临时错误
- 取消支持

### 6.2 接口设计

```rust
pub struct AsyncSftp {
    session: Arc<Mutex<SshSession>>,
    timeout: Duration,
}

impl AsyncSftp {
    // 带超时的下载
    pub async fn download_with_timeout(
        &self,
        remote_path: &str,
        local_path: &str,
        progress: &ProgressCallback,
        cancel: &AtomicBool,
    ) -> Result<u64, TransferError>;

    // 带超时的上传
    pub async fn upload_with_timeout(
        &self,
        local_path: &str,
        remote_path: &str,
        progress: &ProgressCallback,
        cancel: &AtomicBool,
    ) -> Result<u64, TransferError>;

    // 断点续传下载
    pub async fn resume_download(
        &self,
        remote_path: &str,
        local_path: &str,
        offset: u64,
        progress: &ProgressCallback,
        cancel: &AtomicBool,
    ) -> Result<u64, TransferError>;
}
```

## 7. 传输连接池设计

### 7.1 与会话池隔离

```
┌────────────────────────────────────────────────────────────┐
│                   SessionManager                            │
│  ┌──────────────────────────────────────────────────────┐  │
│  │  SessionPool (交互式会话)                              │  │
│  │  ┌────────┐  ┌────────┐  ┌────────┐                  │  │
│  │  │Shell 1 │  │Shell 2 │  │Shell 3 │  ...             │  │
│  │  └────────┘  └────────┘  └────────┘                  │  │
│  └──────────────────────────────────────────────────────┘  │
│                                                            │
│  - 用于终端交互                                             │
│  - 心跳检测                                                 │
│  - AI 辅助                                                  │
└────────────────────────────────────────────────────────────┘

┌────────────────────────────────────────────────────────────┐
│                   TransferManager                           │
│  ┌──────────────────────────────────────────────────────┐  │
│  │  TransferPool (传输专用)                               │  │
│  │  ┌────────┐  ┌────────┐  ┌────────┐                  │  │
│  │  │SFTP 1 │  │SFTP 2 │  │SFTP 3 │  ... (按需创建)     │  │
│  │  └────────┘  └────────┘  └────────┘                  │  │
│  └──────────────────────────────────────────────────────┘  │
│                                                            │
│  - 专用于文件传输                                           │
│  - 独立生命周期                                             │
│  - 不受心跳影响                                             │
└────────────────────────────────────────────────────────────┘
```

### 7.2 连接生命周期

```rust
pub struct TransferPool {
    // 按客户端 ID 分组
    pools: HashMap<String, Vec<TransferConnection>>,
    max_per_client: usize,  // 每个客户端最大连接数
}

pub struct TransferConnection {
    session: SshSession,
    sftp: Sftp,
    last_used: Instant,
    in_use: AtomicBool,
}

impl TransferPool {
    // 获取可用连接
    pub async fn acquire(&self, client_id: &str) -> Result<TransferConnection, Error>;

    // 归还连接
    pub fn release(&mut self, conn: TransferConnection);

    // 清理空闲连接
    pub async fn cleanup_idle(&mut self, idle_timeout: Duration);
}
```

## 8. 数据库扩展

### 8.1 传输记录表

```sql
CREATE TABLE transfer_records (
    id TEXT PRIMARY KEY,
    client_id TEXT NOT NULL,
    operation TEXT NOT NULL,  -- 'upload' or 'download'
    local_path TEXT NOT NULL,
    remote_path TEXT NOT NULL,
    file_size INTEGER NOT NULL,
    transferred INTEGER DEFAULT 0,
    chunk_size INTEGER DEFAULT 65536,

    -- 断点续传
    enable_resume BOOLEAN DEFAULT 1,
    local_checksum TEXT,
    remote_checksum TEXT,

    -- 状态
    status TEXT NOT NULL,  -- TransferStatus
    error_msg TEXT,

    -- 时间
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    completed_at INTEGER,

    FOREIGN KEY (client_id) REFERENCES clients(id) ON DELETE CASCADE
);

CREATE INDEX idx_transfer_client ON transfer_records(client_id);
CREATE INDEX idx_transfer_status ON transfer_records(status);
```

### 8.2 传输块记录表 (可选，用于更细粒度的断点续传)

```sql
CREATE TABLE transfer_chunks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    transfer_id TEXT NOT NULL,
    chunk_index INTEGER NOT NULL,
    offset INTEGER NOT NULL,
    size INTEGER NOT NULL,
    completed BOOLEAN DEFAULT 0,
    created_at INTEGER NOT NULL,

    FOREIGN KEY (transfer_id) REFERENCES transfer_records(id) ON DELETE CASCADE,
    UNIQUE(transfer_id, chunk_index)
);
```

## 9. 文件结构

```
src-tauri/src/ssh/
├── manager.rs              # SessionManager (简化)
├── file_ops.rs             # 前端命令处理 (简化)
├── connection.rs           # SessionPool (保持)
├── heartbeat.rs            # (保持)
├── reconnect.rs            # (保持)
├── utils.rs                # (保持)
│
├── transfer/               # 新建: 传输模块
│   ├── mod.rs              # 模块导出
│   ├── manager.rs          # TransferManager
│   ├── state.rs            # TransferState (状态机)
│   ├── pool.rs             # TransferPool (连接池)
│   ├── checkpoint.rs       # 断点续传
│   ├── async_sftp.rs       # 异步 SFTP 操作
│   └── types.rs            # 传输相关类型
│
└── db.rs                   # 扩展数据库操作
```

## 10. 实施计划

### Phase 1: 核心架构 (1-2天)
- [ ] 创建 transfer/ 目录结构
- [ ] 实现 TransferManager 基础框架
- [ ] 实现 TransferState 状态机
- [ ] 实现 TransferPool 连接池

### Phase 2: 异步操作层 (1天)
- [ ] 实现 AsyncSftp 基础操作
- [ ] 添加超时控制
- [ ] 添加取消支持

### Phase 3: 断点续传 (1天)
- [ ] 实现 TransferCheckpoint
- [ ] 扩展数据库表
- [ ] 实现断点保存/加载

### Phase 4: 集成与测试 (1天)
- [ ] 更新 file_ops.rs 集成新架构
- [ ] 更新前端适配新 API
- [ ] 测试各场景

### Phase 5: 优化与完善 (1天)
- [ ] 性能优化
- [ ] 错误处理完善
- [ ] 文档更新

## 11. 兼容性

### 11.1 向后兼容

- 前端 API 保持不变 (内部实现变化)
- 数据库迁移脚本自动添加新表
- 旧的传输记录可以查询但无法断点续传

### 11.2 前端变化

前端可以添加新功能：
- 暂停/恢复传输按钮
- 断点续传提示
- 传输历史记录
- 传输队列管理

## 12. 配置项

```rust
pub struct TransferSettings {
    // 连接池
    pub max_transfer_connections: usize,      // 默认 3
    pub transfer_connection_idle_timeout: u64, // 默认 300 秒

    // 传输
    pub default_chunk_size: usize,            // 默认 64KB
    pub max_concurrent_transfers: usize,       // 默认 5

    // 超时
    pub transfer_timeout_secs: u32,           // 默认 300 (5分钟)
    pub no_progress_timeout_secs: u32,        // 默认 30
    pub operation_timeout_secs: u32,          // 默认 60

    // 断点续传
    pub enable_resume: bool,                  // 默认 true
    pub checkpoint_interval_secs: u32,        // 默认 10
    pub checkpoint_interval_bytes: u64,       // 默认 10MB
    pub verify_checksum: bool,                // 默认 false

    // 重试
    pub max_retry_attempts: u32,              // 默认 3
    pub retry_delay_ms: u64,                  // 默认 1000
}
```

## 13. 错误处理

### 13.1 错误分类

```rust
pub enum TransferError {
    // 可重试错误
    TemporaryNetwork(String),
    Timeout(String),
    WouldBlock,

    // 不可重试错误
    PermissionDenied(String),
    DiskFull(String),
    InvalidPath(String),

    // 取消
    Cancelled,

    // 断点续传错误
    CheckpointMismatch(String),
    CannotResume(String),

    // 其他
    Unknown(String),
}
```

### 13.2 重试策略

```rust
pub struct RetryStrategy {
    max_attempts: u32,
    base_delay: Duration,
    max_delay: Duration,
    backoff_multiplier: f64,
}

impl RetryStrategy {
    // 指数退避
    pub fn next_delay(&self, attempt: u32) -> Duration {
        let delay = self.base_delay.as_millis() as f64
            * self.backoff_multiplier.powi(attempt as i32);
        Duration::from_millis(delay as u64).min(self.max_delay)
    }
}
```

## 14. 监控与日志

### 14.1 传输事件

```rust
pub enum TransferEvent {
    Started { id: String },
    Progress { id: String, transferred: u64, total: u64 },
    Paused { id: String },
    Resumed { id: String, from_offset: u64 },
    Completed { id: String, duration: Duration },
    Failed { id: String, error: String },
    Cancelled { id: String },
}
```

### 14.2 健康检查

```rust
pub struct TransferHealth {
    active_transfers: usize,
    stuck_transfers: usize,      // 长时间无进度
    failed_transfers: usize,
    avg_speed_bps: f64,
    pool_usage: f64,
}

impl TransferManager {
    pub async fn health_check(&self) -> TransferHealth {
        // 检查活动传输
        // 检测卡住的传输
        // 计算平均速度
        // 检查连接池使用率
    }
}
```

## 15. 总结

这个新架构的核心优势：

1. **职责分离**: SessionManager 和 TransferManager 各司其职
2. **状态机**: 清晰的传输状态，易于调试和监控
3. **断点续传**: 大文件和网络不稳定场景的必备功能
4. **异步 IO**: 更高效的资源利用
5. **独立连接池**: 避免心跳与传输冲突
6. **可扩展性**: 易于添加新功能（如传输队列、优先级等）

预期效果：
- 传输不再卡住（超时 + 健康检查）
- 网络波动不影响传输（独立连接池）
- 大文件传输可靠（断点续传）
- 系统资源占用更少（异步而非线程 spawn）
