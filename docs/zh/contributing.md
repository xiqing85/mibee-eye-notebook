# 为 notebook-cam 做贡献

感谢您有兴趣为 notebook-cam 做贡献！本指南涵盖了您需要了解的一切来开始并为这个基于 Rust 的笔记本电脑监控代理做出有意义的贡献。

## 开始开发

### 前置要求

- **Rust 1.85+** (最低支持版本) - 通过 [rustup](https://rustup.rs/) 安装
- **系统依赖** (Linux):

```bash
# 安装所需的包
sudo apt install libv4l-dev libasound2-dev libclang-dev

# 将用户添加到 video 组以获取摄像头访问权限
sudo usermod -aG video $USER
# 登出并重新登录以使组更改生效
```

### 克隆和构建

```bash
# 克隆仓库
git clone https://https://github.com/xiqing85/notebook-cam.git
cd notebook-cam

# 构建项目
cargo build

# 运行测试
cargo test

# 构建发布版本
cargo build --release
```

### 开发工作流

1. **配置您的开发环境**:

```bash
# 复制默认配置用于本地开发
cp config.toml config.local.toml

# 编辑 config.local.toml 以配置您的开发环境
# 这个文件被 git 忽略，不会被提交
```

2. **运行应用程序**:

```bash
# 使用您的本地配置运行
cargo run -- --config config.local.toml

# 使用发布版本构建进行性能测试
cargo run --release -- --config config.local.toml

# 重置密码 (CLI 工具)
cargo run -- --reset-password
```

3. **使用工作区别名进行开发**:

```bash
# 使用严格的 clippy 规则进行代码检查
cargo ci-clippy

# 使用详细输出运行测试
cargo test-verbose

# 检查格式
cargo fmt-check

# 格式化代码
cargo fmt
```

## 项目结构

```
notebook-cam/
├─ src/                    # 二进制入口点，配置，类型，错误处理
│  ├─ main.rs             # 应用程序入口点
│  ├─ config.rs           # 配置管理，使用 TOML
│  ├─ types.rs           # 核心领域类型 (CameraId, StreamId, CameraType)
│  └─ error.rs           # 统一的错误枚举 (thiserror)
├─ crates/                # 工作区 crate
│  ├─ protocols/         # RTSP, RTMP, ONVIF, GB28181, RTP, H.264 (11.4k 行)
│  ├─ streaming/         # StreamHub 分发，源/输出适配器，MiBee 客户端 (4.1k 行)
│  ├─ web/              # Axum REST API + 内嵌 SPA + TLS (2.5k 行)
│  ├─ security/         # 认证，TLS，加密，速率限制 (1.9k 行)
│  ├─ capture/           # 视频 (nokhwa) + 音频 (cpal) 采集 (800 行)
│  └─ observability/    # tracing + OTel + Prometheus 指标 (423 行)
├─ migrations/           # SQLite 模式 (cameras, settings, stream_sessions)
└─ tls/                  # 开发 TLS 证书
```

### 核心概念

- **摄像头类型**: `Usb`, `Rtsp`, `Onvif`, `Gb28181`, `Rtmp`
- **媒体管道**: Sources → BufferPool → StreamHub → Outputs
- **资源管理**: 信号量保护的并发性（最多 16 个流）
- **安全性**: 全面的 TLS，基于会话的认证
- **可观察性**: 结构化日志，指标，分布式追踪

有关详细的架构文档，请参见 [docs/zh/architecture.md](architecture.md)。

## 代码约定

### Rust 版本和风格

- **Rust 2024 版本**，最低支持版本 1.85
- 优先使用 `async fn` 而不是 `fn -> impl Future`
- 使用 `tokio` 进行所有异步操作
- 绝不使用阻塞式网络 I/O

### 测试约定

#### 单元测试

在每个文件底部使用标准模式放置单元测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_example_logic() {
        // 测试实现
        assert_eq!(2 + 2, 4);
    }

    #[tokio::test]
    async fn test_async_behavior() {
        // 异步测试实现
        let result = some_async_function().await;
        assert!(result.is_ok());
    }
}
```

#### 模拟模式

使用 `MockSource` 和 `MockOutput` 来测试协议适配器：

```rust
// 来自 crates/streaming/src/source.rs
pub(crate) struct MockSource {
    frames: Vec<MediaFrame>,
    started: bool,
    index: usize,
}

