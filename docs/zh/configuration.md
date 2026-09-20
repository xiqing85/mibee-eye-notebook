# 配置参考

本文档提供了 mibee-eye 配置系统的完整参考。

## 概述

配置文件控制 mibee-eye 行为的所有方面。配置系统支持层次化优先级，允许为开发、测试和生产环境设置不同的配置。

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
advertised_host = "192.168.1.100"
```

**字段参考：**

| 字段 | 类型 | 默认值 | 描述 |
|------|------|---------|------|
| `port` | u16 | `8443` | Web UI 和 REST API 的 HTTPS 端口（必须 > 1024） |
| `host` | String | `"0.0.0.0"` | 绑定地址：`"0.0.0.0"`（所有接口）或 `"127.0.0.1"`（仅本地主机） |
| `advertised_host` | Option<String> | `None` | 返回给客户端的 URL 的通告主机名/IP。如果为 None，则在启动时通过 UDP 探测自动检测。消除外部 URL 中的硬编码 localhost。 |

**注意事项：**

- 所有 Web 流量都通过 rustls 使用 TLS（HTTPS）
- 首次运行时在 `tls/cert.pem` + `tls/key.pem` 自动生成自签名 TLS 证书
- 证书支持文件 mtime 更改时的热重载
- 生产环境应提供 CA 签名的证书
- 默认端口避免特权范围（< 1024）

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

- RTSP 服务器以服务器模式运行（客户端从该机器拉取流）
- RTSP 协议支持 RFC 2326，具有 Digest 身份验证和 RTP 交错
- 默认端口避免特权范围；无需特权绑定
- 支持带有正确 SPS/PPS 头的 H.264 流媒体
- 外部客户端（NVR、VLC）连接到 `rtsp://this-host:8554/stream` 以拉取流

### [rtmp_push] - RTMP 推送客户端

配置用于将流推送到外部推流服务器（NVR、直播平台）的 RTMP 推送客户端。

**所有 RTMP 操作均为出站推送 — 此机器推送到外部端点。不存在 RTMP 接收服务器。**

```toml
[rtmp_push]
enabled = false
push_url = "rtmp://192.168.1.100:1935/live"
app_name = "live"
stream_name = "stream1"
reconnect_interval_secs = 5
max_reconnect_attempts = 10
```

**字段参考：**

| 字段 | 类型 | 默认值 | 描述 |
|------|------|---------|------|
| `enabled` | bool | `false` | 主启用开关（所有出站协议默认关闭） |
| `push_url` | String | `"rtmp://192.168.1.100:1935/live"` | 外部 RTMP 推流端点 URL |
| `app_name` | String | `"live"` | RTMP 应用名称 |
| `stream_name` | String | `"stream1"` | RTMP 流密钥/名称 |
| `reconnect_interval_secs` | u64 | `5` | 重连尝试间隔（秒）（如果启用，必须 > 0） |
| `max_reconnect_attempts` | u32 | `10` | 最大重连尝试次数（如果启用，必须 > 0） |

**注意事项：**

- 这只是出站推送 — 此机器推送到外部 RTMP 推流服务器
- 不存在 RTMP 接收服务器；外部客户端无法通过 RTMP 推送到此机器
- 手写的 RTMP 实现，支持增强的时间戳
- 连接丢失时自动重连，具有指数退避
- 可通过 Web UI 进行协议热切换，无需重启服务器

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
- 音频捕获以高优先级运行；切勿在回调中阻塞
- 本产品是仅本地捕获 — 它不会发现或从远程网络摄像头拉取

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
| `rate_limit_max` | usize | `20` | 每个速率限制窗口的最大请求数（必须 > 0） |
| `rate_limit_window_secs` | u64 | `60` | 速率限制时间窗口（秒） |

**注意事项：**

- 使用 `parking_lot::Mutex` 为身份验证端点实现滑动窗口速率限制
- 防止对登录系统的暴力破解攻击
- 速率限制基于每 IP 地址进行
- 5 次失败后登录失败锁定，具有指数退避
- Mutex 不可中毒（对请求处理程序安全）

### [observability] - 监控和日志记录

配置 OpenTelemetry 跟踪、应用程序日志记录和可选的远程日志推送。

```toml
[observability]
otel_endpoint = "http://localhost:4317"
log_level = "info"

[observability.logs]
endpoint = "http://loki:3100/loki/api/v1/push"
batch_size = 100
flush_interval_secs = 5

[observability.logs.labels]
environment = "production"
service = "mibee-eye"
```

**字段参考：**

| 字段 | 类型 | 默认值 | 描述 |
|------|------|---------|------|
| `otel_endpoint` | String | `"http://localhost:4317"` | OpenTelemetry 收集器端点（OTLP gRPC） |
| `log_level` | String | `"info"` | 日志级别过滤器 |
| `logs` | Option<RemoteLogConfig> | `None` | 可选的远程日志推送配置 |

