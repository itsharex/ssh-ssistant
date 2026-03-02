# SSH 文件上传下载实现代码地图

## 1. 概述

本文档详细分析了 SSH Assistant 项目中文件上传下载的实现代码，包括可能导致卡住的原因分析。

## 2. 文件上传下载的完整流程

### 2.1 上传流程 (Upload)

```
Frontend (Tauri Command)
    ↓
[1] upload_file() (file_ops.rs:679-886)
    ├─ 创建 Transfer 对象
    ├─ 创建 TransferState (包含 cancel_flag)
    ├─ 保存到 AppState.transfers
    └─ 发送 SshCommand::SftpUpload
         ↓
[2] SshManager::handle_command() (manager.rs:574-594)
    └─ spawn thread → bg_sftp_upload()
         ↓
[3] bg_sftp_upload() (manager.rs:914-1000)
    ├─ 获取 background session
    ├─ 打开本地文件
    ├─ 创建远程 SFTP 文件
    ├─ 循环读写传输
    │   ├─ 检查 cancel_flag
    │   ├─ local.read() → 读取本地数据
    │   ├─ remote.write() → 写入远程数据
    │   ├─ 发送进度事件
    │   └─ 处理 WouldBlock 错误
    └─ 完成/错误处理
```

### 2.2 下载流程 (Download)

```
Frontend (Tauri Command)
    ↓
[1] download_file() (file_ops.rs:449-677)
    ├─ 创建 Transfer 对象
    ├─ 创建 TransferState (包含 cancel_flag)
    ├─ 保存到 AppState.transfers
    └─ 发送 SshCommand::SftpDownload
         ↓
[2] SshManager::handle_command() (manager.rs:553-573)
    └─ spawn thread → bg_sftp_download()
         ↓
[3] bg_sftp_download() (manager.rs:839-912)
    ├─ 获取 background session
    ├─ 打开远程 SFTP 文件
    ├─ 创建本地文件
    ├─ 循环读写传输
    │   ├─ 检查 cancel_flag
    │   ├─ remote.read() → 读取远程数据
    │   ├─ local.write_all() → 写入本地数据
    │   ├─ 发送进度事件
    │   └─ 处理 WouldBlock 错误
    └─ 完成/错误处理
```

## 3. 关键函数职责和调用关系

### 3.1 核心文件操作函数

#### file_ops.rs
- **upload_file()**: 上传入口，创建传输状态，发送命令
- **download_file()**: 下载入口，创建传输状态，发送命令
- **read_remote_file()**: 读取远程文件内容
- **write_remote_file()**: 写入远程文件内容
- **list_files()**: 列出远程目录
- **delete_item()**: 删除远程文件/目录
- **rename_item()**: 重命名远程文件

#### manager.rs
- **bg_sftp_upload()**: 后台上传实现 (L914-1000)
- **bg_sftp_download()**: 后台下载实现 (L839-912)
- **bg_sftp_read()**: 后台读取文件 (L699-738)
- **bg_sftp_write()**: 后台写入文件 (L740-783)
- **bg_sftp_ls()**: 后台列出目录 (L651-697)
- **bg_sftp_delete()**: 后台删除 (L795-806)
- **bg_sftp_rename()**: 后台重命名 (L830-837)
- **create_remote_dir_recursive()**: 递归创建远程目录 (L1002-1014)

#### utils.rs
- **ssh2_retry()**: SSH2 操作重试包装器 (L12-35)
  - 最多重试 5 次
  - 指数退避：20ms → 40ms → 80ms → 160ms → 320ms
  - 仅处理 ErrorCode::Session(-37) (EAGAIN/WouldBlock)
- **get_sftp_buffer_size()**: 获取 SFTP 缓冲区大小 (L52-60)
  - 默认 512KB
  - 可从设置读取
- **get_remote_file_hash()**: 获取远程文件哈希 (L62-134)
- **compute_local_file_hash()**: 计算本地文件哈希 (L136-163)

### 3.2 连接和会话管理