impl Source for MockSource {
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            self.started = true;
            Ok(())
        })
    }

    fn next_frame(&mut self) -> Pin<Box<dyn Future<Output = Result<MediaFrame>> + Send + '_>> {
        Box::pin(async move {
            if !self.started {
                anyhow::bail!("MockSource 未启动");
            }
            if self.index >= self.frames.len() {
                anyhow::bail!("MockSource 已耗尽");
            }
            let frame = self.frames[self.index].clone();
            self.index += 1;
            Ok(frame)
        })
    }

    fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            self.started = false;
            Ok(())
        })
    }
}
```

#### 数据库测试

对于数据库相关代码，使用内存 SQLite 和嵌入的迁移：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        // 为测试手动应用迁移
        conn.execute_batch(include_str!("../../../migrations/001_initial.sql")).unwrap();
        conn
    }

    #[test]
    fn test_camera_crud() {
        let conn = test_db();
        // 测试数据库操作
    }
}
```

### 序列化约定

所有公共类型必须实现 `Serialize`/`Deserialize` 并进行往返测试：

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CameraId(pub String);

#[test]
fn test_camera_id_serde_roundtrip() {
    let id = CameraId::new();
    let json = serde_json::to_string(&id).unwrap();
    let back: CameraId = serde_json::from_str(&json).unwrap();
    assert_eq!(back, id);
}
```

### 格式化和代码检查

在提交之前始终运行这些命令：

```bash
# 格式化代码
cargo fmt

# 检查格式
cargo fmt-check

# 运行严格的 clippy 代码检查
cargo ci-clippy
```

## 添加协议

### 概览

要添加新的协议支持（例如 WebRTC、SRT 或自定义协议），请按照以下步骤操作：

### 第 1 步：添加协议模块

1. 在 `crates/protocols/src/` 中创建一个新模块：

```rust
// crates/protocols/src/new_protocol.rs
pub struct NewProtocolClient {
    // 客户端实现
}

impl NewProtocolClient {
    pub fn new(config: &NewProtocolConfig) -> Result<Self> {
        // 构造函数逻辑
    }

    pub async fn connect(&mut self) -> Result<()> {
        // 连接逻辑
    }

    pub async fn receive_frame(&mut self) -> Result<MediaFrame> {
        // 帧接收逻辑
    }
}
```

2. 在 `crates/protocols/src/lib.rs` 中导出该模块：

```rust
// crates/protocols/src/lib.rs
pub mod new_protocol;
```

### 第 2 步：创建源和输出适配器

在 `crates/streaming/src/` 中添加源和输出适配器：

```rust
// crates/streaming/src/source.rs - 添加 NewProtocolSource
pub struct NewProtocolSource {
    client: protocols::new_protocol::NewProtocolClient,
    started: bool,
}

impl Source for NewProtocolSource {
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            self.client.connect().await?;
            self.started = true;
            Ok(())
        })
    }

    fn next_frame(&mut self) -> Pin<Box<dyn Future<Output = Result<MediaFrame>> + Send + '_>> {
        Box::pin(async move {
            if !self.started {
                anyhow::bail!("NewProtocolSource 未启动");
            }
            self.client.receive_frame().await
        })
    }

    fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            // 清理逻辑
            self.started = false;
            Ok(())
        })
    }
}
```

### 第 3 步：添加 CameraType 变体

更新 `src/types.rs` 以包含您的新的摄像头类型：

```rust
// src/types.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraType {
    Usb,
    Rtsp,
    Onvif,
    Gb28181,
    Rtmp,
    NewProtocol,  // 在这里添加您的新的类型
}
```

不要忘记添加相应的测试：

```rust
#[test]
fn test_camera_type_new_protocol() {
    let json = serde_json::to_string(&CameraType::NewProtocol).unwrap();
    assert_eq!(json, "\"new_protocol\"");
    let back: CameraType = serde_json::from_str(&json).unwrap();
    assert_eq!(back, CameraType::NewProtocol);
}
```

### 第 4 步：连接 API 路由

在 `crates/web/src/routes/` 中添加端点：

```rust
// crates/web/src/routes/new_protocol.rs
use axum::{extract::Query, Json};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct DiscoverQuery {
    timeout: Option<u64>,
}