**[observability.logs] RemoteLogConfig 字段：**

| 字段 | 类型 | 默认值 | 描述 |
|------|------|---------|------|
| `endpoint` | String | `""` | 用于远程日志推送的兼容 Loki 的 HTTP 端点 URL |
| `batch_size` | usize | `100` | 每次刷新批处理的日志条目数 |
| `flush_interval_secs` | u64 | `5` | 刷新间隔（秒） |
| `labels` | HashMap<String, String> | `{}` | 附加到每个日志流的额外标签 |

**日志级别选项：**

- `"trace"` - 最详细的日志记录，调试信息
- `"debug"` - 调试信息，函数调用
- `"info"` - 一般操作信息（默认）
- `"warn"` - 不停止操作的警告条件
- `"error"` - 可能影响操作的错误条件

**注意事项：**

- OpenTelemetry 集成是可选的 — 没有收集器时系统也能工作
- OTLP（OpenTelemetry 协议）通过 gRPC 在端口 4317 上传输
- 当 OpenTelemetry 不可用时使用结构化 JSON 日志记录
- 远程日志推送到 Loki 是可选的并且是失败开放：不可达的端点记录警告，应用程序继续
- 在传入请求上提取 W3C TraceContext `traceparent` 标头，在传出请求上注入
- 可用于 Prometheus 抓取的 `/metrics` 端点（无身份验证，生产环境中的防火墙）

### [onvif] - ONVIF 设备端点

配置用于通过 WS-Discovery 进行外部 NVR 发现的 ONVIF 设备端点。

**此机器充当 ONVIF 摄像头 — 外部 NVR 发现它。这不是 ONVIF 客户端。**

```toml
[onvif]
enabled = false
device_name = "mibee-eye"
manufacturer = "MiBee"
model = "Rec-01"
serial = "NC00000001"
firmware_version = "1.0.0"
port = 3702
events_enabled = true
```

**字段参考：**

| 字段 | 类型 | 默认值 | 描述 |
|------|------|---------|------|
| `enabled` | bool | `false` | 主启用开关（所有出站协议默认关闭） |
| `device_name` | String | `"mibee-eye"` | ONVIF 设备名称 |
| `manufacturer` | String | `"MiBee"` | 制造商名称 |
| `model` | String | `"Rec-01"` | 设备型号 |
| `serial` | String | `"NC00000001"` | 序列号 |
| `firmware_version` | String | `"1.0.0"` | 固件版本 |
| `port` | u16 | `3702` | WS-Discovery UDP 端口（硬编码） |
| `events_enabled` | bool | `true` | Pull-Point 事件服务：AI 运动告警以 `tns1:VideoSource/MotionAlarm` 推送给持有订阅的 NVR |

**注意事项：**

- 出站协议 — 此设备广播 WS-Discovery 消息，外部 NVR 发现它
- 手写的 WS-Discovery 服务器（701 LOC）
- 可通过 Web UI 进行协议热切换，无需重启服务器

### [gb28181] - GB/T 28181 设备注册

配置与中国监控平台的 GB/T 28181 设备注册。

**此设备通过 SIP REGISTER 向平台注册；平台发送 INVITE，此设备推送 RTP。**

```toml
[gb28181]
enabled = false
platform_sip_address = "192.168.1.100"
platform_sip_port = 5060
device_id = "34020000002000000001"
username = ""
password = ""
sip_domain = "3402000000"
register_interval_secs = 60
```

**字段参考：**

| 字段 | 类型 | 默认值 | 描述 |
|------|------|---------|------|
| `enabled` | bool | `false` | 主启用开关（所有出站协议默认关闭） |
| `platform_sip_address` | String | `"192.168.1.100"` | SIP 平台地址 |
| `platform_sip_port` | u16 | `5060` | SIP 平台端口（如果启用，必须 > 1024） |
| `device_id` | String | `"34020000002000000001"` | 20 字符 GB28181 设备 ID |
| `username` | String | `""` | SIP 身份验证用户名 |
| `password` | String | `""` | SIP 身份验证密码 |
| `sip_domain` | String | `"3402000000"` | SIP 域 |
| `register_interval_secs` | u64 | `60` | SIP REGISTER 间隔（秒）（如果启用，必须 > 0） |
| `channel_id` | String | `"34020000001320000001"` | 20 字符 GB28181 通道 ID |
| `local_sip_port` | u16 | `5060` | 本地 SIP 监听端口 |
| `heartbeat_interval_secs` | u64 | `60` | 心跳间隔（秒） |
| `heartbeat_timeout_count` | u32 | `3` | 判定重连前允许丢失的心跳次数 |
| `talkback_playback` | bool | `true` | 在本机输出设备播放平台语音对讲（不可用时 fail-open 488） |
| `alarm_notify_enabled` | bool | `true` | AI 告警 NOTIFY 初始门控（平台 DeviceConfig AlarmReport 可运行时覆盖） |
| `alarm_cooldown_secs` | u64 | `30` | 告警上升沿冷却（秒）（SPEC §6 `alarm` + NOTIFY） |
| `position_longitude` | String | `""` | 静态 MobilePosition 经度（空 = 不上报位置） |
| `position_latitude` | String | `""` | 静态 MobilePosition 纬度（空 = 不上报位置） |