#### connection.rs
- **SessionSshPool**: 会话池管理
  - get_background_session(): 获取后台会话
  - get_ai_session(): 获取 AI 会话
  - rebuild_all(): 重建所有连接
  - heartbeat_check(): 心跳检查

#### client.rs
- **AppState**: 全局应用状态
  - clients: SSH 客户端映射
  - transfers: 传输状态映射
  - command_cancellations: 命令取消标志

#### heartbeat.rs
- **HeartbeatManager**: 分层心跳检测
  - perform_heartbeat(): 执行心跳检测
  - check_tcp(): TCP 层检查
  - check_ssh(): SSH 层检查
  - check_app(): 应用层检查 (执行 echo hb)

#### network_monitor.rs
- **NetworkMonitor**: 网络质量监控
  - measure_latency(): 测量延迟
  - estimate_bandwidth(): 估算带宽
  - get_recommended_params(): 获取推荐参数

#### reconnect.rs
- **ReconnectManager**: 重连管理器
  - 指数退避算法
  - 永久错误不重试
  - 速率限制错误延长延迟

#### error_classifier.rs
- **SshErrorClassifier**: 错误分类器
  - Temporary: 临时错误，应该重试
  - Permanent: 永久错误，不应重试
  - RateLimited: 速率限制，延长延迟重试
  - ResourceExhausted: 资源耗尽，退避重试

## 4. 可能导致卡住的代码位置

### 4.1 无超时保护的操作

#### 问题 1: SFTP 读写循环没有整体超时
**位置**: `manager.rs:bg_sftp_download()` (L867-901) 和 `bg_sftp_upload()` (L948-989)

**问题代码**:
```rust
loop {
    if cancel_flag.load(Ordering::Relaxed) {
        return Err("Cancelled".to_string());
    }

    // 获取锁，读取一小块数据，然后立即释放锁
    let read_res = {
        let session = session_mutex.lock().map_err(|e| e.to_string())?;
        remote.read(&mut buf)
    };

    match read_res {
        Ok(0) => break,
        Ok(n) => { /* 处理数据 */ }
        Err(e) if e.kind() == ErrorKind::WouldBlock => {
            thread::sleep(Duration::from_millis(5)); // ← 可能无限循环
        }
        Err(e) => return Err(e.to_string()),
    }
}
```

**问题分析**:
- 如果网络中断但 SSH 连接未断开，read() 可能持续返回 WouldBlock
- 没有整体超时机制，可能无限重试
- WouldBlock 时仅 sleep 5ms，然后继续，没有超时检查

**风险等级**: 高
**影响场景**:
- 网络突然中断
- 服务器负载过高无响应
- 中间设备（防火墙/路由）问题

---

#### 问题 2: ssh2_retry() 仅重试 EAGAIN 错误
**位置**: `utils.rs:ssh2_retry()` (L12-35)

**问题代码**:
```rust
pub fn ssh2_retry<F, T>(mut f: F) -> Result<T, ssh2::Error>
where
    F: FnMut() -> Result<T, ssh2::Error>,
{
    const MAX_RETRIES: u32 = 5;
    const BASE_DELAY_MS: u64 = 20;

    for attempt in 0..=MAX_RETRIES {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) => {
                // 仅重试 EAGAIN (-37)
                if e.code() == ssh2::ErrorCode::Session(-37) && attempt < MAX_RETRIES {
                    let delay_ms = BASE_DELAY_MS * (1 << attempt.min(4));
                    thread::sleep(Duration::from_millis(delay_ms));
                    continue;
                }
                return Err(e);
            }
        }
    }
}
```

**问题分析**:
- 仅处理 Session(-37) 错误
- 其他超时相关错误（如 Timeout）不被重试
- 没有整体超时时间
- 对于某些操作，5 次重试可能不够

**风险等级**: 中
**影响场景**:
- 网络延迟高
- 服务器响应慢
- 大文件传输

---

#### 问题 3: SFTP 文件打开没有超时
**位置**: `manager.rs:bg_sftp_download()` (L854) 和 `bg_sftp_upload()` (L940)

