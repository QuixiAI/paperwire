# postal-mcp

Universal Correspondence MCP: connect your postal mail, fax, and virtual
mailbox accounts and give AI agents exactly the authority they need to use
them. See [`design.md`](design.md) for the specification and
[`PLAN.md`](PLAN.md) for the implementation plan.

Status: **M4.** The MCP server runs over stdio. You can connect an account,
grant an AI client limited permissions, and the client can prepare a text
document, get a quote and send a letter within those limits. Two providers
are built in: a mock that mails nothing, and Lob. The Lob adapter is written
from Lob's documentation and tested against a local stand-in; it has not yet
been run against Lob.

## Build and test

```sh
cargo build
cargo test
```

## Run

```sh
cargo run -- serve                      # MCP over stdio (the default command)
cargo run -- serve --client assistant   # name the AI client this instance serves
cargo run -- serve --document-dir ~/Letters   # let the client import PDFs from here
```

To use it from Claude Code:

```sh
claude mcp add postal -- /absolute/path/to/target/debug/postal-mcp serve
```

## Connect an account and grant permissions

The AI cannot do this. Run these yourself in a terminal: `connect` and
`grant` ask you to confirm and refuse to run without an interactive terminal.

```sh
postal-mcp admin connect mock
postal-mcp admin grant --client default --connection <connection id> \
    --action postal.send.letter --country US \
    --max-cost USD:300 --monthly-budget USD:3000 --expires-in 30d
postal-mcp admin grants
postal-mcp admin revoke <grant id>
postal-mcp admin operations
postal-mcp admin audit
```

For Lob, connect with a test key and set your price for a letter, because
Lob cannot quote and nothing is sent without a bounded cost:

```sh
postal-mcp admin connect lob --env test
postal-mcp admin price lob letter --base USD:120 --per-extra-page 20
```

Lob letters need a return address (`from`). A letter is either one PDF or
text and Markdown documents.

Documents are plain text or Markdown given inline, or a PDF with US Letter
pages. The server reads PDF files only from directories you name with
`serve --document-dir`; without that option it reads no files at all.

Money is in minor units: `USD:300` is $3.00. A connected account grants the
AI nothing until you add a grant, and a revoked grant stops working on the
server's next call.

If a send ends with an unknown outcome, the server never sends it again. It
asks the provider what happened; if the provider cannot say, check with the
provider yourself and run `postal-mcp admin resolve <operation id>
accepted|failed`.

State is kept in `~/.local/share/postal-mcp` (override with `--data-dir` or
`POSTAL_MCP_DATA_DIR`). API keys are stored in the OS keychain, not there.

Logs go to stderr; set the level with `POSTAL_MCP_LOG` (e.g. `debug`).