**注意事项：**

- 出站协议 — 此设备向平台注册，而不是平台角色
- GB28181 信令由 `gb28181-rs` 库提供，RTP/PS over UDP 推流
- AI 检测上升沿触发 SPEC §6 `alarm` SSE 事件，并在平台已订阅且门控允许时发送 Alarm NOTIFY——优先级 4、方法 5、类型 2（2022 标准表）
- SIGTERM / 协议停止时以 REGISTER `Expires: 0` 注销（失败仅记录日志并忽略）
- 可通过 Web UI 进行协议热切换，无需重启服务器

### [recording] - 本地录制

配置本地 MP4 段录制到磁盘。

```toml
[recording]
enabled = false
path = "./recordings"
segment_duration_secs = 900
max_capacity_mb = 10240
```

**字段参考：**

| 字段 | 类型 | 默认值 | 描述 |
|------|------|---------|------|
| `enabled` | bool | `false` | 主启用开关。单个流可以通过 Web UI 选择退出。 |
| `path` | String | `"./recordings"` | 写入 MP4 段文件的目录（必须可写） |
| `segment_duration_secs` | u64 | `900` | MP4 段持续时间（秒）（必须 > 0）。默认：15 分钟。 |
| `max_capacity_mb` | u64 | `10240` | 最大总容量（MB）。0 = 无限制（无修剪）。默认：10 GB。 |

**注意事项：**

- 捕获的 H.264 帧被复用到滚动 MP4 段中
- 当总大小超过 `max_capacity_mb` 时，最旧的段自动修剪
- 单个流可以通过 Web UI 启用/禁用录制

### [database] - SQLite 数据库

配置用于摄像头设置、协议配置、会话和用户的 SQLite 数据库路径。

```toml
[database]
path = "~/.local/share/mibee-eye/mibee_eye.db"
```

**字段参考：**

| 字段 | 类型 | 默认值 | 描述 |
|------|------|---------|------|
| `path` | String | `~/.local/share/mibee-eye/mibee_eye.db` | SQLite 数据库文件路径（XDG 兼容默认值） |

**注意事项：**

- 使用 XDG 数据目录作为默认路径：`~/.local/share/mibee-eye/mibee_eye.db`
- 如果 XDG 数据目录不可用，则回退到 `/tmp/mibee-eye/mibee_eye.db`
- 存储摄像头配置、协议配置、会话、用户和流会话数据

## 配置验证

在启动时验证配置。以下规则适用：

- **端口约束**：所有端口必须 > 1024（web.port、rtsp.server_port、gb28181.platform_sip_port）
- **端口冲突**：web.port 不得等于 rtsp.server_port
- **速率限制**：security.rate_limit_max 必须 > 0
- **GB28181 间隔**：gb28181.register_interval_secs 必须 > 0（如果启用）
- **RTMP 推送**：rtmp_push.reconnect_interval_secs 必须 > 0（如果启用）
- **RTMP 推送**：rtmp_push.max_reconnect_attempts 必须 > 0（如果启用）
- **日志级别**：observability.log_level 必须是以下之一：trace、debug、info、warn、error
- **录制路径**：recording.path 不得为空
- **录制段**：recording.segment_duration_secs 必须 > 0

## 示例

### 启用所有协议的生产配置

```toml
# 启用所有出站协议的生产配置
[web]
port = 8443
host = "0.0.0.0"
advertised_host = "192.168.1.100"

[rtsp]
server_port = 8554

[rtmp_push]
enabled = true
push_url = "rtmp://your-nvr.example.com:1935/live"
app_name = "live"
stream_name = "webcam-stream"
reconnect_interval_secs = 5
max_reconnect_attempts = 10

[onvif]
enabled = true
device_name = "mibee-eye"
manufacturer = "MiBee"
model = "Rec-01"
serial = "NC00000001"
firmware_version = "1.0.0"
port = 3702

[gb28181]
enabled = true
platform_sip_address = "192.168.1.100"
platform_sip_port = 5060
device_id = "34020000002000000001"
username = "admin"
password = "secret123"
sip_domain = "3402000000"
register_interval_secs = 60
alarm_notify_enabled = true
alarm_cooldown_secs = 30
position_longitude = ""
position_latitude = ""

[recording]
enabled = true
path = "/var/lib/mibee-eye/recordings"
segment_duration_secs = 900
max_capacity_mb = 20480

[database]
path = "/var/lib/mibee-eye/mibee_eye.db"

[security]
rate_limit_max = 10
rate_limit_window_secs = 30

[observability]
otel_endpoint = "https://monitoring.example.com:4317"
log_level = "warn"

[observability.logs]
endpoint = "http://loki:3100/loki/api/v1/push"
batch_size = 100
flush_interval_secs = 5

[observability.logs.labels]
environment = "production"
service = "mibee-eye"
```