**问题代码**:
```rust
let mut remote = crate::ssh::utils::ssh2_retry(|| sftp.open(Path::new(remote_path)))
    .map_err(|e| e.to_string())?;
```

**问题分析**:
- sftp.open() 可能阻塞
- ssh2_retry() 最多等待约 620ms (20+40+80+160+320)
- 对于慢速网络，可能不够
- 没有应用层超时控制

**风险等级**: 中
**影响场景**:
- 打开大文件
- 网络延迟高
- 服务器负载高

---

#### 问题 4: 进度事件发送可能阻塞
**位置**: `manager.rs:bg_sftp_download()` (L885-893) 和 `bg_sftp_upload()` (L972-980)

**问题代码**:
```rust
if last_emit.elapsed().as_millis() > 100 {
    let _ = app.emit(
        "transfer-progress",
        ProgressPayload {
            id: transfer_id.to_string(),
            transferred,
            total,
        },
    );
    last_emit = Instant::now();
}
```

**问题分析**:
- app.emit() 可能阻塞（虽然用了 let _ = 忽略错误）
- 如果前端事件处理慢，可能影响传输性能
- 没有超时保护

**风险等级**: 低
**影响场景**:
- 前端卡顿
- 事件通道满

---

### 4.2 死锁可能性分析

#### 死锁风险 1: session_mutex 锁竞争
**位置**: `manager.rs:bg_sftp_download()` (L874-876) 和 `bg_sftp_upload()` (L961-963)

**问题代码**:
```rust
let read_res = {
    let session = session_mutex.lock().map_err(|e| e.to_string())?;
    remote.read(&mut buf)
};
```

**问题分析**:
- 每次 read/write 都获取锁
- 如果另一个线程持有锁（如心跳检测），可能等待
- 虽然锁持有时间短，但在高并发下可能有问题
- remote 对象依赖 session，必须保持锁有效

**风险等级**: 低-中
**影响场景**:
- 同时进行多个文件操作
- 心跳检测与传输冲突

---

#### 死锁风险 2: channel.recv() 阻塞
**位置**: `file_ops.rs:download_file()` (L543-581) 和 `upload_file()` (L773-796)

**问题代码**:
```rust
match rx.recv() {
    Ok(Ok(_)) => { /* 成功 */ }
    Ok(Err(e)) => { /* 错误 */ }
    Err(_) => { /* 通道关闭 */ }
}
```

**问题分析**:
- rx.recv() 会阻塞直到收到响应
- 如果 SshManager 线程崩溃或卡住，前端会永久等待
- 没有超时机制

**风险等级**: 高
**影响场景**:
- Manager 线程无响应
- 传输卡住

---

### 4.3 网络中断处理

#### 问题 1: 心跳检测与传输冲突
**位置**: `manager.rs:run()` 主循环 (L243-393)

**流程**:
1. 主循环同时处理:
   - 接收命令 (L254-262)
   - 读取 shell 输出 (L271-306)
   - 执行心跳检测 (L309-338)
   - 触发延迟检测 (L312-314)
   - 处理心跳结果 (L341-377)

2. 心跳检测执行:
   - TCP 层: keepalive_send()
   - SSH 层: channel_session() + close()
   - 应用层: exec("echo hb") + read()

**问题分析**:
- 心跳检测和文件传输使用同一个 session
- 应用层心跳 (exec) 可能与 SFTP 操作冲突
- 如果心跳失败，触发重连，可能中断传输

**风险等级**: 中
**影响场景**:
- 长时间传输
- 网络不稳定

---

#### 问题 2: 重连可能中断传输
**位置**: `manager.rs:handle_heartbeat_action()` (L341-377)

**问题代码**:
```rust
HeartbeatAction::ReconnectBackground => {
    eprintln!("[Heartbeat] Attempting background reconnection...");
    if let Err(e) = self.pool.rebuild_all() {
        eprintln!("[Heartbeat] Background reconnect failed: {}", e);
    } else {
        self.heartbeat_manager.reset();
    }
}
```

