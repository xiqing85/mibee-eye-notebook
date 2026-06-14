# 配置参考

本文档提供了 mibee-rec 配置系统的完整参考。

## 概述

配置文件控制 mibee-rec 行为的所有方面。配置系统支持层次化优先级，允许为开发、测试和生产环境设置不同的配置。

### 配置文件位置

1. **默认配置**：`config.toml`（项目根目录）
2. **本地覆盖**：`config.local.toml`（项目根目录）
3. **CLI 覆盖**：`--config <path>` 命令行参数

### 优先级规则

配置值按优先级顺序加载（优先级高的获胜）：

1. 命令行 `--config <path>`（绝对最高优先级）
2. `config.local.toml`（gitignored，用于本地开发）
3. `config.toml`（默认配置）

当文件不存在时，系统会回退到编译时的默认值。配置文件中未指定的个别字段和部分将使用其默认值。

## 配置参考

### [web] - Web UI 服务器

配置承载用户界面和 REST API 的 HTTPS Web 服务器。

```toml
[web]
port = 8443
host = "0.0.0.0"
```

**字段参考：**

| 字段 | 类型 | 默认值 | 描述 |
|------|------|---------|------|
| `port` | u16 | `8443` | Web UI 和 REST API 的 HTTPS 端口 |
| `host` | String | `"0.0.0.0"` | 绑定地址：`"0.0.0.0"`（所有接口）或 `"127.0.0.1"`（仅本地主机） |

**注意事项：**
- 所有 Web 流量都通过 rustls 使用 TLS（HTTPS）
- 默认端口需要特权绑定或 `setcap 'cap_net_bind_service=+ep'`
- `"127.0.0.1"` 仅限制本地连接访问

### [rtsp] - RTSP 服务器

配置用于摄像头流和客户端连接的 RTSP 流媒体服务器。

```toml
[rtsp]
server_port = 8554
```

**字段参考：**

| 字段 | 类型 | 默认值 | 描述 |
|------|------|---------|------|
| `server_port` | u16 | `8554` | RTSP 服务器监听端口 |

**注意事项：**
- RTSP 协议支持客户端（连接到 IP 摄像头）和服务器（提供流）模式
- 默认端口如果 < 1024 需要特权绑定
- 支持带有正确 SPS/PPS 头的 H.264/H.265 流媒体

### [rtmp] - RTMP 接入服务器

配置用于外部流源头的 RTMP 接入服务器。

```toml
[rtmp]
ingest_port = 1935
```

**字段参考：**

| 字段 | 类型 | 默认值 | 描述 |
|------|------|---------|------|
| `ingest_port` | u16 | `1935` | RTMP 接入服务器监听端口 |

**注意事项：**
- 用于接入来自外部源的流（例如 OBS、FFmpeg、其他摄像头）
- 手写的 RTMP 实现，支持增强的时间戳
- 默认端口是标准 RTMP 端口，不需要特权绑定

### [capture] - 本地捕获设备

配置本地网络摄像头和麦克风捕获。

```toml
[capture]
video_device = "/dev/video0"
audio_device = "default"
```

**字段参考：**

| 字段 | 类型 | 默认值 | 描述 |
|------|------|---------|------|
| `video_device` | String | `"/dev/video0"` | 视频捕获设备路径 |
| `audio_device` | String | `"default"` | 音频捕获设备标识符 |

**注意事项：**
- **Linux**：视频设备通常为 `/dev/video0`、`/dev/video1` 等
- **Linux**：音频设备 `"default"` 使用 ALSA 默认设备
- **Windows**：视频设备使用 MSMF（Media Foundation）设备名称
- **Windows**：音频设备使用 WASAPI 设备名称
- 视频捕获需要 `libv4l-dev` 和用户在 `video` 组中
- 音频捕获以高优先级运行，切勿在回调中阻塞

### [security] - 身份验证和速率限制

配置身份验证和 API 保护的安全策略。

```toml
[security]
rate_limit_max = 20
rate_limit_window_secs = 60
```

**字段参考：**

| 字段 | 类型 | 默认值 | 描述 |
|------|------|---------|------|
| `rate_limit_max` | usize | `20` | 每个速率限制窗口的最大请求数 |
| `rate_limit_window_secs` | u64 | `60` | 速率限制时间窗口（秒） |