#[derive(Serialize)]
pub struct DiscoveryResult {
    devices: Vec<NewProtocolDeviceInfo>,
}

pub async fn discover_new_protocol(
    Query(params): Query<DiscoverQuery>,
) -> Result<Json<DiscoveryResult>, AppError> {
    // 发现逻辑
    let devices = protocols::new_protocol::discover_devices(
        params.timeout.unwrap_or(5)
    ).await?;

    Ok(Json(DiscoveryResult { devices }))
}
```

在 `crates/web/src/routes/mod.rs` 中注册路由：

```rust
// crates/web/src/routes/mod.rs
pub mod new_protocol;

// 在 all_routes() 函数中
pub fn all_routes() -> Router<AppState> {
    Router::new()
        // ... 现有路由
        .route("/api/new_protocol/discover", get(new_protocol::discover_new_protocol))
}
```

### 第 5 步：更新配置

将协议特定的配置选项添加到您的配置结构中，并更新默认配置。

### 参考实现

查看现有协议作为参考：

- **ONVIF**: `crates/protocols/src/onvif.rs` - 使用 oxvif 包装器
- **GB28181**: `crates/protocols/src/gb28181.rs` - 使用 gmv 包装器  
- **RTSP**: `crates/protocols/src/rtsp.rs` - 手写的客户端
- **RTMP**: `crates/protocols/src/rtmp/mod.rs` - 手写的服务器

## 添加摄像头类型

### CameraType 枚举

摄像头类型在 `src/types.rs` 中定义，代表 notebook-cam 可以处理的不同的源：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraType {
    Usb,           // 通过 V4L2/MSMF 的本地摄像头
    Rtsp,          // 网络 RTSP 摄像头
    Onvif,         // ONVIF 兼容的 IP 摄像头
    Gb28181,       // 符合 GB/T 28181 标准的摄像头
    Rtmp,          // RTMP 推送源
    // 在这里添加新的类型
}
```

### 添加新的摄像头类型

1. **将变体添加到 CameraType 枚举**（如上所示）

2. **创建源适配器** 实现 `Source` trait：

```rust
// crates/streaming/src/source.rs
pub struct YourCameraSource {
    // 摄像头特定配置
    config: YourCameraConfig,
    // 摄像头连接句柄
    connection: Option<YourCameraConnection>,
    started: bool,
}

impl Source for YourCameraSource {
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            // 连接到摄像头
            let conn = YourCameraConnection::connect(&self.config).await?;
            self.connection = Some(conn);
            self.started = true;
            Ok(())
        })
    }

    fn next_frame(&mut self) -> Pin<Box<dyn Future<Output = Result<MediaFrame>> + Send + '_>> {
        Box::pin(async move {
            if !self.started {
                anyhow::bail!("YourCameraSource 未启动");
            }
            let conn = self.connection.as_mut()
                .ok_or_else(|| anyhow::anyhow!("没有连接"))?;
            conn.receive_frame().await
        })
    }

    fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            self.connection = None;
            self.started = false;
            Ok(())
        })
    }
}
```

3. **在 API 路由中注册**：

在 `crates/web/src/routes/cameras.rs` 中为您的摄像头类型添加 CRUD 端点。

4. **更新配置模式**：

将您的摄像头特定的配置选项添加到适当的配置结构中。

## 测试策略

### 单元测试

