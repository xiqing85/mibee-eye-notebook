//! Golden wire tests for the MCP stdio client against a spec-exact fake
//! server (newline-delimited JSON-RPC, initialize → initialized →
//! tools/list [cursor pagination] → tools/call). The fake is a python3
//! script written to a temp file at test time — hermetic, no network.
//! Tests are skipped (not failed) when python3 is unavailable.

use std::sync::Arc;
use web::agent::mcp::McpClient;
use web::agent::{AgentConfig, McpServerConfig, ToolRegistry};

/// A minimal MCP server: one tool `echo.text`, one tool `boom` that
/// reports a tool-execution error (isError), cursor-paginated tools/list
/// (one tool per page), and a server→client `sampling/createMessage`
/// request before answering tools/call (the client must -32601 it, and
/// the fake fails the test if it doesn't).
const FAKE_SERVER: &str = r#"
import json, sys

SENT_PROBE = [False]

def send(obj):
    sys.stdout.write(json.dumps(obj, ensure_ascii=False) + "\n")
    sys.stdout.flush()

def main():
    while True:
        line = sys.stdin.readline()
        if not line:
            return
        try:
            msg = json.loads(line)
        except Exception:
            continue
        method = msg.get("method")
        mid = msg.get("id")
        if method == "initialize":
            assert msg["params"]["protocolVersion"] == "2025-06-18", "protocolVersion"
            assert msg["params"]["clientInfo"]["name"] == "mibee-eye", "clientInfo"
            send({"jsonrpc": "2.0", "id": mid, "result": {
                "protocolVersion": "2025-06-18",
                "capabilities": {"tools": {"listChanged": True}},
                "serverInfo": {"name": "fake-mcp", "version": "0.1"}}})
        elif method == "notifications/initialized":
            pass
        elif method == "tools/list":
            cursor = (msg.get("params") or {}).get("cursor")
            if cursor is None:
                send({"jsonrpc": "2.0", "id": mid, "result": {"tools": [{
                    "name": "echo.text",
                    "description": "Echo the given text",
                    "inputSchema": {"type": "object",
                                    "properties": {"text": {"type": "string"}},
                                    "required": ["text"]}}], "nextCursor": "p2"}})
            elif cursor == "p2":
                send({"jsonrpc": "2.0", "id": mid, "result": {"tools": [{
                    "name": "boom",
                    "description": "Always fails",
                    "inputSchema": {"type": "object", "properties": {}}}]}})
            else:
                send({"jsonrpc": "2.0", "id": mid,
                      "error": {"code": -32602, "message": "bad cursor"}})
        elif method == "tools/call":
            if not SENT_PROBE[0]:
                # Server→client request: the client must answer -32601.
                send({"jsonrpc": "2.0", "id": 9001, "method": "sampling/createMessage",
                      "params": {}})
                SENT_PROBE[0] = True
            name = msg["params"]["name"]
            args = msg["params"].get("arguments") or {}
            if name == "echo.text":
                send({"jsonrpc": "2.0", "id": mid, "result": {"content": [
                    {"type": "text", "text": "echo:" + args.get("text", "")}],
                    "isError": False}})
            elif name == "boom":
                send({"jsonrpc": "2.0", "id": mid, "result": {"content": [
                    {"type": "text", "text": "kaboom"}], "isError": True}})
            else:
                send({"jsonrpc": "2.0", "id": mid,
                      "error": {"code": -32602, "message": "Unknown tool: " + name}})
        elif method is None and mid == 9001:
            # The client's -32601 reply to our probe lands here.
            err = msg.get("error") or {}
            if err.get("code") != -32601:
                send({"jsonrpc": "2.0", "id": -1,
                      "error": {"code": -32000, "message": "PROBE_NOT_REJECTED"}})
                return
        elif method is not None:
            send({"jsonrpc": "2.0", "id": mid,
                  "error": {"code": -32601, "message": "method not found"}})

main()
"#;

