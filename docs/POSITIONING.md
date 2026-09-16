# mibee-eye — Product Positioning / 产品定位

> **Authoritative document.** Any conflict between this file and any other doc (README, inline comments) → **this file wins**.
>
> **权威文档。** 本文件与其他任何文档(README、代码注释)冲突时,**以本文件为准**。

Version: 1.0 · Last updated: 2026-06 · Owner: project maintainers

---

## English

### 1. What this product IS

**mibee-eye is a PC-local webcam and microphone capture agent.** It runs on a desktop or laptop, captures video and audio from devices physically attached to that machine (USB webcams, built-in or USB microphones), and exposes them through a TLS-gated Web UI as the primary control surface.

The Web UI is the **default and only** day-to-day interface. Live preview, snapshot, recording controls, and configuration all happen in the browser. The user should never need VLC or any external player for daily use.

### 2. What this product is NOT

- **NOT a network camera scanner or puller.** It does not discover, scan, or pull RTSP/RTMP/ONVIF streams from remote IP cameras. If the camera is not plugged into THIS machine, it is out of scope.
- **NOT an NVR / VMS / video management server.** It does not record or manage third-party cameras. It records only THIS machine's local devices.
- **NOT a cloud service.** It must run fully air-gapped. Telemetry/log exporters are optional and fail-open.
- **NOT a multi-tenant SaaS.** Single admin user model. No tenant isolation layer.
- **NOT a general computer-vision platform — but it does ship on-device object detection.** Local NanoDet-Plus inference (opt-in via `[ai]`, disabled by default) annotates the live view and serves `GET /api/detections` / `ai_detection` SSE events. It is a per-camera annotation feature of the local preview, not an analytics service: no cloud inference, no event storage, no motion-triggered recording.

### 3. Target users

- **Primary**: Small office / home office (SOHO) operators who want to repurpose a spare laptop or mini-PC as a surveillance source for an existing NVR.
- **Secondary**: Hobbyists and self-hosters running a home lab, who want browser-based access to a local webcam without exposing the camera directly.
- **Tertiary (domestic China)**: Small business / campus / government offices that need to register a local capture device with a GB/T 28181-compliant surveillance platform.

### 4. Deployment model

- Binary deployed directly onto the host PC (laptop, desktop, mini-PC, kiosk).
- Runs as a system service (systemd unit on Linux, Windows Service later).
- Operator accesses the Web UI from the same machine or remotely over LAN/VPN.
- TLS mandatory; self-signed dev certs auto-generated, production expects user-supplied certs.
- Single admin account, created on first run via the setup flow.

### 5. Capture scope

| Device type | In scope | Notes |
|---|---|---|
| USB webcam | ✅ | Primary target. V4L2 (Linux), MSMF (Windows), AVFoundation (macOS). |
| Built-in laptop camera | ✅ | Same API as USB webcam. |
| USB microphone | ✅ | ALSA / WASAPI / CoreAudio. |
| Built-in microphone | ✅ | Same API. |
| PCIe capture card (HDMI/SDI grabber) | ⚠️ Nice-to-have | Works if it surfaces as a V4L2/MSMF device. Not a primary use case. |
| MIPI camera (Raspberry Pi cam) | ❌ Out of scope | Different driver stack; revisit if SBC support requested. |
| Network/IP camera (RTSP/RTMP/ONVIF source) | ❌ **Hard NO** | Core product boundary. Never implement. |
| Virtual audio (loopback, PulseAudio monitor) | ❌ Out of scope | Filters out noise; not "physical" enough. |

### 6. Web UI requirements