**注意事项：**
- 为身份验证端点实现滑动窗口速率限制
- 防止对登录系统的暴力破解攻击
- 速率限制基于每 IP 地址进行

### [observability] - 监控和日志记录

配置 OpenTelemetry 跟踪和应用程序日志记录。

```toml
[observability]
otel_endpoint = "http://localhost:4317"
log_level = "info"
```

**字段参考：**

| 字段 | 类型 | 默认值 | 描述 |
|------|------|---------|------|
| `otel_endpoint` | String | `"http://localhost:4317"` | OpenTelemetry 收集器端点 |
| `log_level` | String | `"info"` | 日志级别过滤器 |

**日志级别选项：**
- `"trace"` - 最详细的日志记录，调试信息
- `"debug"` - 调试信息，函数调用
- `"info"` - 一般操作信息（默认）
- `"warn"` - 不停止操作的警告条件
- `"error"` - 可能影响操作的错误条件

**注意事项：**
- OpenTelemetry 集成是可选的 - 没有收集器时系统也能工作
- OTLP（OpenTelemetry 协议）通过 gRPC 在端口 4317 上传输
- 当 OpenTelemetry 不可用时使用结构化 JSON 日志记录
- 可用于 Prometheus 抓取的指标端点（如果已配置）

## 示例

### 本地开发配置

```toml
# 本地开发，最小安全限制
[web]
host = "127.0.0.1"  # 仅可从本地机器访问

[security]
rate_limit_max = 100  # 本地开发更宽松
```

### 生产部署配置

```toml
# 生产环境配置，安全加固
[web]
host = "0.0.0.0"  # 可从所有网络接口访问

[security]
rate_limit_max = 10  # 生产环境严格速率限制
rate_limit_window_secs = 30  # 更短的时间窗口

[observability]
otel_endpoint = "https://monitoring.example.com:4317"
log_level = "warn"  # 减少生产环境噪音
```

### 资源受限环境

```toml
# 为低资源环境优化
[web]
host = "127.0.0.1"  # 仅限制在本地主机

[observability]
otel_endpoint = ""  # 禁用 OpenTelemetry 以减少开销
log_level = "error"  # 仅记录错误

[security]
rate_limit_max = 5  # 非常严格的速率限制
```

### 网络特定配置

```toml
# 不同网络环境的配置
[web]
host = "192.168.1.100"  # 绑定到特定接口

[rtsp]
server_port = 8554

[rtmp]
ingest_port = 1935

[capture]
video_device = "/dev/video2"  # 次要摄像头
audio_device = "hw:1"  # ALSA 硬件设备 1
```

### 带调试日志的开发配置

```toml
# 带详细日志的开发配置
[observability]
otel_endpoint = "http://localhost:4317"
log_level = "debug"

[security]
rate_limit_max = 1000  # 为开发禁用速率限制
```

## 配置最佳实践

1. **切勿提交机密信息**：将敏感配置存储在 `config.local.toml` 中，该文件已被 gitignored
2. **使用描述性文件名**：`config.prod.toml`、`config.test.toml` 用于不同环境
3. **验证配置**：部署前测试配置文件
4. **记录变更**：使用新选项保持配置文档更新
5. **监控性能**：使用可观察性跟踪不同配置的资源使用情况

## 故障排除

### 常见问题

1. **端口已被占用**：尝试不同的端口或确保之前的实例已停止
2. **设备未找到**：验证设备路径和权限（Linux：`video` 组成员身份）
3. **速率限制问题**：检查 `security` 部分配置
4. **TLS 错误**：验证证书配置和端点
5. **OpenTelemetry 失败**：系统在没有收集器的情况下继续运行，但可能会丢失指标

### 验证

使用内置验证来验证配置文件：

```bash
# 创建测试配置
cat > test-config.toml << EOF
[web]
port = 8443
host = "127.0.0.1"
EOF

# 测试加载配置
cargo run -- --config test-config.toml --help
```

### 获取帮助

对于配置问题：
1. 查看本参考文档
2. 查看源代码 `src/config.rs` 以获取权威默认值
3. 检查默认的 `config.toml` 文件
4. 查看 GitHub 问题讨论