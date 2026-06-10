# 快速入门

notebook-cam（MiBee Rec）快速入门指南 — 基于 Rust 构建的专业笔记本监控代理。

## 前置条件

**Rust：** 需要 Rust 1.85+ 和 Cargo。

**Linux 系统依赖：**

```bash
# 安装必需的系统包
sudo apt install libv4l-dev libasound2-dev libclang-dev

# 将用户添加到 video 组以获取摄像头权限
sudo usermod -aG video $USER

# 注销后重新登录以使组变更生效
```

**Windows：** 次要支持。通过 vcpkg 安装（当可用时）。

## 构建

使用 Cargo 构建项目：

```bash
# 调试模式构建（开发）
cargo build

# 发布模式构建（生产）
cargo build --release
```

发布版二进制文件将位于 `target/release/mibee-rec`。

默认端口：
- Web UI：8443（HTTPS 使用自签名 TLS）
- RTSP 服务端：8554
- RTMP 接收：1935

## 首次运行

**在首次执行时**，服务器以设置模式运行。您必须创建管理员用户才能访问 Web 界面。

启动服务器：

```bash
cargo run -- --config config.toml
```

服务器会检测这是首次运行，并允许无需认证即可访问设置端点。

使用 curl 创建管理员用户：

```bash
curl -X POST https://localhost:8443/api/auth/email \
  -H "Content-Type: application/json" \
  -d '{"username":"admin","password":"yourpass123"}'
```

**要求：**
- 用户名不能为空
- 密码必须至少 8 个字符

成功后，服务器：
- 在 SQLite 数据库中创建管理员用户
- 为 HTTPS 生成自签名 TLS 证书
- 在 8443 端口启动 Web 服务器

## 访问 Web 界面

打开浏览器并导航到：

```
https://localhost:8443
```

**重要：** 您会看到关于自签名证书的安全警告。这在开发中是预期的。点击「高级」和「继续前往 localhost」。

设置完成后，Web 界面需要使用基于会话的 cookie 进行身份认证。

## 添加摄像头

设置完成后，您可以通过 API 添加摄像头。

**示例：添加 RTSP 摄像头**

```bash
curl -X POST https://localhost:8443/api/cameras \
  -H "Content-Type: application/json" \
  -H "Cookie: session=<your-session-token>" \
  -d '{
    "name": "Front Door Camera",
    "camera_type": "rtsp",
    "config": {
      "url": "rtsp://192.168.1.100:554/stream"
    }
  }'
```

**支持的摄像头类型：**
- `usb` - 本地摄像头/麦克风
- `rtsp` - 通过 RTSP 连接的 IP 摄像头
- `onvif` - ONVIF 兼容摄像头
- `gb28181` - GB/T 28181 标准
- `rtmp` - RTMP 接收流

**注意：** `login`、`logout` 和 `snapshot` 端点当前返回 501（未实现）。使用设置端点进行身份认证。

## 下一步

继续使用以下资源：

- [安装指南](installation.md) - 详细的安装说明
- [配置说明](configuration.md) - 高级配置选项
- [API 参考](api.md) - 完整的 API 文档

用于开发和测试：
- `cargo test` - 运行所有测试
- `cargo ci-clippy` - 运行 clippy 代码检查
- `cargo fmt-check` - 检查格式

祝监控愉快！

---
*Notebook-cam (MiBee Rec) — 基于 Rust 构建的专业笔记本监控代理。*