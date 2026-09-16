# API 参考文档

## 概览

mibee-eye REST API 遵循 **MiBee 摄像头设备 Web API 统一规范 v1**
（工作区内 `mibee-webui/SPEC.md`）——与树莓派摄像头项目同一契约。
除特别说明外，所有 API 走 TLS 并要求会话认证。

### 基础地址
```
https://localhost:8443
```

### 响应信封（规范 §0）

所有 JSON 端点使用统一信封：

- 成功：`{"ok": true, "data": …}`
- 失败：`{"ok": false, "error": "<机器码>", "message": "<人类可读>"}`
  并携带语义化 HTTP 状态码。机器码：`bad_request`、`unauthorized`、
  `forbidden`、`not_found`、`conflict`、`rate_limited`、
  `not_implemented`、`internal_error`。

二进制端点（快照 / MJPEG / MSE / metrics / 静态资源）与 SSE 流不套
信封。旧 `/health` 端点同样不包裹；规范路径为 `/api/health`。

### 认证（规范 §2）

Cookie 会话 + CSRF 双提交：

1. `POST /api/auth/login` `{"username","password"}` → 下发
   `session=<token>; HttpOnly; Secure; SameSite=Strict`（24 小时）与
   `csrf-token=<token>`（供 JS 读取）两个 cookie。
2. 所有写请求（POST/PUT/DELETE/PATCH）必须在 `X-CSRF-Token` 头中回传
   `csrf-token` cookie 的值（login/setup/logout 豁免）。
3. 首次启动：`GET /api/auth/me` 返回 `503 setup_required`；
   `POST /api/auth/setup` 创建管理员并直接登录。

登录按 IP 限速（20 次/分钟），连续失败后按用户名指数锁定。

## 端点

### 公开

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/health` | `{"ok":true,"data":{"status":"ok","uptime":N}}` |
| GET | `/health` | 旧版不包裹别名 |
| GET | `/metrics` | Prometheus 文本格式 |
| GET | `/`、`/style.css`、`/js/{path}` | 内嵌 Web UI（mibee-webui 共享构建） |

### 认证

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/auth/me` | `{"username","role"}` / 401 / 503 setup_required |
| POST | `/api/auth/setup` | 首启创建管理员并建立会话 |
| POST | `/api/auth/login` | 登录（限速） |
| POST | `/api/auth/logout` | 204，清除会话 |
| POST | `/api/auth/reset` | `{"old_password","new_password"}`；使所有会话失效 |

### 设备（规范 §3）

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/status` | device_name / model / vendor / firmware / uptime / cameras |
| GET | `/api/capabilities` | 规范超集（`multi_camera`、`camera_management`、`camera_control`、`devices`、`mjpeg`、`mse`、`events`、`config_apply` 等）+ 主机硬件探测扩展字段 `system`、`recommended_profiles` |

### 相机（规范 §4）

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/cameras` | 相机列表（信封数组） |
| POST | `/api/cameras` | 创建：`{"name","camera_type","config"}` → 201 |
| GET/PUT/DELETE | `/api/cameras/{id}` | 相机 CRUD（部分更新） |
| POST | `/api/cameras/{id}/start` / `stop` | 启停采集（重复启动 409） |
| GET | `/api/cameras/{id}/snapshot` | JPEG 快照 |
| GET | `/api/cameras/{id}/live` | MJPEG 多部分流 |
| GET | `/api/cameras/{id}/stream.mse` | MSE 播放的 chunked fMP4 流 |

### 配置（规范 §5）

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/config` | `{"settings": {…点键嵌套…}, "protocols": {"onvif": …, "gb28181": …, "rtmp": …, "recording": …, "webrtc": …}}` |
| PUT | `/api/config` | 上文档的部分深合并；协议节立即热切换 ONVIF/GB28181（`config_apply.default = "immediate"`） |

取代旧的 `GET/PUT /api/settings` 与 `/api/protocols/{name}` GET/PUT 端点。

### 事件（规范 §6）

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/events` | SSE 流（`text/event-stream`，15 秒 keepalive）。事件：`camera_added`、`camera_offlined`、`ai_detection`、`ai_model_changed`、`alarm`（SPEC §6：`camera_id`、`active: true`、`source: "ai"`、`targets`、`timestamp` 毫秒时间戳） |

### 主机设备（规范 §4.8）

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/devices/video` | V4L2 视频设备枚举 |
| GET | `/api/devices/video/{index}/formats` | 支持的采集格式 |
| GET | `/api/devices/audio` | ALSA 音频设备枚举 |

### 协议运行态（设备扩展）

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/protocols/runtime-status` | `{"onvif":{"running":b},"gb28181":…,"rtmp":…}` |

## Web UI

内嵌前端是共享的 **mibee-webui** 构建（ES Modules、零构建步骤）——
与树莓派摄像头项目同一套界面，按能力通告渲染。唯一真源：工作区
`mibee-webui/`（`make sync-notebook` 拷入）。
