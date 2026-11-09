# postal-mcp — Implementation Plan (Rust)

Companion to `design.md` (v0.5). The design stays authoritative on **what** to build;
this file covers **how**: milestones and Rust-specific decisions. Section numbers (§) point
into `design.md`.

## 0. Stack (expands §25)

| Originally proposed (TS) | Rust | Notes |
|---|---|---|
| Node + MCP TypeScript SDK v2 | [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk) 3.9 (official Rust SDK) | Supports protocol `2026-07-28`; `#[tool]` macros; stdio now, streamable HTTP later |
| Zod | `serde` + `schemars` | Tool input/output JSON Schemas generated from the Rust types |
| fetch / provider SDKs | `reqwest` (rustls, json, multipart) | Behind our own `Transport` trait so adapters test without a network |
| SQLite / PostgreSQL | `rusqlite` (bundled) now; `sqlx` or Postgres later | Local-first (§25.1) |
| Fastify / Hono | `axum`, later | Only for hosted mode and the local web control plane |
| Vitest | `cargo test` + `wiremock` | `wiremock` = local HTTP server for adapter tests |
| YAML config (§23) | TOML (`toml` crate) | Same shape as §23 |
| pdf-lib / PDF.js | `lopdf` (inspect/page count), later | Phase 2 |
| LibreOffice / Chromium | Sandboxed subprocess, later | Out of MVP |
| CLI | `clap` | `postal-mcp serve` and `postal-mcp admin …` |
| Secrets | `keyring` crate (macOS Keychain); env vars only via `--from-env` for CI | Never through MCP (§4.4) |
| Logging | `tracing` to **stderr** | stdout belongs to MCP stdio |

## 1. Layout

One crate (lib + bin) to start. The lib/bin split keeps it testable. Split into a
workspace only once a boundary hurts, e.g. when adapters need separate compile units.

```text
src/
  main.rs           # clap: `serve` (MCP stdio) | `admin …` (control plane)
  lib.rs
  core/             # capability ids (§3.5), Money, ids, errors
  http/             # Transport trait, Request/Response, ReqwestTransport, Retrying; ambiguous-outcome classification (§19.2)
  mcp/              # rmcp ServerHandler + tools (§9); resources later (§21)
  providers/        # ProviderAdapter trait (§17.5) + mock; lob, phaxio, stable later
  -- added by milestone --
  store/            # rusqlite schema/migrations (§18.2), BEGIN IMMEDIATE for budgets (§16.4)
  connections/      # connection records, credential lookup (keyring), verify (§4)
  delegation/       # grants, policy evaluator (§16), budget reservations
  operations/       # operation/job records, idempotency, reconciliation (§19)
  documents/        # import, hash, page count, md/txt -> HTML (§12)
  admin/            # CLI control plane: connect, grant, revoke, audit (§6)
tests/              # integration: MCP over real stdio, adapter contract tests (§17.6)
```

**The trust boundary is in the types.** `AuthorizedOperation` has a private constructor
that only `delegation::policy` can call. Every adapter method that touches a provider takes
one, so a tool handler can't reach a provider without a policy decision (§17.5, §20.6).

## 2. Milestones

### M0: Scaffold ✅ (this commit)
- Cargo project, CLI, MCP stdio server via rmcp.
- `providers_list` and `providers_capabilities` backed by the mock adapter.
- `http::Transport` with a reqwest implementation and `wiremock` tests.
- Integration test that drives the real binary over stdio.

### M1: HTTP client hardening ✅
- `HttpError` is classified by whether the provider could have received the
  request: `InvalidRequest` / `NotSent` / `Ambiguous`. A 4xx/5xx is a
  `Response`, not an error. (This replaces a connect/TLS/timeout taxonomy:
  the only question callers act on is "safe to resend?")
