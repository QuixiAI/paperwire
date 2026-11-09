# postal-mcp

Rust implementation of the Universal Correspondence MCP. `design.md` is the
spec; `PLAN.md` tracks milestones and Rust-specific decisions.

## Commands
- `cargo test` — unit tests, wiremock HTTP tests, and MCP-over-stdio tests against the real binary
- `cargo clippy --all-targets` and `cargo fmt` before finishing a change

## Conventions
- In `serve`, stdout is the MCP channel. Never print to it; log with `tracing` (stderr).
  Only `admin` commands write results to stdout.
- The MCP side (`src/mcp/`) reads grants and connections but never creates or
  changes them. Only `src/admin/` does.
- The client identity is `serve --client`, never a tool argument.
- All rmcp-specific code stays in `src/mcp/`.
- Adapters reach the network only through `http::Transport`.
- An `HttpError::Ambiguous` outcome is never retried automatically (design.md §19.2).
  Retries go through `http::Retrying` only; adapters do not write their own retry loops.
- Paid or consequential POSTs carry an `idempotency_key` (design.md §19.1), with
  the provider's documented header name. Set one only if the provider
  deduplicates by it: it is what permits a retry.
- `admin` commands that connect accounts or grant permissions must confirm on an
  interactive TTY and refuse otherwise. Never add a flag that skips this.
- Money is integer minor units (`core::Money`), never floats.
- Credentials never pass through MCP tool inputs or appear in logs/Debug output.
- Default deny: connecting an account grants the AI nothing (design.md §5.6).