| Requirement | Detail |
|---|---|
| **Browser live preview** | MSE (H.264 fragmented MP4 over fetch+MSE) or JPEG-sequence polling. Must work in Chrome/Firefox/Safari without plugins. WebRTC is NOT required for v1. |
| **Snapshot** | Single-frame JPEG capture to browser download. Backend already implemented (`GET /api/cameras/{id}/snapshot`). |
| **Recording controls** | Per-stream start/stop recording, segment duration + total capacity configurable, auto-prune oldest. |
| **Device enumeration** | Lists local V4L2 video devices and ALSA audio input devices; one-click "use as camera". |
| **Protocol toggles** | Per-stream enable/disable of RTSP server, RTMP push, ONVIF device, GB28181 device. Default all OFF. |
| **Settings** | Recording path, capacity, segment duration; protocol endpoint config; admin password change; theme; language. |
| **Bilingual** | zh-CN and en-US. Every user-facing string through i18n layer. Language persists to user settings. |
| **Themes** | Day and night. Auto-detect system preference on first run; manual override persists. |
| **Aesthetic** | Modern minimalist tech. References: Linear, Vercel, GitHub dark. Generous whitespace, restrained accent, no skeuomorphism. |
| **Accessibility** | Keyboard-navigable, focus-visible, ARIA labels on icon buttons, WCAG AA color contrast. |
| **Responsive** | Phone / tablet / desktop breakpoints. Operator may check status from a phone on LAN. |
| **Help** | Inline help text under every setting. First-run flow walks through device check + recording setup + admin password. No external docs site required for daily use. |

### 7. Outbound protocol scope (all default-OFF)

| Protocol | Direction | Typical external consumer | Trigger to enable |
|---|---|---|---|
| **RTSP Server** | OUT (pull) | External NVR or VLC pulls from `rtsp://this-host:8554/...` | Web UI toggle, per-stream |
| **RTMP Push** | OUT (push) | External NVR ingest or live-streaming platform (any RTMP-compatible: Bilibili, YouTube, etc.) | Web UI toggle, per-stream, requires `rtmp_url` + optional stream key |
| **ONVIF Device** | OUT (discoverable) | External NVR discovers this host via WS-Discovery UDP 3702, then queries SOAP device service | Web UI toggle, global |
| **GB/T 28181 Device** | OUT (register + push) | Chinese surveillance platform sends SIP INVITE, this device pushes RTP | Web UI toggle, requires platform SIP address + device ID |

**Toggling rule**: state survives restart. Configs MUST round-trip through SQLite, never in-memory only.

### 8. Local recording scope

| Attribute | Default | Range |
|---|---|---|
| Enable state | OFF | Per-stream toggle |
| Container format | MP4 (H.264 video + AAC audio when audio captured) | MP4 only for v1; MKV is a future option |
| Segment duration | 15 minutes | 1–60 minutes configurable |
| Total capacity | 10 GB | 1 GB – unlimited; 0 = unlimited |
| Pruning policy | Delete oldest segments when capacity hit | FIFO; never deletes anything not written by mibee-eye |
| Path | `./recordings/` | User-configurable absolute path; must be writable |
| Filename pattern | `{camera_id}_{YYYYmmddHHMMSS}.mp4` | Sortable lexicographically |
| Audio | Muxed into same MP4 when audio capture is enabled for that stream | Optional per-stream |

### 9. Security posture

- **TLS mandatory** — no HTTP fallback. rustls only.
- **Auth** — bcrypt-hashed admin credentials, 24h session cookies, `HttpOnly; Secure; SameSite=Strict`.
- **Rate limiting** — per-IP sliding window on auth endpoints (default 20 req / 60s). Reset counter after successful login. Per-user lockout after 5 consecutive failures (exponential backoff).
- **CSRF** — token issued at login, verified on every state-changing request.
- **CSP** — strict header, no inline scripts in production build.
- **No anonymous access** — only `/health` and `/metrics` are public; everything else requires auth.
- **2FA / mTLS** — explicitly out of scope for v1. Strong password is the only auth factor.

### 10. Observability stack

| Layer | Target backend | Status |
|---|---|---|
| **Metrics** | Prometheus | ✅ 10 counters/gauges at `/metrics` (public). |
| **Distributed traces** | OpenTelemetry → OTLP gRPC → Jaeger / Tempo / Grafana | ⚠️ Pipeline wired; needs `#[tracing::instrument]` across handlers + W3C `traceparent` propagation in HTTP middleware. |
| **Logs** | stdout / systemd-journal (local) | ✅ Structured JSON via `tracing-subscriber`. |
| **Remote log shipping** | Loki (via `tracing-loki`) OR OTLP logs | ❌ To be built. Optional and fail-open. |
| **Health** | `/health` endpoint | ✅ Uptime + status (public). |

