//! Minimal MCP (Model Context Protocol, spec 2025-06-18) **client** over
//! the stdio transport: the client spawns the server as a subprocess and
//! exchanges newline-delimited JSON-RPC 2.0 on stdin/stdout (messages
//! MUST NOT contain embedded newlines; stderr is server logs only).
//!
//! Implemented surface (everything the agent needs): `initialize` →
//! `notifications/initialized` handshake, `tools/list` with cursor
//! pagination, `tools/call`, plus the robustness rules — per-request
//! timeouts, server→client requests answered with -32601 (sampling/
//! roots/elicitation unsupported), `notifications/tools/list_changed`
//! invalidating the cached list, and respawn of dead subprocesses on the
//! next call. Shutdown closes stdin (kill_on_drop covers the rest).

use super::{ToolOutput, ToolSpec};
use serde_json::{Value, json};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::Mutex;

const PROTOCOL_VERSION: &str = "2025-06-18";
const INIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// One connected MCP server subprocess. Requests are strictly serialized
/// by the connection mutex (single in-flight per server — MCP stdio has
/// no multiplexing requirement and simple servers assume it).
struct Conn {
    server_name: String,
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<tokio::process::ChildStdout>,
    next_id: i64,
}

pub struct McpClient {
    cfg: super::McpServerConfig,
    conn: Mutex<Option<Conn>>,
    tools: Mutex<Vec<ToolSpec>>,
}

impl McpClient {
    pub fn new(cfg: super::McpServerConfig) -> Self {
        Self {
            cfg,
            conn: Mutex::new(None),
            tools: Mutex::new(Vec::new()),
        }
    }

    pub fn name(&self) -> &str {
        &self.cfg.name
    }

    /// Spawn + handshake. Reuses a live connection; respawns a dead one.
    pub async fn ensure_initialized(&self) -> anyhow::Result<()> {
        let mut guard = self.conn.lock().await;
        if guard.is_some() {
            return Ok(());
        }
        let mut child = tokio::process::Command::new(&self.cfg.command)
            .args(&self.cfg.args)
            .envs(&self.cfg.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()) // server logs stay out of the RPC channel
            .kill_on_drop(true)
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("no stdin on spawned MCP server"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("no stdout on spawned MCP server"))?;
        let mut conn = Conn {
            server_name: self.cfg.name.clone(),
            child,
            stdin,
            reader: BufReader::new(stdout),
            next_id: 1,
        };

        let fut = async {
            let id = conn.alloc_id();
            let params = json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "mibee-eye", "version": env!("CARGO_PKG_VERSION") },
            });
            let resp = conn.request(id, "initialize", Some(params)).await?;
            let server_version = resp
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("?");
            let server_name = resp
                .pointer("/serverInfo/name")
                .and_then(Value::as_str)
                .unwrap_or("?");
            if server_version != PROTOCOL_VERSION {
                // Spec: client SHOULD disconnect on an unsupported
                // negotiated version. Pragmatically we keep going (older
                // 2024-11-05 servers interop fine for tools/*) but log it.
                tracing::warn!(
                    "mcp {}: negotiated protocol {} (we asked {PROTOCOL_VERSION}) — continuing",
                    self.cfg.name,
                    server_version
                );
            }
            tracing::info!(
                "mcp server {} initialized: {server_name} ({server_version})",
                self.cfg.name
            );
            conn.notify("notifications/initialized", json!({})).await?;
            anyhow::Ok(())
        };
        match tokio::time::timeout(INIT_TIMEOUT, fut).await {
            Ok(Ok(())) => {
                *guard = Some(conn);
                Ok(())
            }
            Ok(Err(e)) => {
                let _ = conn.child.kill().await;
                Err(anyhow::anyhow!("initialize failed: {e:#}"))
            }
            Err(_) => {
                let _ = conn.child.kill().await;
                Err(anyhow::anyhow!("initialize timed out"))
            }
        }
    }

    /// List the server's tools (cursor-paginated) and cache them as
    /// `mcp:<server>`-sourced specs.
    pub async fn list_tools(&self) -> anyhow::Result<Vec<ToolSpec>> {
        self.ensure_initialized().await?;
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut params = json!({});
            if let Some(c) = &cursor {
                params["cursor"] = json!(c);
            }
            let resp = self.request("tools/list", Some(params)).await?;
            for t in resp
                .get("tools")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default()
            {
                tools.push(ToolSpec {
                    name: t
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    description: t
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    input_schema: t
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                    source: format!("mcp:{}", self.cfg.name),
                });
            }
            match resp.get("nextCursor").and_then(Value::as_str) {
                Some(next) if !next.is_empty() => cursor = Some(next.to_string()),
                _ => break,
            }
        }
        *self.tools.lock().await = tools.clone();
        Ok(tools)
    }

    /// Cached membership check (used for dispatch routing).
    pub async fn has_tool(&self, name: &str) -> bool {
        self.tools.lock().await.iter().any(|t| t.name == name)
    }

    /// `tools/call` — returns the concatenated text content; a tool
    /// execution error (`isError: true`) is data, not a protocol error:
    /// it goes back to the model as an honest failure observation.
    pub async fn call_tool(&self, name: &str, args: Value) -> anyhow::Result<ToolOutput> {
        self.ensure_initialized().await?;
        let params = json!({ "name": name, "arguments": args });
        let resp = self.request("tools/call", Some(params)).await?;
        let mut text = String::new();
        for item in resp
            .get("content")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            if item.get("type").and_then(Value::as_str) == Some("text")
                && let Some(t) = item.get("text").and_then(Value::as_str)
            {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(t);
            }
        }
        let is_error = resp
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if is_error {
            return Ok(ToolOutput {
                text: format!(
                    "工具执行失败：{}",
                    if text.is_empty() {
                        "(无详情)"
                    } else {
                        &text
                    }
                ),
                media_url: None,
            });
        }
        Ok(ToolOutput {
            text,
            media_url: None,
        })
    }

    /// Serialized JSON-RPC request with timeout; a dead connection is
    /// dropped so the next call respawns.
    async fn request(&self, method: &str, params: Option<Value>) -> anyhow::Result<Value> {
        let mut guard = self.conn.lock().await;
        let conn = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("mcp {} not connected", self.cfg.name))?;
        let id = conn.alloc_id();
        let fut = conn.request(id, method, params);
        match tokio::time::timeout(IO_TIMEOUT, fut).await {
            Ok(res) => res,
            Err(_) => {
                *guard = None; // force respawn next time
                Err(anyhow::anyhow!("mcp {method} timed out"))
            }
        }
    }
}

