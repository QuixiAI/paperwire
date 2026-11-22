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

### M2: State, connections, delegation (§27 Phase 0) ✅
- SQLite (`store/`): connections, grants, usage ledger, audit events. The
  server and the admin CLI share the file and cache nothing, so a revocation
  applies on the server's next call (§6.3).
- Control plane (`admin/`): `connect`, `connections`, `disconnect`, `grant`,
  `grants`, `revoke`, `audit`. `connect` and `grant` require confirmation on
  an interactive terminal and refuse without one. `revoke` and `disconnect`
  only narrow authority and need no confirmation.
- Credentials: typed at a hidden prompt and stored in the OS keychain
  (`keyring`). `--from-env VAR` records the variable name instead, for
  machines without a keychain. It still needs the terminal confirmation.
- Client identity (open question 4): `serve --client NAME` (default
  `default`). Grants name that client. It is configuration, not
  authentication (§5.2).
- Policy evaluator (`delegation/policy.rs`): a pure function. Default deny
  (§5.6), the §5.3 limits, fail closed when a limit's input is missing.
  Limits apply only where they make sense (reads are not constrained by
  cost or destination). Every decision is audited (§6.5).
- Tools: `connections_list`, `connections_status`, `permissions_get`,
  `permissions_check`. A client sees only connections it holds a grant on.
- **Milestone met:** `tests/delegation.rs` drives the real binary: a client
  can use only what it was granted, and revoking or disconnecting stops it.
- **Not yet:** only the mock provider can be connected (Lob is M4). Budgets
  read the usage ledger but nothing writes to it until M3, which also adds
  atomic reservations (§16.4). Grants cannot be edited, only revoked and
  re-created.

### M3: Operations on the mock provider ✅
- Tools: `documents_prepare`, `correspondence_quote`, `correspondence_send`,
  `correspondence_status`.
- `operations::send` runs in three steps. (1) In one `BEGIN IMMEDIATE`
  transaction: evaluate authority, reserve the cost, record the operation
  as `pending`. (2) Submit, with the operation id as idempotency key.
  (3) Record the outcome: accepted commits the reservation, a definite
  rejection releases it, uncertain keeps it held (§16.4, §19.2).
- `AuthorizedOperation` has private fields and one constructor,
  `delegation::authorize`. Adapters require one to submit (§17.5).
- A quote is sent at most once. Sending it again returns the same operation
  and never contacts the provider, so the quote id is the client's
  idempotency key (§19.1).
- Authority is decided at send time, not quote time (§16.3). A send needs a
  bounded cost even under a grant with no money limits (§13.4).
- Uncertain operations, and `pending` ones left by a crash, are reconciled
  through the adapter's `lookup` when status is read. They are never
  resubmitted. If the provider cannot say, the user settles them with
  `admin resolve <operation> accepted|failed` (§19.2 step 5).
- Documents: inline text or Markdown only, with a page count estimated from
  a layout rule and flagged as estimated. Real formats are M5.
- Tests (`tests/operations.rs`): every provider outcome, replay, budget
  reservation and release, ten concurrent sends over two database handles
  against a budget for three, revocation between quote and send, and
  isolation between clients (§28.1, §28.2).
- **Not yet:** only "letter" is supported. Status stops at `accepted`;
  mailed and delivered come from provider tracking in M4. No approval
  workflow, so `approval_required` is a refusal. The mock keeps its orders
  in memory, so they do not survive a server restart.

### M4: Lob adapter ✅ (built from Lob's documentation; not yet run against Lob)
- `providers/lob.rs`, generic over `Transport`. Registered in
  `Registry::builtin()` behind the retry wrapper.
- Verify: `GET /v1/letters?limit=1`. The key's `test_` / `live_` prefix must
  match the connection's environment, checked before any request.
- Create: `POST /v1/letters` as JSON, with `Idempotency-Key` set to the
  operation id and the operation id in `metadata`. 4xx is a definite
  rejection; 409, 5xx, timeouts and unreadable replies are uncertain.
- Reconcile: list letters by `metadata[operation_id]`. A hit settles the
  operation as accepted. An empty list is reported as unknown, not as "never
  sent", so the user resolves it with `admin resolve`.
- Price: Lob cannot quote, so the adapter reports the price as unavailable.
  `admin price lob letter --base USD:… --per-extra-page …` sets the user's
  own price, which becomes an `estimated` quote. With no price set, nothing
  can be sent (§13.4). Lob reports no charge, so the full estimate is
  counted as spent.