All remote exporters MUST be optional and fail-open: a missing/unreachable backend never crashes the app.

### 11. Platform roadmap

| Phase | Platform | Status |
|---|---|---|
| **v1.0** | Linux x86_64 + aarch64 | ✅ Tier 1, ships first |
| **v1.1** | Windows x86_64 | 🚧 Planned. Known blockers: `libc::getifaddrs`, `#[cfg(unix)]` in capture code, `/dev/videoN` paths. |
| **v1.2** | macOS (Intel + Apple Silicon) | 🚧 Planned. Needs AVFoundation-aware device enumeration. |
| **Out of scope** | BSD, Android, iOS, embedded RTOS | ❌ Not planned. |

Cross-platform rule: any new platform-specific code MUST have `#[cfg(...)]` branches for all three Tier-1/2 targets, OR be guarded with a `compile_error!` for unsupported platforms.

### 12. Non-negotiable product boundaries

These are the lines that, if crossed, change the product into something else. They are NOT engineering decisions to revisit — they are definitional.

1. **Local capture only.** Never scan for, discover, or pull from remote network cameras. (If we ever add this, we are building a different product.)
2. **Web UI as primary surface.** CLI and config files are power-user escape hatches, not the main UX.
3. **Outbound protocols default-OFF.** No external surface is exposed without an explicit user action.
4. **TLS mandatory on Web UI.** No HTTP fallback ever, even on localhost.
5. **Single admin user.** No multi-tenant model; no per-user ACLs.
6. **No cloud dependency.** Telemetry exporters are optional and fail-open.
7. **Minimal dependencies.** Hand-written codec/protocol code preferred over heavyweight media frameworks.

---

## 中文

### 1. 本产品是什么

**mibee-eye 是部署在 PC 上的本地摄像头与麦克风采集代理。** 运行在桌面机或笔记本上,采集物理连接到该机器的设备(USB 摄像头、内置或 USB 麦克风)的视频和音频,并通过强制 TLS 的 Web UI 作为主控制面暴露给用户。

Web UI 是**默认且唯一**的日常操作界面。实时预览、截图、录像控制、参数配置全部在浏览器中完成。日常使用中,用户不应需要 VLC 或任何外部播放器。

### 2. 本产品不是什么

- **不是网络摄像头扫描器或拉流器。** 不会发现、扫描、拉取远程 IP 摄像头的 RTSP/RTMP/ONVIF 流。如果摄像头没插在本机上,就不在范围内。
- **不是 NVR / VMS / 视频管理服务器。** 不录像也不管理第三方摄像头。只录本机本地设备。
- **不是云服务。** 必须能在完全气隙环境运行。遥测/日志导出器是可选的,fail-open。
- **不是多租户 SaaS。** 单管理员用户模型,无租户隔离层。
- **不是通用计算机视觉平台——但自带端侧目标检测。** 本地 NanoDet-Plus 推理(`[ai]` 显式开启,默认关闭)为实时预览叠加检测框,并提供 `GET /api/detections` / `ai_detection` SSE 事件。它是本地预览的逐相机标注能力,不是分析服务:无云端推理、无事件存储、无移动触发录像。

### 3. 目标用户

- **主要**:SOHO(小型办公/家庭办公)用户,想把闲置笔记本或迷你主机改造成现有 NVR 的视频源。
- **次要**:自托管爱好者与家庭实验室玩家,希望通过浏览器访问本地摄像头,而不直接暴露摄像头本身。
- **三级(国内)**:需要将本地采集设备注册到符合 GB/T 28181 标准的监控平台的小型企业、园区、政府办公室。

### 4. 部署模式

- 直接部署在主机 PC 上的二进制(笔记本、台式机、迷你主机、自助终端)。
- 作为系统服务运行(Linux 上 systemd unit,后续 Windows Service)。
- 操作员从本机或通过 LAN/VPN 远程访问 Web UI。
- 强制 TLS;首次运行自动生成自签名开发证书,生产环境期望用户提供证书。
- 单管理员账户,首次运行通过 setup 流程创建。