**问题分析**:
- rebuild_all() 会重建所有后台连接
- 正在进行的传输使用的是旧 session
- 旧 session 失效会导致传输失败
- 没有通知传输线程会话已变更

**风险等级**: 高
**影响场景**:
- 传输期间网络波动
- 心跳失败触发重连

---

#### 问题 3: WouldBlock 错误处理不当
**位置**: 所有 SFTP 读写循环

**问题代码**:
```rust
Err(e) if e.kind() == ErrorKind::WouldBlock => {
    thread::sleep(Duration::from_millis(5));
}
```

**问题分析**:
- WouldBlock 可能表示:
  - 非阻塞模式下没有数据可读（正常）
  - 网络中断（异常）
- 代码无法区分这两种情况
- 持续 WouldBlock 可能表示连接已死
- 没有计数器或超时来检测异常

**风险等级**: 高
**影响场景**:
- 网络突然中断
- SSH 连接半开状态

---

### 4.4 缓冲区大小问题

#### 问题 1: 固定缓冲区大小
**位置**: `manager.rs:bg_sftp_download()` (L863) 和 `bg_sftp_upload()` (L944)

**问题代码**:
```rust
// 下载
let mut buf = [0u8; 16384]; // 固定 16KB

// 上传
let buffer_size = crate::ssh::utils::get_sftp_buffer_size(Some(app));
let mut buf = vec![0u8; buffer_size]; // 可配置，但默认 512KB
```

**问题分析**:
- 下载缓冲区固定 16KB，太小
- 上传缓冲区可配置，但下载不是
- 缓冲区小可能导致更多系统调用
- 缓冲区大可能导致内存问题和单次操作时间长

**风险等级**: 中
**影响场景**:
- 大文件传输效率低
- 小文件传输开销大

---

#### 问题 2: 进度更新频率固定
**位置**: `manager.rs:bg_sftp_download()` (L884) 和 `bg_sftp_upload()` (L971)

**问题代码**:
```rust
if last_emit.elapsed().as_millis() > 100 {
    // 发送进度
}
```

**问题分析**:
- 每 100ms 发送一次进度
- 对于慢速网络，可能太频繁
- 对于快速网络，可能太稀疏
- 没有根据传输量自适应

**风险等级**: 低
**影响场景**:
- 大量并发传输
- 前端性能

---

## 5. 当前超时和错误处理现状

### 5.1 超时配置

#### ConnectionTimeoutSettings (models.rs:91-97)
```rust
pub struct ConnectionTimeoutSettings {
    pub connection_timeout_secs: u32,        // 默认 30
    pub jump_host_timeout_secs: u32,         // 默认 15
    pub local_forward_timeout_secs: u32,     // 默认 10
    pub command_timeout_secs: u32,           // 默认 30
    pub sftp_operation_timeout_secs: u32,    // 默认 300 (5分钟)
}
```

**问题**:
- sftp_operation_timeout_secs 定义了，但未在代码中使用
- 没有应用到文件传输的超时

---

#### HeartbeatSettings (models.rs:135-141)
```rust
pub struct HeartbeatSettings {
    pub tcp_keepalive_interval_secs: u32,      // 默认 60
    pub ssh_keepalive_interval_secs: u32,      // 默认 15
    pub app_heartbeat_interval_secs: u32,      // 默认 30
    pub heartbeat_timeout_secs: u32,           // 默认 5
    pub failed_heartbeats_before_action: u32,  // 默认 3
}
```

**使用情况**:
- 心跳检测有超时 (5秒)
- 但不应用于文件传输

---

### 5.2 错误处理现状

#### 已实现的处理:
1. **ssh2_retry()**: 重试 EAGAIN 错误
2. **WouldBlock 处理**: sleep 后继续
3. **错误分类**: SshErrorClassifier 区分临时/永久错误
4. **重连机制**: ReconnectManager 指数退避
5. **取消标志**: cancel_flag 允许取消操作

