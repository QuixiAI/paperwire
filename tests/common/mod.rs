//! Drives the real binary over stdio with raw JSON-RPC, the way an MCP
//! client does.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

pub struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
}

impl Server {
    /// Start `paperwire serve --client <client>` on the database in `data_dir`.
    pub fn start(data_dir: &Path, client: &str) -> Self {
        Self::start_with(data_dir, client, &[])
    }

    /// As [`Server::start`], with extra `serve` arguments.
    pub fn start_with(data_dir: &Path, client: &str, serve_args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_paperwire"))
            .args(["serve", "--client", client])
            .args(serve_args)
            .arg("--data-dir")
            .arg(data_dir)
            .env("PAPERWIRE_LOG", "warn")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn paperwire");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
            next_id: 1000,
        }
    }

    pub fn send(&mut self, message: Value) {
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    /// The response to request `id`, skipping notifications.
    pub fn response(&mut self, id: i64) -> Value {
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

    pub fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
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
pub fn meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "0"},
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

pub fn tool_names(list: &Value) -> Vec<String> {
    let mut names: Vec<String> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    names.sort();
    names
}

impl Server {
    /// Call a tool with the stateless protocol's metadata; returns `result`.
    pub fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.next_id += 1;
        let response = self.request(
            self.next_id,
            "tools/call",
            json!({"name": name, "arguments": arguments, "_meta": meta()}),
        );
        response["result"].clone()
    }

    /// The structured output of a tool call that must succeed.
    pub fn call_ok(&mut self, name: &str, arguments: Value) -> Value {
        let result = self.call(name, arguments);
        assert_ne!(result["isError"], true, "{name}: {result}");
        result["structuredContent"].clone()
    }
}

/// A minimal PDF of `pages` blank US Letter pages.
pub fn sample_pdf(pages: u32) -> Vec<u8> {
    use lopdf::{Document, Object, Stream, dictionary};

    let mut document = Document::with_version("1.5");
    let tree = document.new_object_id();
    let kids: Vec<Object> = (0..pages)
        .map(|_| {
            let content = document.add_object(Stream::new(dictionary! {}, Vec::new()));
            let page = dictionary! { "Type" => "Page", "Parent" => tree, "Contents" => content };
            document.add_object(page).into()
        })
        .collect();
    let tree_node = dictionary! {
        "Type" => "Pages",
        "Kids" => kids,
        "Count" => i64::from(pages),
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
    };
    document.objects.insert(tree, Object::Dictionary(tree_node));
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => tree });
    document.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    bytes
}