### 5. 采集范围

| 设备类型 | 是否在范围 | 说明 |
|---|---|---|
| USB 摄像头 | ✅ | 主要目标。V4L2(Linux)、MSMF(Windows)、AVFoundation(macOS)。 |
| 笔记本内置摄像头 | ✅ | 同 USB 摄像头 API。 |
| USB 麦克风 | ✅ | ALSA / WASAPI / CoreAudio。 |
| 内置麦克风 | ✅ | 同上。 |
| PCIe 采集卡(HDMI/SDI 抓帧器) | ⚠️ 锦上添花 | 若能以 V4L2/MSMF 设备形式暴露则可用。非主要场景。 |
| MIPI 摄像头(树莓派摄像头) | ❌ 不在范围 | 驱动栈不同;若后续有 SBC 支持需求再议。 |
| 网络/IP 摄像头(RTSP/RTMP/ONVIF 源) | ❌ **绝对禁止** | 核心产品边界,永不实现。 |
| 虚拟音频(loopback、PulseAudio monitor) | ❌ 不在范围 | 会引入噪声;"物理性"不足。 |

### 6. Web UI 要求

| 要求 | 详情 |
|---|---|
| **浏览器实时预览** | MSE(H.264 分片 MP4,通过 fetch + MSE)或 JPEG 序列轮询。必须在 Chrome/Firefox/Safari 中无插件工作。v1 不要求 WebRTC。 |
| **截图** | 单帧 JPEG 截图,浏览器下载。后端已实现(`GET /api/cameras/{id}/snapshot`)。 |
| **录像控制** | 每路流独立启停录像,可配切片时长 + 总容量上限,自动滚动删除最旧。 |
| **设备枚举** | 列出本地 V4L2 视频设备和 ALSA 音频输入设备;一键"用作摄像头"。 |
| **协议开关** | 每路流独立启停 RTSP server、RTMP 推流、ONVIF 设备、GB28181 设备。默认全关。 |
| **设置** | 录像路径、容量、切片时长;协议端点配置;管理员密码修改;主题;语言。 |
| **双语** | zh-CN 和 en-US。每个面向用户的字符串都通过 i18n 层。语言持久化到用户设置。 |
| **主题** | 日间与夜间主题。首次运行自动检测系统偏好;手动覆盖持久化。 |
| **美学** | 现代简约科技感。参考:Linear、Vercel、GitHub dark。充裕留白,克制强调色,无拟物化。 |
| **无障碍** | 键盘可导航,focus-visible,图标按钮有 ARIA 标签,WCAG AA 色彩对比。 |
| **响应式** | 手机 / 平板 / 桌面断点。操作员可能在 LAN 内用手机查状态。 |
| **帮助** | 每项设置下方有内联帮助文本。首次运行引导走完设备检查 + 录像设置 + 管理员密码。日常使用不需要外部文档站点。 |

### 7. 出站协议范围(全部默认关闭)

| 协议 | 方向 | 典型外部消费者 | 启用触发 |
|---|---|---|---|
| **RTSP Server** | 出(拉流) | 外部 NVR 或 VLC 从 `rtsp://本机:8554/...` 拉流 | Web UI 开关,每路流 |
| **RTMP Push** | 出(推流) | 外部 NVR ingest 或直播平台(任何 RTMP 兼容:B 站、YouTube 等) | Web UI 开关,每路流,需 `rtmp_url` + 可选 stream key |
| **ONVIF Device** | 出(可被发现) | 外部 NVR 通过 WS-Discovery UDP 3702 发现本机,再查询 SOAP 设备服务 | Web UI 开关,全局 |
| **GB/T 28181 Device** | 出(注册 + 推流) | 国内监控平台发 SIP INVITE,本设备推 RTP | Web UI 开关,需平台 SIP 地址 + 设备 ID |

**开关规则**:状态必须跨重启保留。配置必须经 SQLite 往返,绝不仅存内存。

### 8. 本地录像范围