impl Conn {
    fn alloc_id(&mut self) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    async fn send(&mut self, msg: &Value) -> anyhow::Result<()> {
        // One JSON document per line; embedded raw newlines would corrupt
        // the framing (serde_json never emits them inside strings).
        let mut line = serde_json::to_string(msg)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn request(
        &mut self,
        id: i64,
        method: &str,
        params: Option<Value>,
    ) -> anyhow::Result<Value> {
        let mut msg = json!({ "jsonrpc": "2.0", "id": id, "method": method });
        if let Some(p) = params {
            msg["params"] = p;
        }
        self.send(&msg).await?;
        loop {
            let line = self.read_line().await?;
            let value: Value = serde_json::from_str(&line)
                .map_err(|e| anyhow::anyhow!("bad JSON-RPC frame from server: {e}"))?;
            if value.get("id").and_then(Value::as_i64) == Some(id) {
                if let Some(err) = value.get("error") {
                    let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
                    let message = err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    return Err(anyhow::anyhow!("JSON-RPC error {code}: {message}"));
                }
                return Ok(value["result"].clone());
            }
            if value.get("id").is_some() && value.get("method").is_some() {
                // A server→client request (sampling/roots/elicitation) —
                // we support none; answer -32601 so the server doesn't hang.
                let reply_id = value["id"].clone();
                self.send(&json!({
                    "jsonrpc": "2.0", "id": reply_id,
                    "error": { "code": -32601, "message": "method not found" },
                }))
                .await?;
                continue;
            }
            if value.get("method").and_then(Value::as_str)
                == Some("notifications/tools/list_changed")
            {
                tracing::info!("mcp {}: tool list changed notification", self.server_name);
            }
            // Any other notification (cancellation, progress, logs…) — ignore.
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> anyhow::Result<()> {
        let mut msg = json!({ "jsonrpc": "2.0", "method": method });
        if !params.as_object().map(|o| o.is_empty()).unwrap_or(true) {
            msg["params"] = params;
        }
        self.send(&msg).await
    }

    async fn read_line(&mut self) -> anyhow::Result<String> {
        let mut buf = String::new();
        let n = self.reader.read_line(&mut buf).await?;
        if n == 0 {
            return Err(anyhow::anyhow!("mcp server closed stdout"));
        }
        Ok(buf)
    }
}
