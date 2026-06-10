# MiBee Rec

[English](README.md) · [文档](docs/zh/)

基于 Rust 构建的专业本地采集代理。

采集本机摄像头和麦克风，编码为 H.264/AAC，通过 RTSP 服务端 / RTMP 推流 / ONVIF 设备端 / GB/T 28181 设备端 向外部 NVR 提供流媒体服务。属于 [MiBee](https://https://github.com/xiqing85) 生态系统的一部分。

## 功能特性

- **本地采集** — 摄像头通过 V4L2（Linux）/ MSMF（Windows），麦克风通过 ALSA / WASAPI
- **对外协议** — RTSP 服务端（客户端拉流）、RTMP 推流、ONVIF 设备端点、GB/T 28181 设备注册
- **H.264 / H.265** — 手写 NAL 单元解析器、关键帧检测、SPS/PPS 提取
- **MiBee NVR 集成** — REST API 客户端、摄像头同步、SSE 事件流
- **Web 界面** — Axum REST API + 嵌入式 SPA、TLS 通过 rustls、基于会话的身份认证
- **资源约束** — 信号量控制的并发（最多 16 路流）、每流内存预算
- **可观测性** — 结构化日志、OpenTelemetry 导出、Prometheus 指标端点
- **低资源占用** — 目标 <5% CPU 空闲占用、<200 MB 内存；尽可能零拷贝

### Crate 职责

| Crate | 代码行数 | 作用 |
|-------|---------|------|
| `protocols` | ~11.4k | RTSP、RTMP、ONVIF、GB28181、RTP、H.264 — 手写编解码器和协议实现 |
| `streaming` | ~4.1k | StreamHub 扇出编排器、源/输出适配器、MiBee NVR 客户端 |
| `web` | ~2.5k | Axum REST API + 嵌入式 SPA + 通过 rustls 的 TLS |
| `security` | ~1.9k | 基于会话的身份认证、速率限制、加密 |
| `capture` | ~800 | 视频（nokhwa）+ 音频（cpal）设备包装器 |
| `observability` | ~423 | 结构化追踪、OpenTelemetry 导出、Prometheus 指标 |

## 架构

```
┌──────────────────────────────────┐
│        Web UI (Axum + SPA)       │
│  REST API · TLS · Auth Session   │
├──────────────────────────────────┤
│       Streaming Hub              │
│  Source → BufferPool → fan-out   │
│  ResourceController (max 16)     │
├──────────────────────────────────┤
│        Protocol Layer            │
│  RTSP · RTMP · ONVIF · GB28181   │
│  RTP · H.264 NAL Parser          │
├──────────────────────────────────┤
│        Capture Layer             │
│  Video (nokhwa) · Audio (cpal)   │
├──────────────────────────────────┤
│   Security · Observability       │
│  Auth · TLS · Tracing · Metrics  │
└──────────────────────────────────┘
```

## 工作空间布局

```
mibee-rec/
├─ src/                # 二进制入口、配置、类型、错误处理
├─ crates/
│  ├─ protocols/       # RTSP、RTMP、ONVIF、GB28181、RTP、H.264
│  ├─ streaming/       # StreamHub 扇出、源/输出适配器、MiBee 客户端
│  ├─ web/             # Axum REST API + 嵌入式 SPA + TLS
│  ├─ security/        # 身份认证、TLS、加密、速率限制
│  ├─ capture/         # 视频 + 音频捕获包装器
│  └─ observability/   # tracing + OTel + Prometheus
├─ migrations/         # SQLite 模式
└─ config.toml         # 默认运行时配置
```

## 协议支持状态

| 协议 | 组件 | 实现方式 | 状态 |
|------|------|---------|------|
| RTSP | 服务端 | 手写（`RtspServer`）— 外部客户端连接拉流 | ✅ |
| RTMP | 推流客户端 | 推送本地流到外部 NVR 接入点 | ✅ |
| ONVIF | 设备端点 | 提供设备信息，让外部 NVR 发现本机 | ✅ |
| GB/T 28181 | 设备端 | 向外部平台注册，收到 INVITE 后推送 RTP | ✅ |
| GB/T 28181 | SIP + RTP | 封装 [gmv](https://crates.io/crates/gmv)（`Gb28181Client`） | ✅ |
| H.264 | NAL 单元解析器 | 手写（`H264Parser`） | ✅ |
| H.265 | 解码 | 浏览器回退到 H.264 | ⚠️ |
| CaptureSource | 采集适配器 | `crates/streaming/src/capture_source.rs` | ✅ |
| 流 → 根绑定 | 线路连接 | root `main.rs` → streaming crate | ✅ |
| 登录/注销 | 会话管理 | 返回 501 | 🚧 桩代码 |

**图例**: ✅ 已实现 · ⚠️ 部分/回退 · ❌ 缺失 · 🚧 桩代码

## 资源目标

| 指标 | 目标 | 机制 |
|------|------|------|
| CPU（空闲） | <5% | 零拷贝 I/O、全异步、无忙循环 |
| 内存 | <200 MB | `BufferPool`（10 MB 池）、每流预算、`ResourceController` |
| 并发流数 | ≤16 | `ResourceController` 中通过 `tokio::sync::Semaphore` 控制 |
| 首帧延迟 | <500 ms | 最小缓冲、急切关键帧检测 |

## 快速开始

```bash
# 克隆并进入
git clone https://github.com/xiqing85/mibee-eye-notebook.git
cd mibee-rec

# 安装系统依赖（Linux）
sudo apt install libv4l-dev libasound2-dev libclang-dev
sudo usermod -aG video $USER
# 注销后重新登录以使组变更生效

# 构建
cargo build --release

# 配置
cp config.toml config.local.toml
# 根据需要编辑 config.local.toml

# 运行
cargo run --release -- --config config.local.toml
```

## 构建

**前置条件（Linux）：**

```bash
# 安装系统依赖
sudo apt install libv4l-dev libasound2-dev libclang-dev

# 将用户添加到 video 组以获取摄像头权限
sudo usermod -aG video $USER
# 注销后重新登录以使组变更生效
```

**构建和运行：**

```bash
cargo build                                # 调试构建
cargo build --release                      # 发布构建
cargo run -- --config config.toml          # 使用配置运行
cargo run -- --reset-password              # 密码重置 CLI
```

**开发：**

```bash
cargo test                                 # 运行所有测试
cargo test -p protocols                    # 测试单个 crate
cargo ci-clippy                            # 代码检查（clippy -D warnings）
cargo fmt-check                            # 格式检查
```

## 配置

复制并编辑默认配置：

```bash
cp config.toml config.local.toml
```

`config.local.toml` 已被 .gitignore 忽略——请在此放置本地覆盖配置。

默认端口：Web UI `8443`（TLS）、RTSP `8554`、RTMP `1935`。

## 文档

完整文档请参阅 [docs/zh/](docs/zh/)：

- [快速入门](docs/zh/getting-started.md)
- [安装指南](docs/zh/installation.md)
- [配置说明](docs/zh/configuration.md)
- [API 参考](docs/zh/api.md)
- [架构说明](docs/zh/architecture.md)
- [贡献指南](docs/zh/contributing.md)

英文文档：[README.md](README.md) | [docs/en/](docs/en/)

---

## 许可证

本项目采用 **非商业源代码可用许可证**。

您可以出于非商业目的使用、研究和修改代码。商业使用需要获得明确的书面许可。详见 [LICENSE](LICENSE)。