### 本地开发配置

```toml
# 本地开发，最小安全限制
[web]
host = "127.0.0.1"
advertised_host = "127.0.0.1"

[security]
rate_limit_max = 100

[observability]
log_level = "debug"
```

### 最小配置

```toml
# 最小配置 — 大多数字段使用默认值
[web]
port = 8443

[capture]
video_device = "/dev/video0"
audio_device = "default"

[recording]
enabled = true
path = "./recordings"
```

### 资源受限环境

```toml
# 为低资源环境优化
[web]
host = "127.0.0.1"

[observability]
otel_endpoint = ""
log_level = "error"

[security]
rate_limit_max = 5

[recording]
enabled = false
```

### 网络特定配置

```toml
# 不同网络环境的配置
[web]
host = "192.168.1.100"
advertised_host = "192.168.1.100"

[rtsp]
server_port = 8554

[capture]
video_device = "/dev/video2"
audio_device = "hw:1"

[rtmp_push]
enabled = true
push_url = "rtmp://streaming.example.com:1935/live"
```

### TLS 证书管理

```toml
# 生产环境，使用自定义 TLS 证书
[web]
port = 8443
# 在 tls/cert.pem 和 tls/key.pem 提供您自己的 CA 签名证书
# 证书在文件 mtime 更改时自动重载
```

**TLS 证书注意事项：**

- 首次运行时在 `tls/cert.pem` + `tls/key.pem` 自动生成自签名证书
- 证书支持文件 mtime 更改时的热重载
- 生产环境应提供 CA 签名的证书
- 通用名称：CN=mibee-eye，SAN：mibee-eye.local
- 自动生成的开发证书的有效期：约 30 天

## 配置最佳实践

1. **切勿提交机密信息**：将敏感配置存储在 `config.local.toml` 中，该文件已被 gitignored
2. **使用描述性文件名**：`config.prod.toml`、`config.test.toml` 用于不同环境
3. **验证配置**：部署前测试配置文件（使用 --config 标志运行应用程序）
4. **记录变更**：使用新选项保持配置文档更新
5. **监控性能**：使用可观察性跟踪不同配置的资源使用情况
6. **协议默认值**：所有出站协议（RTMP 推送、ONVIF、GB28181）默认关闭以确保安全
7. **端口约束**：使用端口 > 1024 以避免特权要求
8. **通告主机**：为多宿主网络或 NAT 后设置 `web.advertised_host`

## 故障排除

### 常见问题

1. **端口已被占用**：尝试不同的端口或确保之前的实例已停止。检查验证中的端口冲突。
2. **设备未找到**：验证设备路径和权限（Linux：`video` 组成员身份）。这是仅本地捕获 — 不会发现远程摄像头。
3. **速率限制问题**：检查 `security` 部分配置。确保 `rate_limit_max` > 0。
4. **TLS 错误**：验证证书配置和端点。检查 `tls/cert.pem` 和 `tls/key.pem` 是否存在。
5. **OpenTelemetry 失败**：系统在没有收集器的情况下继续运行，但可能会丢失指标。验证通过。
6. **端口冲突**：web.port 不得等于 rtsp.server_port。验证将拒绝。
7. **GB28181 注册失败**：检查 platform_sip_address、platform_sip_port、username、password。平台必须可达。
8. **RTMP 推送失败**：验证 push_url、app_name、stream_name。外部推流服务器必须正在接受连接。
9. **录制路径不可写**：确保 recording.path 存在且可写。自动修剪需要读/写访问权限。

### 验证

通过使用 `--config` 启动应用程序来验证配置文件：

```bash
# 通过启动应用程序测试配置
cargo run -- --config test-config.toml

# 如果配置无效，启动失败并显示描述性错误
# 示例错误："web.port: must be > 1024, got 80"
# 示例错误："rtsp.server_port: must not equal web.port"
```

### 获取帮助

对于配置问题：
1. 查看本参考文档
2. 查看源代码 `src/config.rs` 和 `crates/web/src/config.rs` 以获取权威默认值
3. 检查默认的 `config.toml` 文件
4. 查看 GitHub 问题讨论
5. 通过启动验证：`cargo run -- --config your-config.toml`