- Content: plain-text documents rendered as escaped HTML in a layout that
  matches the page estimate, with the address on an inserted first page
  (billed as a page). Lob's inline limit is 10,000 characters of HTML.
- Tracking: `correspondence_status` adds the provider's delivery report
  (`processing`, `mailed`, `in_transit`, `delivered`, `returned`,
  `cancelled`) when the client holds `postal.read.status`.
- Contract tests: `tests/lob.rs` against a wiremock stand-in.
- **To confirm with a real Lob test key:**
  - the `Lob-Version` value (`2020-02-11`) and the JSON request shape,
    including `use_type` and `address_placement`;
  - that Lob's renderer lays the HTML out as the page estimate assumes;
  - whether listing by metadata shows a new letter immediately, which would
    let an empty list count as "never sent";
  - the tracking event names.
- **Fixed choices worth revisiting:** black and white, double-sided,
  first-class mail, `use_type: operational`.

### M5: Documents ✅
- PDF: imported by path, stored unchanged, hashed, with the real page count
  (`documents/pdf.rs`, `lopdf`). Rejected if not a PDF, damaged,
  password-protected, over 10 MiB or 100 pages, or if any page is not
  upright US Letter.
- File access is default deny: `serve --document-dir DIR` (repeatable) names
  the directories PDFs may be read from. Paths are resolved through
  symlinks and `..` first. Without the option no file is read, so a client
  cannot use the server to read and mail arbitrary files.
- Markdown and text → HTML (`documents/render.rs`, `pulldown-cmark`). Text
  is escaped. In Markdown, raw HTML prints as text, images are reduced to
  their alt text and links to "text (address)", so the page loads nothing.
  Markdown keeps the 12-point monospaced grid, so the page estimate holds.
- Lob: a PDF is uploaded as multipart with the other fields as form fields.
  A letter is one PDF or any number of text/Markdown documents, not a mix.
- **Not yet:** DOCX, HTML and image input; merging several PDFs;
  `documents_preview`; page sizes other than US Letter. Text and Markdown
  page counts are still estimates.

### Login ✅
- `postal-mcp login [provider] [--client NAME]` (`admin/login.rs`): hidden key
  prompt → verify with the provider → price (providers that cannot quote) →
  spending limits → one confirmation → store connection, price and
  grant. Reuses an existing connection if there is one. The environment comes
  from the key's prefix where the provider has one (Lob).
- It does not ask for a client: the name is unverifiable in local mode, so
  the grant goes to `default` (what `serve` uses) unless `--client` is given.
- Prices are exact decimals (`core::Rate`, any number of digits up to 12
  decimal places of a cent). A letter's cost is computed from them and
  rounded up to a whole cent once; limits, reservations and the ledger stay
  in whole cents (`core::Money`).
- `postal-mcp logout [connection id]` disconnects and deletes the key.
- Fixed in `login`: US only, USD, letters, no expiry. Use `admin grant` for
  anything else.

### M6: MVP demo (§29) as an integration test
- Connect → grant "domestic letters < $3" → send → status → revoke → send denied.
- Passing on the mock provider (`send_a_letter_then_revoke` in
  `tests/delegation.rs`).
- Written for Lob's test environment but **not yet run**: `tests/lob_live.rs`,
  ignored by default because it reaches api.lob.com. It connects with the key
  from the environment, sends a Markdown letter and a two-page PDF through the
  real binary, reads Lob's delivery report, revokes, and sees the next send
  denied. It refuses any key that does not start with `test_`.
  ```sh
  LOB_TEST_API_KEY=test_... cargo test --test lob_live -- --ignored
  ```
  Its first run is also the check of the M4 "to confirm" list.

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
9. Local client identity is the `serve --client` name (open question 4).

## 4. Risks
- **An AI with shell access shares the machine with the control plane.** A
  coding agent can run the admin CLI and read env vars. The TTY confirmation
  and keychain storage stop the unattended path, but they are not a boundary
  against an agent with full shell access as the same OS user. That needs
  the hosted mode (§7.2) or a separate OS user; say so in the threat model.
- **rmcp churn.** The SDK is at 3.x and moves fast. Keep all MCP code in `src/mcp/` so upgrades stay local.
- **Pricing.** Unattended sends need a bounded cost, and Lob doesn't quote per request.
- **Client support for 2026-07-28 features** (URL elicitation, stateless discovery) varies (§8.6). Test with Claude Code early.
