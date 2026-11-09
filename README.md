# postal-mcp

Universal Correspondence MCP: connect your postal mail, fax, and virtual
mailbox accounts and give AI agents exactly the authority they need to use
them. See [`design.md`](design.md) for the specification and
[`PLAN.md`](PLAN.md) for the implementation plan.

Status: **M0 scaffold.** The MCP server runs over stdio with read-only
provider discovery backed by a mock provider. Nothing touches a real
provider yet.

## Build and test

```sh
cargo build
cargo test
```

## Run

```sh
cargo run -- serve          # MCP over stdio (the default command)
```

To use it from Claude Code:

```sh
claude mcp add postal -- /absolute/path/to/target/debug/postal-mcp serve
```

Logs go to stderr; set the level with `POSTAL_MCP_LOG` (e.g. `debug`).