fn python3() -> Option<std::path::PathBuf> {
    let exe = std::process::Command::new("python3")
        .arg("-c")
        .arg("print('ok')")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|_| std::path::PathBuf::from("python3"))?;
    Some(exe)
}

fn fake_server_script(test: &str) -> std::path::PathBuf {
    // Per-test path: parallel tests in this binary must not race on one
    // file (a torn read once produced a partial script).
    let dir = std::env::temp_dir().join(format!("mibee-mcp-test-{}-{}", std::process::id(), test));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("fake_mcp_server.py");
    std::fs::write(&path, FAKE_SERVER).unwrap();
    path
}

fn client_for(script: &std::path::Path) -> McpClient {
    McpClient::new(McpServerConfig {
        name: "fake".into(),
        command: "python3".into(),
        args: vec![script.to_string_lossy().to_string()],
        env: Default::default(),
    })
}

fn shared_tools() -> web::server::SharedTools {
    Arc::new(std::sync::RwLock::new(
        streaming::tools::ToolsConfig::default(),
    ))
}

#[tokio::test]
async fn handshake_list_paginate_and_call() {
    if python3().is_none() {
        eprintln!("skipping: python3 unavailable");
        return;
    }
    let script = fake_server_script("handshake");
    let client = client_for(&script);

    client.ensure_initialized().await.expect("handshake");
    let tools = client.list_tools().await.expect("tools/list");
    assert_eq!(tools.len(), 2, "cursor pagination merges both pages");
    assert_eq!(tools[0].name, "echo.text");
    assert_eq!(tools[0].source, "mcp:fake");
    assert_eq!(
        tools[0].input_schema["properties"]["text"]["type"],
        serde_json::json!("string")
    );
    assert_eq!(tools[1].name, "boom");

    // tools/call — the server probes with a sampling request first; the
    // client must reject it (-32601) and still deliver our result.
    let out = client
        .call_tool("echo.text", serde_json::json!({"text": "你好"}))
        .await
        .expect("tools/call");
    assert_eq!(out.text, "echo:你好");
}

#[tokio::test]
async fn tool_execution_error_is_data_not_protocol_error() {
    if python3().is_none() {
        eprintln!("skipping: python3 unavailable");
        return;
    }
    let script = fake_server_script("error");
    let client = client_for(&script);
    client.ensure_initialized().await.expect("handshake");
    let out = client
        .call_tool("boom", serde_json::json!({}))
        .await
        .expect("isError responses are Ok");
    assert!(
        out.text.contains("工具执行失败"),
        "honest error text: {}",
        out.text
    );
    assert!(out.text.contains("kaboom"));
}

#[tokio::test]
async fn registry_merges_mcp_tools_and_dispatches_calls() {
    if python3().is_none() {
        eprintln!("skipping: python3 unavailable");
        return;
    }
    let script = fake_server_script("registry");
    let cfg = AgentConfig {
        mcp_servers: vec![McpServerConfig {
            name: "fake".into(),
            command: "python3".into(),
            args: vec![script.to_string_lossy().to_string()],
            env: Default::default(),
        }],
        ..AgentConfig::default()
    };
    let registry = Arc::new(ToolRegistry::new(shared_tools(), &cfg));
    registry.attach_streams(Arc::new(web::stream_manager::StreamManager::new()));
    registry.refresh_mcp().await;
    let specs = registry.cached_specs();
    assert!(
        specs
            .iter()
            .any(|t| t.name == "time.now" && t.source == "builtin")
    );
    assert!(
        specs
            .iter()
            .any(|t| t.name == "echo.text" && t.source == "mcp:fake")
    );

    let out = registry
        .call(
            "echo.text",
            serde_json::json!({"text": "reg"}),
            std::time::Duration::from_secs(10),
        )
        .await
        .expect("registry dispatch reaches the MCP tool");
    assert_eq!(out.text, "echo:reg");

    let err = registry
        .call(
            "no.such.tool",
            serde_json::json!({}),
            std::time::Duration::from_secs(10),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("unknown tool"));
}