每个模块都应该有全面的单元测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_function_logic() {
        // 使用已知输入测试核心逻辑
    }

    #[tokio::test]
    async fn test_async_behavior() {
        // 测试异步操作
    }
}
```

### 集成测试

#### 协议适配器测试

使用真实摄像头（如果可用）和模拟实现来测试协议适配器：

```rust
#[tokio::test]
async fn test_rtsp_integration() {
    let mut source = RtspSource::new("rtsp://test-camera:554/stream", "user", "pass");
    source.start().await.unwrap();

    let frame = source.next_frame().await.unwrap();
    assert!(matches!(frame, MediaFrame::Video { .. }));

    source.stop().await.unwrap();
}
```

#### 资源耗尽测试

测试资源控制器限制：

```rust
#[tokio::test]
async fn test_max_concurrent_streams() {
    // 测试信号量正确限制并发流
    // 尝试启动超过 16 个流并验证失败
}
```

### 测试运行

- **所有测试**: `cargo test`
- **特定 crate**: `cargo test -p protocols`
- **详细输出**: `cargo test-verbose`（显示详细的测试输出）
- **过滤测试**: `cargo test test_name`
- **仅运行失败的测试**: `cargo test --lib -- --test-threads=1`

### 测试覆盖率

追求高测试覆盖率，特别是对于：
- 协议解析逻辑
- 错误处理路径
- 资源管理
- 数据库操作
- 认证流程

## 提交约定

### 提交消息格式

使用常规提交格式：

```
type(scope): description
```

### 提交类型

- `feat`: 新功能
- `fix`: 错误修复
- `docs`: 文档更改
- `refactor`: 既不修复错误也不添加功能的代码更改
- `test`: 添加或修复测试
- `chore`: 构建过程或辅助工具更改

### 示例

```
feat(protocols): 添加 WebRTC 摄像头支持
fix(streaming): 正确处理连接超时
docs(contributing): 更新开发设置说明
refactor(config): 简化配置加载逻辑
test(rtsp): 为错误场景添加集成测试
chore(ci): 更新 GitHub Actions 工作流
```

### 提交指南

1. **保持提交专注**: 每个提交应该解决单个逻辑更改
2. **编写清晰的描述**: 描述更改内容和原因
3. **包含破坏性更改说明**: 对于不兼容的更改使用 `BREAKING CHANGE:` 页脚
4. **引用问题**: 使用 `Closes #123` 链接到 GitHub 问题
5. **提交前测试**: 确保所有测试通过且 `cargo ci-clippy` 通过

## Pull Request 流程

### 打开 PR 前

1. **从适当的基分支创建功能分支**
2. **运行完整测试套件**：
   ```bash
   cargo test
   cargo ci-clippy
   cargo fmt-check
   ```
3. **如果您的更改影响 API 或面向用户的功能，请更新文档**
4. **为新功能或错误修复添加测试**

### PR 描述模板

为您的 pull request 使用此模板：

```markdown
## 描述
对所做更改的简短描述。

## 所做的更改
- [ ] 添加了新协议支持
- [ ] 修复了 X 中的错误
- [ ] 更新了文档
- [ ] 为 Y 添加了测试

## 测试
- [ ] 单元测试通过
- [ ] 集成测试通过
- [ ] 手动测试完成
- [ ] 评估了性能影响（如果适用）

## 破坏性更改
- [ ] 无
- [ ] 在这里列出破坏性更改

## 相关问题
Closes #123
相关到 #456
```

### PR 审查流程

1. **自动检查**: CI 将运行测试和代码检查
2. **代码审查**: 等待至少一个维护者审查
3. **请求的更改**: 处理所有审查评论
4. **最终批准**: 从维护者获得批准
5. **合并**: CI 通过后维护者将合并

### 合并后

- 删除您的功能分支
- 更新您的本地主分支
- 继续处理下一个功能

## 重要注意事项

### 1. Linux 上的 nokhwa

**问题**: nokhwa 需要特定的系统依赖和用户权限。

**解决方案**:
```bash
# 安装所需的包
sudo apt install libv4l-dev input-native

# 将用户添加到 video 组
sudo usermod -aG video $USER
# 登出并重新登录以使组更改生效
```

**绝不要假设 root/sudo** - 为特权操作输出确切的命令。

### 2. cpal ALSA 音频回调

**问题**: 音频回调绝不能阻塞 - 使用非阻塞操作。