- `http::Retrying<T>` wraps any transport. `NotSent` is retried for any
  method. 429/502/503/504 are retried only when the request is replay-safe
  (GET/PUT/DELETE, or the adapter set an idempotency key). The adapter
  supplies the key's header name, and setting one declares that the provider
  documents deduplication by it. Backoff is jittered. `Retry-After` is
  honored, and if it exceeds `max_delay` the response is returned instead.
  **`Ambiguous` is never retried** (§19.2).
- Total timeout per request. A response over the size cap is
  `ResponseTooLarge`, which keeps the status and headers: a 2xx is still an
  accepted request, not an uncertain one.
- `Auth::{Basic, Bearer}`. `Debug` for requests, responses and bodies hides
  credentials, header values, query strings and body contents. Error
  messages strip the URL.
- Multipart upload; response headers (rate limits, request IDs).

### M2: State, connections, delegation (§27 Phase 0)
- SQLite schema for the MVP tables.
- `admin connect lob --env test`: the key is typed at a prompt and stored in
  the OS keychain (`keyring`), never MCP. An env var is accepted only with an
  explicit `--from-env` flag, for CI.
- `admin grant` / `revoke` / `audit`.
- `connect` and `grant` require confirmation on an interactive terminal and
  refuse when stdin is not a TTY, so an AI client with shell access cannot
  run them unattended (see Risks).
- Policy evaluator with default deny (§5.6), the §5.3 constraints, and audit events (§6.5).
- Tools: `connections_list`, `connections_status`, `permissions_get`, `permissions_check`.
- **Milestone:** a simulated client can use only the capabilities granted to it.

### M3: Operations on the mock provider
- Tools: `documents_prepare`, `correspondence_quote`, `correspondence_send`, `correspondence_status`.
- Persist the operation before calling the provider. Idempotency keys.
- Atomic budget reservations held until reconciliation. Re-check right before execution (§16.3).
- Tests from §28.1–28.2, including concurrent sends.

### M4: Lob adapter (test keys first)
- Letters. Verify via an authenticated GET. Create with `Idempotency-Key`. Status from tracking events.
- Lob has no quote endpoint, so use a user-configured price table marked `estimated`
  with a hard max. Unattended sends are allowed only under that bound (§13.4).
- Contract tests against mock + Lob (wiremock fixtures).

### M5: Documents
- PDF passthrough (hash, size, page count).
- Markdown/text → sanitized HTML, which Lob renders.

### M6: MVP demo (§29) as an integration test
- Connect → grant "domestic letters < $3" → send → status → revoke → send denied.

After the MVP: Phaxio fax (multipart), Stable inbound mailbox, local web control plane
(axum), streamable HTTP + OAuth for hosted mode (§7.2).

## 3. Decisions (change any)
1. Single crate until a boundary hurts.
2. TOML config.
3. CLI control plane first; local web UI later (open question 3).
4. Credentials: OS keychain from the start; env vars only with `--from-env`. Never through MCP input.
5. First live provider: Lob test environment.
6. Grants are a custom serde schema evaluated in Rust (open question 6).
7. Async runtime: tokio (rmcp and reqwest require it).
8. `ProviderAdapter` async methods return boxed futures, because the registry
   stores `dyn ProviderAdapter`. `Transport` stays generic (`impl Future`);
   adapters are generic over it (`LobAdapter<T: Transport>`).

## 4. Risks
- **An AI with shell access shares the machine with the control plane.** A
  coding agent can run the admin CLI and read env vars. The TTY confirmation
  and keychain storage stop the unattended path, but they are not a boundary
  against an agent with full shell access as the same OS user. That needs
  the hosted mode (§7.2) or a separate OS user; say so in the threat model.
- **rmcp churn.** The SDK is at 3.x and moves fast. Keep all MCP code in `src/mcp/` so upgrades stay local.
- **Pricing.** Unattended sends need a bounded cost, and Lob doesn't quote per request.
- **Client support for 2026-07-28 features** (URL elicitation, stateless discovery) varies (§8.6). Test with Claude Code early.
