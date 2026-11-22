# paperwire

Universal Correspondence MCP: connect your postal mail, fax, and virtual
mailbox accounts and give AI agents exactly the authority they need to use
them. See [`design.md`](design.md) for the specification and
[`PLAN.md`](PLAN.md) for the implementation plan.

Status: **M5**, plus guided `login`. The MCP server runs over stdio. You can
connect an account, grant an AI client limited permissions, and the client
can prepare a text or Markdown document or a PDF, get a quote and send a
letter within those limits. It can also read the mail a virtual mailbox
has received. Three providers are built in: a mock that mails and receives
nothing, Lob for sending, and Stable for receiving. The Lob and Stable
adapters are written from each provider's documentation and tested against
local stand-ins; neither has yet been run against the provider.

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
claude mcp add paperwire -- /absolute/path/to/target/debug/paperwire serve
```

## Log in

```sh
paperwire login          # or: paperwire login lob --client claude
```

`login` asks for the provider's API key (hidden, and stored in the OS
credential store), checks it with the provider, and asks how much the AI
may spend. It then shows everything it is about to
store and asks you to confirm once. A Lob key says whether it is a test or
live key, so there is no environment to choose. For Lob it also asks what a
letter costs you, because Lob cannot quote and nothing is sent without a
known cost.

For a sending account, the permission it grants is letters within the US,
by default up to $3.00 each and $30.00 a month, with no expiry. For a
mailbox account (`paperwire login stable`) it asks instead whether the AI
may see envelope images and read scanned contents; see
[Receive mail](#receive-mail). It applies to every AI client that
runs `paperwire serve`. To give clients different limits, start each with
its own `serve --client NAME` and run `login --client NAME` for each; the
name is a label, not a check of who is connecting. `paperwire logout` disconnects the account and
deletes the stored key.

The AI cannot do this. `login` refuses to run without an interactive
terminal.

## Finer control

The `admin` commands do the same steps one at a time, with every limit
available. `connect`, `grant`, `price` and `resolve` ask you to confirm and
refuse to run without an interactive terminal.

```sh
paperwire admin connect mock
paperwire admin grant --client default --connection <connection id> \
    --action postal.send.letter --country US \
    --max-cost USD:300 --monthly-budget USD:3000 --expires-in 30d
paperwire admin grants
paperwire admin revoke <grant id>
paperwire admin operations
paperwire admin audit
```

For Lob without `login`, connect with a test key and set your price for a
letter:

```sh
paperwire admin connect lob --env test
paperwire admin price lob letter --base USD:120 --per-extra-page 20
```

A price may have fractions of a cent: `--base USD:106.6` is $1.066. The
cost of a letter is worked out from the exact price and rounded up to a
whole cent once, per letter.

Lob letters need a return address (`from`). A letter is either one PDF or
text and Markdown documents.

## Receive mail

A virtual mailbox provider receives your mail at its address and scans it.
`paperwire login stable` connects the account and asks which parts the AI
may read. A sandbox key (`stbl_dev_...`) connects to Stable's sandbox; for
any other key, answer `live` when asked for the environment. To do the same
by hand, or to grant different clients different parts:

```sh
paperwire admin connect stable --env live     # sandbox keys: --env test
paperwire admin grant --client default --connection <connection id> \
    --action endpoint.read --action mailbox.read.items \
    --action mailbox.read.envelopes --action mailbox.read.scans
```

| Permission | What the AI can read |
|---|---|
| `endpoint.read` | The mailbox's street address (`endpoints_list`) |
| `mailbox.read.items` | That mail arrived, who it is from and whether it is scanned (`correspondence_inbox`, `correspondence_read`) |
| `mailbox.read.envelopes` | A link to the envelope image |
| `mailbox.read.scans` | The provider's summary and a link to the scanned document |

`correspondence_read` leaves out any part that was not granted and names the
missing permission. Links are the provider's temporary ones and are not
stored. Reading is all there is: the server does not ask the provider to
open, scan, forward or shred anything. To try it without an account,
`paperwire admin connect mock` has a mailbox holding two made-up letters.

To have replies to a letter arrive where the AI can read them, give the
mailbox's address (with your name) as the letter's return address.

Mail is written by other people. The server marks what a sender wrote as
untrusted, but the AI client decides what to do with it: grant sending and
reading to the same client with that in mind.

To check the Lob adapter against Lob's test environment (nothing is printed
or mailed, and it refuses anything but a `test_` key):

```sh
LOB_TEST_API_KEY=test_... cargo test --test lob_live -- --ignored
```

Documents are plain text or Markdown given inline, or a PDF with US Letter
pages. The server reads PDF files only from directories you name with
`serve --document-dir`; without that option it reads no files at all.

Money is in minor units: `USD:300` is $3.00. A connected account grants the
AI nothing until you add a grant, and a revoked grant stops working on the
server's next call.

If a send ends with an unknown outcome, the server never sends it again. It
asks the provider what happened; if the provider cannot say, check with the
provider yourself and run `paperwire admin resolve <operation id>
accepted|failed`.

State is kept in `~/.local/share/paperwire` (override with `--data-dir` or
`PAPERWIRE_DATA_DIR`). API keys are stored in the OS keychain, not there.

Logs go to stderr; set the level with `PAPERWIRE_LOG` (e.g. `debug`).