**错误模式**（不要这样做）：
```rust
// 错误：在回调中阻塞
fn audio_callback(data: &mut [f32]) {
    let result = some_blocking_operation(); // 这会导致音频中断
    // ...
}
```

**正确模式**：
```rust
// 正确：使用通道进行非阻塞通信
let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

fn audio_callback(data: &mut [f32]) {
    if let Err(e) = tx.try_send(data.to_vec()) {
        tracing::warn!("音频通道已满: {}", e);
    }
}
```

### 3. GB/T 28181 PS → H.264 转换

**问题**: GB28181 摄像头发送 MPEG-2 Program Stream，必须解码为 H.264 才能在 Web 上播放。

**解决方案**: 使用 `gmv` crate 自动处理转换：

```rust
// GB28181 客户端内部处理 PS → H.264 转换
let client = Gb28181Client::new(config);
let nal_units = client.receive_ps_and_convert_to_nal().await?;
```

**浏览器兼容性**: H.265 在浏览器中并非普遍支持 - 总是回退到 H.264。

### 4. ONVIF WS-Discovery

**问题**: ONVIF 发现使用 UDP 多播端口 3702，可能需要原始套接字访问。

**要求**:
```bash
# 可能需要 CAP_NET_RAW 能力或 root 访问权限
# 或使用抽象化此功能的库
```

**替代方案**: 使用 oxvif 包装器自动处理发现。

### 5. WebRTC Rust 生态系统

**问题**: Rust WebRTC 生态系统尚不成熟，不适合生产使用。

**解决方案**: 通过 MiBee NVR WHEP 端点代理：

```rust
// 而不是直接的 WebRTC 实现
let whep_endpoint = "https://mibee-nvr.example.com/whep";
// 通过 MiBee NVR 使用 WHEP 协议
```

### 6. H.265 浏览器兼容性

**问题**: H.265 (HEVC) 在 Web 浏览器中并非普遍支持。

**解决方案**: 总是提供 H.264 回退：

```rust
// 在流式传输端点，检查客户端支持
if client_supports_h265 && source_has_h265 {
    stream_h265();
} else {
    // 转换/转码为 H.264
    stream_h264();
}
```

### 7. 特权端口 (< 1024)

**问题**: 绑定到 < 1024 的端口需要 root 权限或特殊能力。

**解决方案**:
- **使用高端口**: Web UI 在 8443，RTSP 在 8554，RTMP 在 1935
- **或使用 setcap**: `sudo setcap 'cap_net_bind_service=+ep' /path/to/binary`

## 开发命令参考

### 构建命令

```bash
cargo build                                # 调试构建
cargo build --release                      # 发布构建
cargo run -- --config config.toml          # 使用配置运行
cargo run -- --reset-password              # 密码重置 CLI
```

### 测试命令

```bash
cargo test                                 # 运行所有测试
cargo test -p protocols                    # 测试特定 crate
cargo test-verbose                         # 带详细输出的测试
cargo test test_name                      # 运行特定测试
```

### 代码质量命令

```bash
cargo ci-clippy                            # 严格的 clippy 代码检查 (-D warnings)
cargo fmt-check                            # 检查格式
cargo fmt                                  # 格式化代码
```

### 工作区别名

工作区定义了这些便捷别名：

- `cargo ci-clippy` - 运行带有 `-D warnings` 的 clippy（严格代码检查）
- `cargo fmt-check` - 检查代码是否正确格式化而不进行更改

**注意**: `cargo test-verbose` 是运行测试并显示详细输出的标准 cargo 命令。

## 资源

- [Rust 书](https://doc.rust-lang.org/book/)
- [Tokio 异步运行时](https://tokio.rs/docs/)
- [Axum Web 框架](https://docs.rs/axum)
- [SQLite 与 Rust](https://docs.rs/rusqlite)
- [Serde 序列化](https://serde.rs/)

## 获取帮助

- [GitHub Issues](https://https://github.com/xiqing85/notebook-cam/issues)
- [文档](https://https://github.com/xiqing85/notebook-cam/docs)
- [项目 Discord/社区] (如果可用)

祝编码愉快！🚀