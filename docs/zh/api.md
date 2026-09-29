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
| GET | `/api/capabilities` | 规范超集（`multi_camera`、`camera_management`、`camera_control`、`devices`、`mjpeg`、`mse`、`events`、`config_apply` 等）+ 端侧智能布尔位 `ai` / `zones` / `audio_ai` / `voice` / `chat` / `vlm` / `ocr` / `substream` + 主机硬件探测扩展字段 `system`、`recommended_profiles`，`events` 追加按能力门控的事件名 |

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
| GET | `/api/cameras/{id}/stream.sub.mse` | 低分辨率子码流（每相机 `config.substream`；经 `capabilities.substream` 通告） |
| GET | `/api/cameras/{id}/zones` | 已存区域：`{"zones":[{name, kind:"intrusion"\|"line_cross", points:[[x,y]…], dwell_secs}], "applied":"immediate"}`（能力位 `zones`） |
| PUT | `/api/cameras/{id}/zones` | 整体替换区域列表——body 为**裸数组**；结构校验（入侵 ≥ 3 点、越线恰 2 点） |

### 配置（规范 §5）

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/config` | `{"settings": {…点键嵌套…}, "protocols": {"onvif": …, "gb28181": …, "rtmp": …, "recording": …, "webrtc": …}}` |
| PUT | `/api/config` | 上文档的部分深合并；协议节立即热切换 ONVIF/GB28181（`config_apply.default = "immediate"`） |

取代旧的 `GET/PUT /api/settings` 与 `/api/protocols/{name}` GET/PUT 端点。

### 事件（规范 §6）

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/events` | SSE 流（`text/event-stream`，15 秒 keepalive）。事件：`camera_added`、`camera_offlined`、`ai_detection`、`ai_model_changed`、`alarm`（SPEC §6：`camera_id`、`active: true`、`source: "ai"` 或 `"audio"`、`targets` / `class` + `score`、`timestamp` 毫秒时间戳）、`zone_event`（`{camera_id, zone, event, track_id, label, timestamp}`）、`voice_transcript`（`{keyword, transcript, speaker, timestamp}`——`speaker` 为最优匹配的已注册声纹，未知为空串）、`chat_reply`（`{source, reply, timestamp}`）、`voice_decision`（`{camera_id, transcript, choice, confidence, act_probability, timestamp}`——语音转写的意图决策）、`alarm_description`（`{camera_id, alarm_timestamp, description, elapsed_s}`）。按能力门控的事件只在对应引擎活跃时送出。 |

### 端侧智能（设备扩展）

以下端点全部按能力位门控：引擎未激活时如实作答（`{"enabled": false}` 形态
/ `not_implemented` 类错误），绝不假装成功。

| 方法 | 路径 | 说明 |
|------|------|------|
| POST | `/api/chat` | 本地 LLM 对话：`{"text","history":[{role,content}]}` → `{"reply"}`（能力位 `chat`；语音环路的回复另经 `chat_reply` SSE 送出） |
| POST | `/api/ocr` | body = JPEG 原始字节 → `{"items":[{text, score, bbox}]}`（能力位 `ocr`） |
| GET | `/api/audio/records` | 听觉记录（能力位 `audio_records`）：`{"records":[{id, kind:"sound"\|"voice", text, score, keyword, speaker, timestamp_ms}]}`，最新在前；`?limit=N`（缺省 100、上限 500）、`?kind=sound\|voice` 过滤 |
| DELETE | `/api/audio/records` | 清空全部记录 → `{"applied":"immediate","removed":N}` |
| GET | `/api/voice/speakers` | 声纹档案列表（能力位 `voice_speakers`，**无副作用**）→ `{"speakers":[{id,name,dim,count,created_at}], "enrollment":{name,collected,needed}\|null, "capable":bool}` |
| POST | `/api/voice/speakers` | 开始注册：体 `{"name", "utterances"?（缺省 3，1..=10）}`——接下来 `utterances` 次唤醒词各采一条嵌入样本，进度经 GET 轮询；已注册名 400 |
| POST | `/api/voice/speakers/commit` | 样本集满后持久化并入内存 → `{"enrolled","samples","dim"}`；未集满 400 |
| POST | `/api/voice/speakers/cancel` | 放弃进行中的注册会话 |
| DELETE | `/api/voice/speakers/{name}` | 删除声纹档案（内存+数据库；不存在 404） |
| GET/PUT | `/api/cameras/{id}/zones` | 见上方相机表 |
| GET | `/api/ai/models` | 检测模型库：可用/激活模型列表 |
| POST | `/api/ai/models/{id}/activate` | 激活已上传模型 |
| POST | `/api/ai/models` | 上传模型包 |
| DELETE | `/api/ai/models/{id}` | 删除已上传模型 |

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