#### 缺失的处理:
1. **整体传输超时**: 没有
2. **单次操作超时**: 部分有（心跳），但没有应用在 SFTP
3. **WouldBlock 计数**: 没有检测持续 WouldBlock
4. **连接状态验证**: 传输前不验证连接健康
5. **部分重传**: 传输失败后无法从断点续传

---

## 6. 关键超时路径分析

### 6.1 可能无限等待的路径

```
1. 文件传输循环
   ├─ bg_sftp_upload() / bg_sftp_download()
   ├─ while read/write 循环
   ├─ 持续 WouldBlock
   └─ 无超时退出

2. Channel 接收
   ├─ rx.recv() (file_ops.rs)
   ├─ 等待传输完成
   └─ 如果 Manager 卡住，永久等待

3. Session 获取
   ├─ pool.get_background_session()
   ├─ 如果池已耗尽/损坏
   └─ 可能返回损坏的 session
```

### 6.2 隐式超时

```
1. ssh2_retry()
   └─ 最多等待 ~620ms (20+40+80+160+320)

2. 心跳检测
   └─ heartbeat_timeout_secs (默认 5秒)

3. 网络延迟检测
   └─ 硬编码 5 秒超时 (network_monitor.rs:70)
```

---

## 7. 推荐修复措施

### 7.1 高优先级

1. **添加传输整体超时**
   - 在 bg_sftp_upload/download 中添加总超时
   - 使用 Instant::now() + Duration 跟踪

2. **添加 WouldBlock 计数器**
   - 连续 N 次 WouldBlock 后认为连接有问题
   - 超过阈值主动退出或重连

3. **添加 channel recv 超时**
   - 使用 recv_timeout() 替代 recv()
   - 或在单独线程中接收

4. **重连前通知传输**
   - 重建连接前设置标志
   - 传输线程检测标志后主动退出

### 7.2 中优先级

1. **统一缓冲区大小配置**
   - 下载也使用 get_sftp_buffer_size()

2. **应用 sftp_operation_timeout**
   - 将配置的超时应用到实际操作

3. **改进错误恢复**
   - 区分临时中断和永久错误
   - 实现断点续传

### 7.3 低优先级

1. **优化进度更新频率**
   - 基于传输量而非时间
   - 自适应调整

2. **添加传输状态监控**
   - 检测长时间无进度
   - 自动卡住的传输

---

## 8. 测试建议

### 8.1 卡住场景测试

1. **网络中断测试**
   - 传输中拔网线
   - 传输中禁用网卡
   - 验证是否能在合理时间内检测并退出

2. **服务器无响应测试**
   - 使用 iptables DROP 包
   - 服务器高负载
   - 验证超时机制

3. **慢速网络测试**
   - 使用 tc 模拟延迟
   - 验证进度更新和超时

4. **并发传输测试**
   - 同时上传/下载多个文件
   - 验证锁竞争和资源管理

### 8.2 压力测试

1. **大文件传输**
   - 1GB+ 文件
   - 验证内存使用和性能

2. **大量小文件**
   - 1000+ 文件
   - 验证连接池和会话管理

3. **长时间运行**
   - 连续运行数小时
   - 验证内存泄漏和资源释放

---

## 9. 总结

### 9.1 主要问题

1. **缺少整体超时机制** - 可能导致无限等待
2. **WouldBlock 处理不当** - 无法区分正常和异常情况
3. **重连与传输冲突** - 重连可能导致传输失败
4. **channel.recv 阻塞** - 前端可能永久等待

### 9.2 当前优势

1. **完善的错误分类** - SshErrorClassifier 设计良好
2. **分层心跳检测** - TCP/SSH/应用 三层检测
3. **指数退避重连** - ReconnectManager 实现合理
4. **取消机制** - cancel_flag 允许主动取消

### 9.3 修复优先级

1. **立即修复**: 添加整体超时、WouldBlock 计数
2. **短期**: channel 超时、重连通知
3. **中期**: 断点续传、自适应参数
4. **长期**: 完整的传输状态机、健康监控
