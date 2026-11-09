//! Drives the real binary over stdio with raw JSON-RPC, the way an MCP
//! client does.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_postal-mcp"))
            .arg("serve")
            .env("POSTAL_MCP_LOG", "warn")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn postal-mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn send(&mut self, message: Value) {
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    /// The response to request `id`, skipping notifications.
    fn response(&mut self, id: i64) -> Value {
        loop {
            let mut line = String::new();
            assert!(
                self.stdout.read_line(&mut line).unwrap() > 0,
                "server closed stdout"
            );
            let message: Value = serde_json::from_str(&line).unwrap();
            if message["id"] == id {
                return message;
            }
        }
    }

    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        self.response(id)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Per-request metadata of the stateless 2026-07-28 protocol.
fn meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "0"},
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

fn tool_names(list: &Value) -> Vec<String> {
    let mut names: Vec<String> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn stateless_2026_07_28_lists_and_calls_tools() {
    let mut server = Server::start();

    let list = server.request(1, "tools/list", json!({"_meta": meta()}));
    assert_eq!(
        tool_names(&list),
        ["providers_capabilities", "providers_list"]
    );
    for tool in list["result"]["tools"].as_array().unwrap() {
        assert_eq!(
            tool["annotations"]["readOnlyHint"], true,
            "{}",
            tool["name"]
        );
    }

    let providers = server.request(
        2,
        "tools/call",
        json!({"name": "providers_list", "arguments": {}, "_meta": meta()}),
    );
    let structured = &providers["result"]["structuredContent"];
    assert_eq!(structured["providers"][0]["id"], "mock");
    assert!(
        structured["providers"][0]["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("postal.send.letter"))
    );

    let mock = server.request(
        3,
        "tools/call",
        json!({"name": "providers_capabilities", "arguments": {"provider": "mock"}, "_meta": meta()}),
    );
    assert_eq!(mock["result"]["structuredContent"]["auth"], "none");

    let unknown = server.request(
        4,
        "tools/call",
        json!({"name": "providers_capabilities", "arguments": {"provider": "nope"}, "_meta": meta()}),
    );
    assert_eq!(unknown["result"]["isError"], true);
}

#[test]
fn initialize_handshake_still_works_for_older_clients() {
    let mut server = Server::start();

    let init = server.request(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "0"}
        }),
    );
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(init["result"]["serverInfo"]["name"], "postal-mcp");
    server.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    let list = server.request(2, "tools/list", json!({}));
    assert_eq!(
        tool_names(&list),
        ["providers_capabilities", "providers_list"]
    );
}