| 属性 | 默认值 | 范围 |
|---|---|---|
| 启用状态 | 关 | 每路流独立开关 |
| 容器格式 | MP4(H.264 视频 + 录音时 AAC 音频) | v1 仅 MP4;MKV 为未来选项 |
| 切片时长 | 15 分钟 | 1–60 分钟可配 |
| 总容量 | 10 GB | 1 GB – 无限;0 = 无限 |
| 滚动策略 | 容量达到上限时删除最旧切片 | FIFO;绝不删除非 mibee-eye 写入的文件 |
| 路径 | `./recordings/` | 用户可配绝对路径;必须可写 |
| 文件名模式 | `{camera_id}_{YYYYmmddHHMMSS}.mp4` | 字典序可排序 |
| 音频 | 该路流开启音频采集时合流到同一 MP4 | 每路流可选 |

### 9. 安全姿态

- **强制 TLS** — 无 HTTP 回退。仅 rustls。
- **认证** — bcrypt 哈希管理员凭证,24 小时 session cookie,`HttpOnly; Secure; SameSite=Strict`。
- **限流** — 认证端点按 IP 滑动窗口(默认 20 次/60 秒)。登录成功后重置计数。连续 5 次失败后按用户锁定(指数退避)。
- **CSRF** — 登录时签发 token,每个状态变更请求验证。
- **CSP** — 严格头,生产构建无内联脚本。
- **无匿名访问** — 仅 `/health` 和 `/metrics` 公开;其他全部要求认证。
- **2FA / mTLS** — v1 明确不在范围。强密码是唯一认证因素。

### 10. 可观测性栈

| 层 | 目标后端 | 状态 |
|---|---|---|
| **指标** | Prometheus | ✅ `/metrics` 暴露 10 个 counter/gauge(公开)。 |
| **分布式追踪** | OpenTelemetry → OTLP gRPC → Jaeger / Tempo / Grafana | ⚠️ 管线已接;需在各 handler 加 `#[tracing::instrument]` + HTTP 中间件加 W3C `traceparent` 传播。 |
| **日志** | stdout / systemd-journal(本地) | ✅ 通过 `tracing-subscriber` 结构化 JSON。 |
| **远程日志推送** | Loki(通过 `tracing-loki`)或 OTLP logs | ❌ 待建。可选且 fail-open。 |
| **健康检查** | `/health` 端点 | ✅ uptime + 状态(公开)。 |

所有远程导出器必须可选且 fail-open:后端缺失或不可达时,绝不崩溃应用。

### 11. 平台路线图

| 阶段 | 平台 | 状态 |
|---|---|---|
| **v1.0** | Linux x86_64 + aarch64 | ✅ Tier 1,首发 |
| **v1.1** | Windows x86_64 | 🚧 计划中。已知阻塞:`libc::getifaddrs`、采集代码中的 `#[cfg(unix)]`、`/dev/videoN` 路径。 |
| **v1.2** | macOS(Intel + Apple Silicon) | 🚧 计划中。需要 AVFoundation 感知的设备枚举。 |
| **不在范围** | BSD、Android、iOS、嵌入式 RTOS | ❌ 不计划。 |

跨平台规则:任何新增的平台特定代码必须有 Tier-1/2 三个目标的 `#[cfg(...)]` 分支,或用 `compile_error!` 守护不支持的平台。

### 12. 不可妥协的产品边界

这些线一旦越过,产品就变成了别的东西。它们不是待重审的工程决策,而是定义性的。

1. **仅本地采集。** 绝不扫描、发现、拉取远程网络摄像头。(若我们真要做这个,就是在做另一个产品。)
2. **Web UI 是主控制面。** CLI 和配置文件是高级用户的逃生口,不是主 UX。
3. **出站协议默认关闭。** 没有用户显式动作,绝不暴露任何外部面。
4. **Web UI 强制 TLS。** 即使在 localhost 上也永不回退到 HTTP。
5. **单管理员用户。** 无多租户模型;无按用户 ACL。
6. **无云依赖。** 遥测导出器可选且 fail-open。
7. **最小依赖。** 优先手写 codec/protocol 代码,而非笨重的媒体框架。
