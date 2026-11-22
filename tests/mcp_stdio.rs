//! Protocol-level behavior of the server over real stdio.

mod common;

use serde_json::json;

use common::{Server, meta, tool_names};

const ALL_TOOLS: [&str; 10] = [
    "connections_list",
    "connections_status",
    "correspondence_quote",
    "correspondence_send",
    "correspondence_status",
    "documents_prepare",
    "permissions_check",
    "permissions_get",
    "providers_capabilities",
    "providers_list",
];

/// Tools that change something. Every other tool must declare itself
/// read-only.
const WRITING_TOOLS: [&str; 3] = [
    "correspondence_quote",
    "correspondence_send",
    "documents_prepare",
];

#[test]
fn stateless_2026_07_28_lists_and_calls_tools() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = Server::start(dir.path(), "default");

    let list = server.request(1, "tools/list", json!({"_meta": meta()}));
    assert_eq!(tool_names(&list), ALL_TOOLS);
    for tool in list["result"]["tools"].as_array().unwrap() {
        let name = tool["name"].as_str().unwrap();
        assert_eq!(
            tool["annotations"]["readOnlyHint"],
            !WRITING_TOOLS.contains(&name),
            "{name}"
        );
    }
    let send = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "correspondence_send")
        .unwrap();
    assert_eq!(send["annotations"]["destructiveHint"], true);
    assert_eq!(send["annotations"]["idempotentHint"], true);

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
    let dir = tempfile::tempdir().unwrap();
    let mut server = Server::start(dir.path(), "default");

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
    assert_eq!(init["result"]["serverInfo"]["name"], "paperwire");
    server.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    let list = server.request(2, "tools/list", json!({}));
    assert_eq!(tool_names(&list), ALL_TOOLS);
}
