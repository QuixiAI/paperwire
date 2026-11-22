# Universal Correspondence MCP

## Design & Architecture Specification

**Version:** 0.5  
**Status:** Proposed  
**Date:** November 9, 2026  
**Working repository name:** `paperwire`  
**Product name:** Universal Correspondence MCP  
**Primary language:** Rust  
**Protocol:** Model Context Protocol (MCP)  
**License:** MIT or Apache-2.0

---

# 1. Executive Summary

Universal Correspondence MCP is an open-source, provider-agnostic MCP server that allows users to connect their postal mail, fax, and virtual mailbox accounts and delegate specific capabilities to AI agents.

The server provides a standardized interface through which AI assistants and applications can:

- Send physical letters and postcards.
- Send faxes.
- Receive physical mail through virtual mailbox providers.
- Receive faxes.
- Read incoming documents and correspondence.
- Request document scans.
- Forward or dispose of physical mail.
- Track deliveries and transmissions.
- Retrieve receipts and available evidence.
- Discover and use multiple providers.

The system does not provide postal, mailroom, or telecommunications infrastructure itself.

Instead, it connects to existing service providers and allows users to control what an AI may do with those accounts.

## 1.1 Core proposition

**Connect your accounts. Delegate capabilities. Let AI handle physical correspondence.**

The MCP server acts as a secure interoperability and authorization layer between AI applications and real-world correspondence providers.

## 1.2 Example user experience

A user says:

> Connect my Lob and Phaxio accounts. Allow my AI assistant to send domestic letters costing less than $3 each, up to $30 per month. Let it read incoming faxes, but require approval before sending a fax.

The user completes any required provider account setup and authorization.

The server stores the connections and enforces the user's delegation policy.

The AI can subsequently:

- Send permitted letters without additional server-side approval.
- Read incoming faxes.
- Prepare and preview outgoing faxes.
- Request approval when an action falls outside its delegated authority.

The AI cannot:

- Increase its spending limit.
- Change its own permissions.
- Access unconnected accounts.
- Read resources outside its granted scope.
- Send unauthorized faxes.
- Retrieve underlying provider credentials.

## 1.3 Primary design principle

**The user controls the accounts and grants authority. The AI requests actions. The MCP server enforces permissions. The provider performs the real-world operation.**

---

# 2. Scope and Responsibilities

The project should remain an MCP integration service rather than evolve into an unnecessarily large correspondence-management application.

## 2.1 In scope

The core server SHALL support:

1. Provider discovery and capability inspection.
2. Account connection and onboarding assistance.
3. Secure connection and credential management.
4. User-defined delegation policies.
5. Per-operation permission enforcement.
6. Provider-independent MCP tools.
7. Document import and format normalization.
8. Outbound postal and fax operations.
9. Inbound postal and fax retrieval.
10. Provider tracking and status queries.
11. Provider-supported physical mailroom actions.
12. Minimal persistent state for reliability and auditing.
13. Provider adapters.
14. Test/sandbox execution.
15. Bounded autonomous operation.

## 2.2 Out of scope

The core server SHALL NOT be responsible for:

- Operating a physical mailroom.
- Operating a fax telephone network.
- Providing its own postal delivery service.
- Acting as a full customer relationship manager.
- Building a comprehensive document management system.
- Automatically composing every piece of correspondence.
- Providing legal advice.
- Determining whether correspondence satisfies legal filing requirements.
- Maintaining a general-purpose email service.
- Supplying payment or banking services.
- Guaranteeing physical delivery.
- Guaranteeing that all providers support identical features.

The project MAY implement supporting services necessary for safe, reliable MCP operations.

Examples include PDF conversion, temporary document storage, provider event synchronization, authorization records, and submission reconciliation.

## 2.3 Architectural boundary

The system consists of four distinct parts.

| Component | Responsibility |
|---|---|
| MCP server | Expose tools and resources; authenticate requests; enforce delegated authority |
| Account and delegation service | Connect provider accounts; manage grants and credentials |
| Provider adapters | Translate operations into vendor-specific APIs |
| External providers | Actually print, mail, transmit, receive, scan, and fulfill correspondence |

The account/delegation service is conventional application infrastructure used by the MCP server. It is not a separate MCP protocol requirement.

---

# 3. Supported Communication Capabilities

The server should expose capabilities instead of assuming every provider supports the same operations.

## 3.1 Outbound postal mail

Capabilities:

- Send letters.
- Send postcards.
- Send greeting cards.
- Send multipage documents.
- Specify printing options.
- Specify postal service.
- Request Certified Mail where supported.
- Request return receipts where supported.
- Track fulfillment.
- Retrieve available evidence.

## 3.2 Inbound postal mail

Capabilities:

- List virtual mailboxes.
- Retrieve mailbox addresses.
- List incoming physical mail.
- Retrieve envelope images.
- Request contents scanning.
- Retrieve scanned PDFs.
- Forward original physical mail.
- Request shredding or disposal.
- Retrieve mailroom status information.

These require a provider-operated receiving address.

## 3.3 Outbound fax

Capabilities:

- Send PDF documents.
- Send converted DOCX documents.
- Send multiple attached documents.
- Select an authorized fax account or sending number.
- Set supported transmission options.
- Track transmission.
- Retrieve transmission results and reports.

## 3.4 Inbound fax

Capabilities:

- List configured fax numbers.
- Retrieve incoming faxes.
- Download received PDFs.
- Retrieve caller information when available.
- Retrieve receipt timestamps.
- Retrieve transmission metadata.
- Search or filter received faxes where supported.

## 3.5 Capability taxonomy

Use normalized capability identifiers.

```text
postal.send.letter
postal.send.postcard
postal.send.certified
postal.read.status
postal.read.evidence

mailbox.read.items
mailbox.read.envelopes
mailbox.read.scans
mailbox.request.scan
mailbox.request.forward
mailbox.request.shred

fax.send
fax.read.inbox
fax.read.document
fax.read.status
fax.read.confirmation

endpoint.read
endpoint.provision
endpoint.release
```

These are application-level capability names, not MCP protocol-defined scopes.

Providers advertise which capabilities are actually available to the connected account.

---

# 4. Account Setup and Connection Management

Account setup is a first-class product feature.

The server should help users establish or connect service-provider accounts, but must respect each provider's actual onboarding and authorization requirements.

## 4.1 Connection lifecycle

```text
User wants a capability
          |
          v
Discover eligible providers
          |
          v
Select provider
          |
          v
Existing account?
    /           \
  Yes            No
   |              |
   v              v
Authorize      Provider signup
connection     and verification
    \            /
     \          /
          v
Store connection securely
          |
          v
Verify API access
          |
          v
Discover account capabilities
          |
          v
User delegates permissions
          |
          v
MCP tools become usable
```

## 4.2 Supported authentication methods

Provider connections may use:

- OAuth authorization.
- Provider-issued API keys.
- Provider-issued access tokens.
- Other officially documented authentication mechanisms.

The server should prefer narrowly scoped, revocable authorization mechanisms when available.

## 4.3 OAuth connections

Where a provider supports suitable OAuth authorization:

1. User requests a connection.
2. Server creates a connection attempt.
3. User is directed to a trusted authorization URL.
4. User authenticates with the provider.
5. User authorizes the requested access.
6. Server validates the callback and authorization state.
7. Server securely stores the resulting credentials.
8. Server verifies access and available capabilities.

Use appropriate OAuth security measures, including state validation, PKCE where applicable, issuer validation, and appropriate redirect URI handling.

## 4.4 API-key connections

Where providers rely on API keys:

1. User selects the provider.
2. Server opens a secure connection-management page or approved local setup flow.
3. User enters the key directly into that trusted interface.
4. Key is stored in protected credential storage.
5. Server tests the connection.
6. Server records the available account capabilities.

Provider credentials must not be entered through ordinary model-visible MCP inputs or elicitation forms.

In the local implementation the approved setup flow is the command line: `paperwire login` (or `paperwire admin connect <provider>`) prompts for the key on an interactive terminal and stores it in the OS credential store (§20.2).

## 4.5 New account creation

The server MAY facilitate creating a new provider account.

However, automated registration is available only where explicitly supported by the provider.

The user may need to complete:

- Provider signup.
- Email verification.
- Identity verification.
- Payment setup.
- Service agreements.
- Postal mailbox authorization.
- Fax number provisioning.

The MCP should report what is required and provide a secure route to complete it.

It must never claim an account has been created until the provider confirms it.

## 4.6 Connection states

```text
NOT_CONNECTED
CONNECTING
ACTION_REQUIRED
CONNECTED
DEGRADED
EXPIRED
DISCONNECTED
```

A connected account must have a stable internal ID.

Example:

```json
{
  "connection_id": "conn_lob_001",
  "provider": "lob",
  "status": "connected",
  "environment": "live",
  "capabilities": [
    "postal.send.letter",
    "postal.read.status"
  ]
}
```

## 4.7 Connection revocation

Users must be able to disconnect accounts.

Disconnection should:

- Prevent new provider operations.
- Disable associated delegation grants.
- Invalidate locally cached connection authorization.
- Revoke provider tokens where supported.
- Preserve minimal historical records required for auditing.

Disconnecting a provider does not necessarily cancel orders already accepted by that provider.

---

# 5. Delegated Authority

Delegation is the central security and product feature.

Users must be able to grant AI clients specific abilities over specific connected accounts.

## 5.1 Delegation model

A delegation grant defines:

- Who is authorized.
- Which connected accounts may be used.
- Which operations are permitted.
- Which resources are accessible.
- Which restrictions apply.
- Which financial limits apply.
- When the grant expires.
- Whether additional approval is required.

## 5.2 Delegation principal

Authority should be granted to a verifiable principal such as:

- An authenticated MCP client/application.
- A particular user-and-client combination.
- A locally configured trusted installation.
- An independently authenticated automation identity.

An agent name or user ID supplied in tool arguments is not sufficient proof of identity.

The effective principal must come from trusted authentication and server-side configuration.

A hosted installation should distinguish different clients and users.

A local `stdio` installation normally operates within the trust boundary of the local process and operating-system account; it should not claim strong isolation between multiple AI clients that share the same environment and credentials.

## 5.3 Example delegation policy

```json
{
  "grant_id": "grant_001",
  "principal_id": "client_personal_assistant",
  "connection_ids": [
    "conn_lob_001"
  ],
  "actions": [
    "postal.send.letter",
    "postal.read.status"
  ],
  "constraints": {
    "destination_countries": ["US"],
    "max_pages_per_item": 10,
    "max_cost_per_item": {
      "currency": "USD",
      "amount_minor": 300
    },
    "monthly_budget": {
      "currency": "USD",
      "amount_minor": 3000
    },
    "max_items_per_day": 20,
    "allowed_products": ["letter"],
    "require_additional_approval": false
  },
  "expires_at": "2027-01-01T00:00:00Z"
}
```

The values and IDs are illustrative.

## 5.4 Delegation categories

### Read

Examples:

- List inbox items.
- Read previously scanned documents.
- Retrieve fax history.
- Retrieve mailing status.
- Read available provider receipts.

### Prepare

Examples:

- Import a document.
- Convert DOCX to PDF.
- Preview a postal letter.
- Prepare fax artwork.
- Request quotes.

### Send

Examples:

- Send a physical letter.
- Send a postcard.
- Send a fax.

### Physical actions

Examples:

- Open and scan an envelope.
- Forward original physical mail.
- Shred an item.

### Administrative actions

Examples:

- Connect a provider.
- Revoke a connection.
- Create a delegation.
- Raise a spending limit.
- Provision a paid receiving endpoint.

Administrative actions belong to a trusted management interface rather than normal model-callable tools.

## 5.5 Effective authorization

Permission must be granted by every relevant layer.

```text
Authenticated principal
        AND
MCP client authorization
        AND
User delegation grant
        AND
Connected provider account
        AND
Provider account capabilities
        AND
Action-specific policy constraints
        AND
Applicable safety restrictions
        =
Action permitted
```

No layer may override a denial from another layer.

## 5.6 Default policy

The default policy is deny.

Connecting an account does not automatically authorize an AI to use every capability of that account.

A user must explicitly delegate the relevant permissions.

---

# 6. Delegation Management

Delegation management should be accessible to the user independently of the language model.

## 6.1 Management interface

A minimal management interface should allow users to:

1. See connected providers.
2. See available capabilities.
3. Select an AI client.
4. Grant or deny actions.
5. Select allowed provider accounts.
6. Set spending limits.
7. Set volume limits.
8. Configure recipient restrictions.
9. Configure expiration dates.
10. Require additional approval for sensitive actions.
11. Review recent activity.
12. Revoke capabilities immediately.

The interface may be:

- A local web page.
- A local CLI.
- An authenticated hosted settings page.
- Another independently trusted management surface.

## 6.2 Grant creation

AI agents may help the user understand available options.

For example:

"To send letters automatically, you need postal.send.letter permission."

The AI may initiate an approval/setup session.

However, the AI must not directly issue a valid grant to itself.

Only the trusted administrative system can create or modify effective permissions.

In the local implementation, `paperwire login` and the admin commands that connect accounts or create grants require confirmation on an interactive terminal and refuse to run when standard input is not a terminal. No flag skips this. It keeps an AI client with shell access from running them unattended; §20.3 states the limits of that protection.

## 6.3 Revocation

Grants must be revocable.

Revocation must take effect before any new protected provider operation.

Pending jobs must be rechecked before execution.

An already completed provider action cannot necessarily be reversed.

## 6.4 Expiration

Delegation grants may be:

- Session-limited.
- Time-limited.
- Volume-limited.
- Budget-limited.
- Indefinite until revoked.

For powerful permissions, encourage explicit expiration.

## 6.5 Auditability

Record:

- Grant creation.
- Grant modification.
- Grant revocation.
- Principal identity.
- Connection ID.
- Authorized action.
- Authorization decision.
- Applicable policy version.

Avoid storing private document contents in authorization logs.

---

# 7. MCP Authorization Versus Provider Authorization

The design must distinguish three different authorization relationships.

## 7.1 User to provider

The provider establishes the user's account and permissions.

Examples:

- Lob account credentials.
- Phaxio API credentials.
- Stable mailbox account.
- Provider billing agreement.

## 7.2 MCP client to MCP server

For remote MCP over HTTP, use the MCP authorization specification and a suitable OAuth authorization service.

The MCP server acts as a protected resource server.

Clients present access tokens issued specifically for that MCP resource.

The server must validate token identity, intended audience/resource, scopes, issuer, and expiration as applicable.

For local MCP over `stdio`, use secure local configuration and operating-system credential mechanisms rather than assuming remote OAuth applies.

## 7.3 Delegation inside the correspondence server

The delegation engine determines the allowed operations against connected providers.

These restrictions are distinct from both the upstream provider's permissions and the MCP transport authorization scopes.

## 7.4 No token passthrough

An MCP client token must not simply be forwarded as authorization to a postal or fax provider.

The server maintains separate provider credentials associated with a verified user connection.

The upstream credentials are selected and used only after evaluating the caller's delegated authority.

## 7.5 Account ownership

A provider connection belongs to a verified user or organizational principal.

Connection IDs alone do not grant access.

Every operation must verify that the authenticated principal has permission to use the requested connection.

---

# 8. MCP Protocol Conformance

The implementation should target the official MCP specification revision `2026-07-28`.

## 8.1 MCP primitives

Use standard MCP primitives where appropriate.

### Tools

For operations such as:

- Discover providers.
- Start account connection.
- Prepare documents.
- Send correspondence.
- Retrieve inbox items.
- Request mailroom actions.

### Resources

For identifiable data such as:

- Document previews.
- Received scans.
- Provider status records.
- Delivery receipts.
- Transmission reports.

### Prompts

Optional convenience prompts may guide common workflows, but the server should not depend on them.

## 8.2 Stateless protocol

The July 2026 MCP protocol revision uses stateless request handling.

Do not rely on persistent MCP transport sessions or legacy initialization semantics for application state.

Persist explicit application handles such as:

- `connection_id`
- `connection_attempt_id`
- `document_id`
- `draft_id`
- `quote_id`
- `job_id`
- `approval_id`

These handles must be scoped to the authenticated user and must not function as unrestricted bearer credentials.

## 8.3 Authorization-aware tool discovery

Tool availability may vary according to the authenticated request's authorization.

For example, an AI client with fax read access may see inbound fax tools without receiving access to fax-sending capabilities.

Actual tool execution must independently enforce permissions.

Tool visibility alone is not a security boundary.

## 8.4 User interaction

Where supported, use secure URL-based elicitation for account connection and sensitive authorization workflows.

The server must not collect passwords, API keys, access tokens, or payment credentials through ordinary MCP form elicitation.

For modern MCP, multi-round-trip requests may be used when a tool requires additional user interaction.

Clients without the relevant elicitation support should receive a secure management URL or a structured action-required result.

## 8.5 Long-running operations

Physical mail processing and fax transmission can take time.

Return stable job identifiers and support status retrieval.

For long-running workflows, use durable application jobs.

Optional MCP Tasks extension support can be added where useful, but must not be required by clients that lack the extension.

## 8.6 Client compatibility

Test against multiple MCP clients.

Document which features are supported by each client, especially:

- Remote OAuth.
- URL elicitation.
- Resources.
- Long-running task support.
- Structured tool results.

Do not assume all clients implement every optional feature.

---

# 9. MCP Tool Interface

Keep the tool surface small, predictable, and provider-independent.

## 9.1 Provider discovery

| Tool | Purpose |
|---|---|
| `providers_list` | List supported providers |
| `providers_capabilities` | Discover services supported by a provider |

These operations may be available before connecting an account.

## 9.2 Account connections

| Tool | Purpose |
|---|---|
| `connections_list` | List the caller's authorized account connections |
| `connections_start` | Start a secure connection or onboarding workflow |
| `connections_status` | Check connection progress and account readiness |

Connections are completed through the trusted onboarding system.

Do not expose a model-callable tool that accepts raw API keys.

## 9.3 Permissions

| Tool | Purpose |
|---|---|
| `permissions_get` | Return the caller's effective delegated capabilities |
| `permissions_check` | Check whether a proposed operation is permitted |

These tools are read-only.

Permission grants and administrative changes occur through the trusted management interface.

## 9.4 Documents

| Tool | Purpose |
|---|---|
| `documents_prepare` | Import and normalize documents |
| `documents_preview` | Preview final outgoing artwork |

## 9.5 Sending correspondence

| Tool | Purpose |
|---|---|
| `correspondence_quote` | Get eligible provider prices |
| `correspondence_send` | Submit a permitted postal or fax job |
| `correspondence_status` | Query job status |
| `correspondence_evidence` | Retrieve provider receipts and evidence |

## 9.6 Receiving correspondence

| Tool | Purpose |
|---|---|
| `endpoints_list` | List permitted receiving endpoints |
| `correspondence_inbox` | List received postal items and faxes |
| `correspondence_read` | Retrieve an authorized incoming item and its documents |

## 9.7 Physical mailroom operations

| Tool | Purpose |
|---|---|
| `mailroom_scan` | Request content scanning |
| `mailroom_forward` | Request physical forwarding |
| `mailroom_shred` | Request destruction |

These actions require independent capability grants and any applicable approval.

## 9.8 Optional additions

Potential future tools:

- `connections_health`
- `providers_compare`
- `correspondence_cancel`
- `endpoints_status`
- `fax_numbers_list`
- `correspondence_list_history`

Avoid creating dozens of tools before there is a demonstrated need.

---

# 10. Example Account Connection

User:

"Connect my Phaxio account."

The AI invokes:

`connections_start`

```json
{
  "provider": "phaxio"
}
```

Example response:

```json
{
  "connection_attempt_id": "attempt_123",
  "status": "action_required",
  "action": {
    "type": "open_secure_url",
    "url": "https://mcp.example/connect/attempt_123"
  }
}
```

The URL above is illustrative.

The user completes the connection in a browser or trusted local management interface.

The server subsequently returns:

```json
{
  "connection_attempt_id": "attempt_123",
  "status": "connected",
  "connection_id": "conn_phaxio_001",
  "capabilities": [
    "fax.send",
    "fax.read.inbox",
    "fax.read.document",
    "fax.read.status"
  ]
}
```

The returned capabilities must reflect the real account and provider environment.

The existence of a connected account does not automatically grant all these capabilities to the AI.

The user must separately authorize the relevant actions.

---

# 11. Example Permission Evaluation

User:

"Can you fax this document?"

The AI calls:

`permissions_check`

```json
{
  "connection_id": "conn_phaxio_001",
  "action": "fax.send",
  "context": {
    "destination": "+12025550199",
    "page_count": 4
  }
}
```

Response:

```json
{
  "allowed": false,
  "reason": "ACTION_NOT_DELEGATED",
  "approval_available": true
}
```

The AI may tell the user that permission is required.

It may initiate a secure approval workflow if available, but cannot approve the request itself.

Once authorization is completed independently, the operation may be retried.

The server must re-evaluate permission when the actual sending operation occurs.

---

# 12. Document Support

Document processing is a supporting MCP capability, not the product's primary identity.

## 12.1 Supported formats

| Format | Priority |
|---|---|
| PDF | Required |
| DOCX | Required |
| Markdown | Required |
| Plain text | Required |
| HTML | Required |
| PNG/JPEG | Required |
| ODT | Optional |
| RTF | Optional |
| TIFF | Optional |
| WebP/HEIC | Future |
| Legacy DOC | Future |

## 12.2 Canonical output

Postal letters should generally be normalized to print-ready PDF.

Fax documents should be normalized into provider-compatible fax artwork or PDF.

Postcards may require separate front/back artifacts.

## 12.3 Multiple documents

Support one correspondence item containing multiple documents.

For example:

- Cover letter.
- Supporting exhibit.
- Scanned image.
- Additional attachment.

The server preserves their order.

## 12.4 Document safety

Converters must:

- Disable macros.
- Avoid executing scripts.
- Block external resource loading by default.
- Enforce file and page limits.
- Use isolated conversion processes.
- Detect corrupt files.
- Check page dimensions.
- Preserve original document hashes.

## 12.5 Document access

Imported or received documents belong to a specific user or authorized account context.

Document IDs are not sufficient authorization.

The server must check resource-level access for every document read.

A local server may import a document from a file path only inside directories the user has explicitly allowed (`serve --document-dir`). By default it reads no files, so an AI client cannot use it to read and then mail files it was never given.

## 12.6 Agent document generation

The MCP server should not duplicate a general-purpose AI document-authoring system.

Agents can create content using their existing tools and pass files or inline content to the correspondence server.

The correspondence server handles validation and provider-compatible rendering.

---

# 13. Sending Operations

The sending workflow should be the same regardless of provider.

## 13.1 Outbound workflow

```text
Prepare document
       |
       v
Validate destination
       |
       v
Validate provider compatibility
       |
       v
Preflight artwork
       |
       v
Get quote
       |
       v
Evaluate delegated authority
       |
       +--> Allowed
       |       |
       |       v
       |    Submit
       |
       +--> Approval required
       |       |
       |       v
       |    Trusted approval flow
       |       |
       |       v
       |    Revalidate and submit
       |
       +--> Denied
               |
               v
          Return reason
```

## 13.2 Postal requirements

Postal sending may specify:

- Product.
- Sender.
- Recipient.
- Documents.
- Color.
- Duplex.
- Paper size.
- Postal service.
- Tracking.
- Certified Mail.
- Return receipt.

Every required option must be validated against provider capabilities.

## 13.3 Fax requirements

Fax sending may specify:

- Sending account.
- Authorized sender number.
- Recipient number.
- Documents.
- Page count.
- Transmission options.
- Retry settings.

Phone numbers should use an appropriate normalized representation such as E.164.

The server should not invent or spoof a sender identity.

## 13.4 Price handling

A price may be:

- Firm.
- Estimated.
- Unavailable.

The server must not invent prices.

Autonomous sending is allowed only when the actual financial exposure is bounded by an enforceable authorization.

If the provider cannot guarantee or sufficiently bound the eventual charge, the server should require a new approval or reject unattended submission.

## 13.5 Provider selection

Routing strategies may include:

- Cheapest compatible provider.
- Preferred provider.
- Fastest supported service.
- Highest evidence capability.
- Explicit provider selection.

Routing must satisfy every required capability.

Do not silently downgrade Certified Mail, tracking, or other mandatory features to reduce price.

---

# 14. Receiving Operations

Inbound operations are normally read-oriented but may involve sensitive personal information.

## 14.1 Inbound fax

Providers may deliver:

- Fax metadata.
- Sender number.
- Receiving number.
- Received timestamp.
- Page count.
- PDF document.
- Transmission details.

The MCP server makes authorized content available through tools and resources.

## 14.2 Inbound postal mail

Virtual mailbox providers may expose:

- Envelope images.
- Sender and recipient information.
- Arrival dates.
- Contents scans.
- Physical custody status.
- Forwarding options.
- Disposal options.

## 14.3 Receiving endpoints

An endpoint is a provider-managed mailbox address or fax number.

```json
{
  "endpoint_id": "endpoint_123",
  "connection_id": "conn_mailbox_001",
  "type": "postal_mailbox",
  "status": "active",
  "capabilities": [
    "mailbox.read.items",
    "mailbox.request.scan"
  ]
}
```

## 14.4 Receiving limitations

The server cannot independently receive postal items.

It requires an actual physical mailbox provider.

Similarly, inbound fax requires an active receiving number or provider service.

Mailbox setup may require identity verification, authorization forms, or monthly charges.

Those remain provider responsibilities.

## 14.5 Provider synchronization

Support:

- Provider webhooks.
- Polling.
- User-triggered refresh.

Do not require an MCP client to remain continuously connected.

The server may store minimal cursor and event state to avoid missing or duplicating incoming records.

---

# 15. Mailroom Actions

Physical mailroom operations are distinct from reading scans.

They may cause real-world changes.

## 15.1 Scan

A scan request may:

- Incur charges.
- Require an envelope to be opened.
- Create new digital content.

The user's grant must explicitly permit the operation.

## 15.2 Forward

Forwarding requires:

- Authorized receiving item.
- Forwarding destination.
- Provider capability.
- Financial authorization.
- Any required additional approval.

## 15.3 Shred

Shredding is potentially irreversible.

A delegation granting general read access must never imply authority to shred.

Shredding requires a separate, explicit permission.

The policy may require human approval for every item even when the capability is delegated.

## 15.4 Destruction evidence

The server should retain the provider's actual disposal confirmation when available.

Do not invent certificates of destruction or claim a document was destroyed merely because a request was submitted.

---

# 16. Policy Enforcement Engine

The policy engine is the central internal component.

It must operate independently of language-model reasoning.

## 16.1 Evaluation inputs

Each protected operation is evaluated against:

- Authenticated principal.
- Connected account.
- Requested action.
- Target resource.
- Destination.
- Product.
- Document characteristics.
- Estimated or maximum charge.
- Current spending usage.
- Current volume usage.
- Time and policy expiration.
- Additional approval requirements.

## 16.2 Evaluation result

Example:

```json
{
  "decision": "allow",
  "grant_id": "grant_001",
  "policy_version": 3,
  "constraints_satisfied": true,
  "additional_approval_required": false
}
```

Possible decisions:

- `allow`
- `deny`
- `approval_required`

## 16.3 Enforcement timing

Evaluate authorization:

1. When an operation is first requested.
2. When a price quote is selected.
3. Immediately before external provider execution.

For queued operations, re-evaluate after the job is dequeued.

An expired or revoked grant must prevent new execution.

## 16.4 Atomic budgets

Concurrent requests must not bypass spending limits.

The system must atomically reserve authorized spend before submitting paid jobs.

A reservation should have:

- Unique operation ID.
- Grant ID.
- Authorized maximum amount.
- Currency.
- Expiration.
- State.

Reconcile the reservation when the actual charge is known.

If the outcome is uncertain, keep the reservation until reconciliation rather than automatically releasing it.

## 16.5 Recipient controls

Delegations may restrict:

- Specific approved recipients.
- Address patterns.
- Countries.
- Fax numbers.
- Domains or organizations where reliable identifiers exist.

Recipient data must be normalized before policy evaluation.

Do not rely on untrusted organization names alone to enforce security-sensitive recipient restrictions.

## 16.6 Usage limits

Support limits such as:

- Sends per day.
- Sends per month.
- Maximum pages per job.
- Maximum cost per job.
- Maximum total monthly spend.
- Maximum fax destinations.
- Allowed countries.
- Allowed products.

All limits must be enforced server-side.

---

# 17. Provider Adapter Architecture

Providers should be implemented as modular adapters.

## 17.1 Initial postal providers

| Provider | Priority |
|---|---|
| Lob | P0 |
| PostGrid | P0 |
| Click2Mail | P1 |
| PostalMethods | P1 |
| Mailform | P1 |
| PostalForm Projects | P1 |
| MailStream | P2 |
| Pingen | P2 |
| thanks.io | P2 |
| Stannp | P2 |

## 17.2 Initial fax providers

| Provider | Priority |
|---|---|
| Phaxio | P0 |
| Sinch Fax | P0 |
| Fax.Plus | P1 |

## 17.3 Initial inbound mailbox providers

| Provider | Priority |
|---|---|
| Stable | P0 |
| Earth Class Mail | P1 |
| PostScan Mail | P2 |
| PostGrid Virtual Mailboxes | P2 |
| mailbox.bot | P2 |

These are proposed integration targets. Actual API availability, costs, supported operations, and account-level permissions require adapter-specific verification.

## 17.4 Adapter responsibilities

Adapters handle:

- Connection verification.
- Capability discovery.
- Provider authentication.
- Request translation.
- Document submission.
- Quote retrieval.
- Status normalization.
- Inbound item retrieval.
- Provider events.
- Error translation.
- Provider-specific idempotency.
- Cancellation where supported.

Adapters do not grant permissions.

Every provider operation passes through the central policy engine.

## 17.5 Illustrative adapter interface

```rust
trait ProviderAdapter: Send + Sync {
    fn info(&self) -> ProviderInfo;

    /// Check the credential and report what the account can do.
    fn verify<'a>(
        &'a self,
        environment: Environment,
        credential: Option<&'a Secret>,
    ) -> BoxFuture<'a, Result<Vec<Capability>, ProviderError>>;

    fn quote<'a>(&'a self, item: &'a MailItem) -> BoxFuture<'a, Result<Price, ProviderError>>;

    /// The operation id inside `authorized` is the idempotency key.
    fn submit<'a>(
        &'a self,
        authorized: &'a AuthorizedOperation,
        account: Account<'a>,
        item: &'a MailItem,
        documents: &'a [Document],
    ) -> BoxFuture<'a, Result<Submission, SubmitError>>;

    /// Find an earlier submission, to reconcile an uncertain outcome.
    fn lookup<'a>(
        &'a self,
        operation_id: &'a str,
        account: Account<'a>,
    ) -> BoxFuture<'a, Result<Lookup, ProviderError>>;
}
```

Adapters are stored as `dyn ProviderAdapter`, so async methods return boxed futures. `submit` fails as either `Rejected` (definitely not accepted) or `Uncertain`; the two are never merged (§19.2).

Channel-specific adapters extend this interface with typed operations.

`AuthorizedOperation` must be created internally after authorization. The AI must not be able to supply or forge it. In Rust its constructor is private to the policy module, so code that has not passed a policy decision cannot build one.

## 17.6 Adapter certification

Every provider adapter should pass shared contract tests for:

- Authentication.
- Capabilities.
- Environment isolation.
- Error normalization.
- Authorization boundaries.
- Price handling.
- Safe submission.
- Reconciliation.
- Resource access.

Provider-specific operations should be documented as extensions rather than silently altering the core contract.

---

# 18. Data Model

Avoid building an unnecessarily large correspondence database.

Persist only the information required for:

- Account connections.
- Delegation.
- Authorization.
- Reliable job execution.
- Resource access.
- Usage accounting.
- Basic auditing.

## 18.1 Required entities

### User

Authenticated owner of one or more connections.

### Client principal

Authenticated application or installation receiving delegated authority.

### Provider connection

Secure reference to an external service account.

### Delegation grant

Capabilities and restrictions assigned to a principal.

### Document

Imported or retrieved artifact with access controls.

### Operation

Requested action against a provider.

### Job

Durable record of an asynchronous provider operation.

### Usage ledger

Financial and volume accounting for delegation limits.

### Audit event

Security-relevant action or state change.

## 18.2 Suggested tables

```text
users
client_principals
provider_connections
delegation_grants
delegation_events
documents
operations
jobs
usage_reservations
usage_ledger
audit_events
inbound_sync_cursors
provider_events
```

A full correspondence thread and CRM data model is not required for the initial implementation.

Minimal relationship metadata may be retained for replies and user-requested correlation.

## 18.3 Data minimization

Prefer provider references and document identifiers over unnecessarily duplicated content.

Cache documents only when needed.

Provide configurable retention policies.

Do not retain private faxes or scanned letters indefinitely by default.

---

# 19. Operation Reliability

Physical correspondence involves irreversible external actions.

Retries must be safe.

## 19.1 Idempotency

Every paid or consequential operation must have a stable idempotency key.

Persist an operation record before calling a provider.

Use provider-supported idempotency where available.

The adapter supplies both the key and the provider's documented header name. Attaching a key is the adapter's declaration that the provider deduplicates by that header. A key must not be attached for a provider that ignores it, because the key is what permits a retry of a non-idempotent request (§19.2).

## 19.2 Ambiguous outcomes

A provider may accept an order but the network connection may fail before returning confirmation.

The server must not blindly retry.

Instead:

1. Mark the operation uncertain.
2. Preserve the original submission reference.
3. Reconcile using provider-supported lookup mechanisms.
4. Avoid duplicate fulfillment.
5. Require operator review if the outcome cannot be determined.

The HTTP layer classifies every failure by whether the provider could have received the request, and one retry wrapper applies these rules. Adapters do not write their own retry loops.

| Outcome | Meaning | Retried automatically? |
|---|---|---|
| Not sent | No connection was established (DNS, refused, TLS handshake) | Yes, for any request |
| 429, 502, 503 or 504 response | The provider or a gateway asked to try later | Only if replay-safe: GET, PUT or DELETE, or the request carries an idempotency key (§19.1) |
| Any other response | The provider answered; the outcome is known | No |
| Response too large | The provider answered, but the body exceeded the size limit. Status and headers are kept, so a 2xx is still an accepted request | No |
| Ambiguous | The request may have been sent, but no complete response arrived (timeout, reset) | Never; follow the steps above |

Retry delays use jittered exponential backoff. A `Retry-After` header is honored; if it asks for longer than the configured maximum delay, the response is returned instead of retrying early. Redirects are not followed.

## 19.3 Revocation during a pending job

Before executing a queued job, recheck delegation authority.

If the grant has expired or been revoked, do not submit the job.

Previously accepted provider operations may continue if they cannot be canceled.

## 19.4 Provider fallback

Do not automatically submit to another provider after an ambiguous outcome.

Fallback must be compatible with the original delegation and approved material terms.

If the provider changes or the cost increases beyond authorization, obtain additional approval.

## 19.5 Status reporting

Distinguish:

- Accepted by provider.
- Being processed.
- Printed.
- Mailed.
- Transmitted.
- Delivered.
- Received.
- Failed.
- Unknown.

Do not treat provider acceptance as proof of postal delivery or fax transmission.

---

# 20. Security Architecture

## 20.1 Principle of least privilege

Every component should have only the access it needs.

MCP clients never receive raw provider credentials.

Provider adapters should access credentials only for the relevant connection.

## 20.2 Credential storage

Use:

- OS credential storage (the system keychain) for local installations. This is the default.
- Environment variables only by explicit opt-in, for CI and automated tests. An AI client with shell access can read them.
- Encrypted secrets storage for hosted deployments.
- Dedicated secret-management services when appropriate.

Never store provider API keys in plain-text audit logs. Debug output and error messages must not contain credentials, header values, URL query strings or request bodies.

Never store provider API keys in plain-text audit logs.

## 20.3 Separation of control and execution

The architecture has two control surfaces.

### Trusted control plane

Used by the account owner to:

- Connect accounts.
- Manage secrets.
- Create delegation grants.
- Modify limits.
- Revoke access.
- Provision paid services where supported.

### MCP execution plane

Used by AI applications to:

- Discover authorized capabilities.
- Prepare documents.
- Perform permitted correspondence operations.
- Retrieve authorized information.
- Request external authorization when necessary.

AI-controlled tools must not bypass the control plane.

**Limit of the local deployment.** When the AI client is a coding agent with shell access, it runs as the same OS user as the control plane. The interactive-terminal requirement (§6.2) and keychain storage (§20.2) stop unattended use of the admin commands, but they are not a security boundary against an agent with full shell access. A real boundary needs the hosted deployment (§25.1) or a control plane running as a separate OS user.

## 20.4 Prompt injection

Incoming letters, faxes, OCR text, and external documents are untrusted data.

Their contents must not modify the server's operational instructions or permissions.

A received letter cannot grant the AI authority to send mail, disclose private documents, or change a spending limit.

## 20.5 Resource authorization

Every document, job, receipt, connection, and endpoint must be checked against the current authenticated principal and applicable grants.

Opaque IDs are not sufficient proof of access.

## 20.6 Confused deputy prevention

The server must not use its broad upstream provider credentials in response to an unauthorized or improperly scoped client request.

Maintain explicit user-client consent and account binding.

Validate each requested action independently.

## 20.7 Hosted deployments

Require:

- Authenticated access.
- Tenant isolation.
- HTTPS.
- Correct token audience and issuer validation.
- Request rate limits.
- Authorization checks.
- Secure upload handling.
- Webhook authentication.
- Secret encryption.
- Audit logging.
- Revocation support.

## 20.8 High-risk actions

Actions such as shredding, provisioning paid endpoints, changing mailbox destinations, and large-volume fax sending require explicit, separately granted capabilities and appropriate controls.

## 20.9 Legal and regulatory considerations

The server should not claim that a delivered postal item constitutes legal service of process.

It should not assume that successful fax transmission satisfies a filing requirement.

Users and applications remain responsible for determining appropriate submission methods.

The server must also respect applicable restrictions on unsolicited fax advertising, privacy, and provider acceptable-use policies.

---

# 21. MCP Resources

Expose relevant resources with suitable authorization controls.

Example resource identifiers:

```text
correspondence://documents/{id}
correspondence://documents/{id}/preview
correspondence://jobs/{id}
correspondence://jobs/{id}/receipt
correspondence://inbox/{id}
correspondence://inbox/{id}/document
```

The server must authorize every resource request.

Do not expose permanent unauthenticated URLs to sensitive documents.

For large files, use authenticated or expiring download mechanisms.

---

# 22. Optional MCP Prompts

Prompts are convenience features.

Possible examples:

```text
prepare_postal_letter
prepare_fax
review_document_for_printing
summarize_incoming_correspondence
reply_to_received_letter
```

The project should not depend on prompt execution.

The MCP client or agent may already have better document composition and summarization capabilities.

Prompts do not grant authority to perform actions.

---

# 23. Configuration

A local configuration might resemble:

```toml
[server]
name = "universal-correspondence"
transport = "stdio"
environment = "test"

[storage]
database = "sqlite"
path = "~/.local/share/paperwire"

[documents]
max_file_size_mb = 25
max_pages = 100

[authorization]
default_policy = "deny"
require_delegation = true
allow_unattended_execution = true

[providers.lob]
enabled = true

[providers.phaxio]
enabled = true

[providers.stable]
enabled = true
```

Credentials are not part of the configuration file. They are stored in the OS credential store by `paperwire admin connect` (§4.4, §20.2).

This is proposed configuration, not an existing vendor schema.

`allow_unattended_execution` permits only actions covered by valid delegation grants. It does not disable authorization or user-defined limits.

A production implementation must separate provider test and live environments and use the exact authentication mechanism supported by each provider.

---

# 24. Implementation Architecture

```text
                  User
                   |
                   v
       Account / Delegation UI
                   |
          +--------+--------+
          |                 |
          v                 v
    Provider Account   Delegation
      Connections       Policies
          |                 |
          +--------+--------+
                   |
                   v
             Secure Store
                   |
                   |
AI Agent / MCP Client
          |
          v
    MCP Tool Interface
          |
          v
  Authenticated Principal
          |
          v
     Policy Enforcement
          |
          v
    Operation Dispatcher
          |
    +-----+------+---------+
    |            |         |
    v            v         v
  Postal        Fax      Mailbox
 Adapters     Adapters   Adapters
    |            |         |
    v            v         v
 External     External   External
 Providers    Providers  Providers
          |
          v
   Normalized Results
          |
          v
       MCP Client
```

## 24.1 Architecture principles

- MCP defines the AI integration interface.
- The control plane owns connections and delegation.
- The policy engine is authoritative.
- Adapters implement provider-specific operations.
- Provider APIs perform the real-world work.
- Persistent storage exists to make operations reliable.
- No AI model is required inside the MCP server.

## 24.2 Stateless transport, stateful application

The MCP protocol may be stateless, but the application maintains necessary durable state.

For example:

- Connected accounts.
- Grants.
- Pending jobs.
- Approval sessions.
- Usage totals.
- Provider identifiers.
- Inbound synchronization cursors.

Application state is addressed using explicit identifiers and authenticated access.

---

# 25. Technology Stack

| Component | Technology |
|---|---|
| Language | Rust (edition 2024) |
| Async runtime | tokio |
| MCP | `rmcp`, the official Rust SDK |
| Schemas | `serde` + `schemars` |
| Provider HTTP | `reqwest` (rustls), behind an internal `Transport` trait |
| HTTP framework (hosted mode, later) | `axum` |
| Local database | SQLite (`rusqlite`) |
| Hosted database | PostgreSQL |
| PDF inspection | `lopdf` |
| DOCX conversion | Sandboxed LibreOffice |
| HTML rendering | Sandboxed Chromium |
| Command line | `clap` |
| Configuration | TOML |
| Logging | `tracing`, to stderr (stdout carries MCP messages) |
| Testing | `cargo test` + `wiremock` |
| Local secrets | OS credential store (`keyring`) |
| Hosted secrets | Encrypted secrets manager |
| Hosted authentication | OAuth-compatible authorization service |

## 25.1 Deployment modes

### Local

- MCP over stdio.
- SQLite.
- Local document conversion.
- Secure local setup.
- Direct provider API access.

### Hosted

- MCP over Streamable HTTP.
- OAuth-based user/client authorization.
- Multi-tenant storage.
- Authenticated account-management interface.
- Background job processing.
- Webhook receivers.
- Secure object storage.

The first implementation should prioritize local operation while maintaining boundaries suitable for future hosted deployment.

---

# 26. Repository Structure

One Cargo crate (library plus binary). It is split into a workspace only when a boundary requires it.

```text
paperwire/
  README.md
  PLAN.md             # milestones and Rust-specific decisions
  design.md
  Cargo.toml

  src/
    main.rs           # `serve` (MCP over stdio) and `admin ...` (control plane)
    lib.rs
    core/             # capability identifiers, Money, errors
    http/             # Transport trait, failure classification, retry wrapper
    mcp/              # tools, resources, prompts (all MCP SDK code)
    providers/        # adapter trait, mock, lob; postgrid, phaxio, sinch, stable later

    # added by milestone
    store/            # SQLite schema and migrations
    connections/      # connection records, credential store, verification
    delegation/       # grants, policy evaluation, budgets
    operations/       # dispatch, idempotency, reconciliation
    documents/        # import, conversion, preview
    admin/            # command-line control plane

  tests/              # MCP over real stdio, HTTP and adapter contract tests
```

Adapters should be independently testable.

The core MCP tool definitions should not need to change every time a provider is added.

---

# 27. Development Roadmap

## Phase 0 — Foundation

Implement:

- Rust MCP server.
- Core schemas.
- Connection records.
- Delegation grants.
- Policy evaluator.
- Mock provider.
- SQLite persistence.
- Read-only capability discovery.

**Milestone:** A simulated AI client can use only the capabilities granted to it.

## Phase 1 — Provider connection

Implement:

- Secure account-management interface.
- API-key provider connection.
- OAuth provider connection where supported.
- Connection verification.
- Capability discovery.
- Disconnection.
- Grant creation and revocation.

**Milestone:** A user can connect a provider account and delegate a capability without disclosing credentials to the model.

## Phase 2 — Outbound postal mail

Implement:

- PDF/DOCX/Markdown input.
- Document rendering.
- Postal letter preflight.
- Lob adapter.
- PostGrid adapter.
- Quotes.
- Bounded spending.
- Safe sending.
- Status retrieval.

**Milestone:** An AI sends a real letter under a preauthorized spending policy.

## Phase 3 — Fax

Implement:

- Phaxio adapter.
- Sinch adapter.
- Fax rendering.
- Fax sending.
- Incoming fax retrieval.
- Confirmation reports.
- Fax-specific delegation controls.

**Milestone:** An AI can send and read faxes within delegated permissions.

## Phase 4 — Inbound postal mail

Implement:

- Stable adapter.
- Mailbox endpoint discovery.
- Inbound mail listing.
- Envelope images.
- Scan retrieval.
- Authorized scan requests.
- Forwarding and shredding when supported.

**Milestone:** An AI can read and manage physical mail through a connected virtual mailbox.

## Phase 5 — Provider expansion

Add:

- Click2Mail.
- PostalMethods.
- Mailform.
- PostalForm Projects.
- Additional fax services.
- Additional virtual mailbox services.
- International services.
- Postcards.
- Certified Mail.

**Milestone:** Multiple providers offer interchangeable capabilities through one MCP interface.

## Phase 6 — Production hardening

Implement:

- Hosted authorization.
- Tenant isolation.
- Distributed usage accounting.
- Webhook handling.
- Durable job workers.
- Reconciliation.
- Audit export.
- Advanced delegation policies.
- Security review.

**Milestone:** A secure multi-user deployment capable of supporting independently authorized AI clients.

---

# 28. Testing Requirements

## 28.1 Permission tests

Test that:

- Unconnected accounts cannot be used.
- Undelegated actions are denied.
- Read permission does not imply send permission.
- Send permission does not imply shredding permission.
- A revoked grant prevents new operations.
- An expired grant prevents new operations.
- Users cannot access another user's connections.
- Clients cannot forge user or agent identity.
- Administrative grants cannot be created through ordinary AI tools.

## 28.2 Budget tests

Test:

- Per-job spending limits.
- Monthly spending limits.
- Concurrent send requests.
- Budget reservation.
- Reservation reconciliation.
- Provider charge uncertainty.
- Failed submissions.
- Ambiguous submission outcomes.

## 28.3 Provider tests

Test:

- Authentication.
- Capability verification.
- Test/live separation.
- Unsupported services.
- Document requirements.
- Provider status translation.
- Retry behavior.
- Idempotency.
- Rate limits.
- API errors.

## 28.4 Security tests

Test:

- Prompt injection in incoming documents.
- Malicious PDF/DOCX input.
- Unauthorized resource reads.
- Forged webhook events.
- Expired account connections.
- Stolen or substituted job IDs.
- Cross-tenant access.
- Credential leakage.
- Revocation during queued execution.
- Confused-deputy authorization failures.

## 28.5 MCP conformance tests

Test:

- Tool listing.
- Tool invocation.
- Structured results.
- Resources.
- Authentication.
- Protocol version compatibility.
- URL-based user interaction.
- Error handling.
- Client compatibility.

---

# 29. Minimum Viable Product

The MVP should prove the core promise without implementing every provider or channel.

## Required MVP functionality

1. Local Rust MCP server.
2. Secure account connection for at least one provider.
3. User-defined delegation grants.
4. Independent policy enforcement.
5. PDF/Markdown document preparation.
6. One outbound letter provider.
7. Mock provider for testing.
8. Quote or bounded-cost evaluation.
9. Authorized live sending.
10. Status retrieval.
11. Idempotency.
12. Permission revocation.
13. Basic audit history.

## MVP demonstration

A user:

1. Installs the server.
2. Connects a postal account.
3. Grants an AI permission to send domestic letters under $3.
4. Gives the AI a document and postal destination.
5. Allows the AI to send within its delegated authority.
6. Receives the provider's real order and status information.
7. Revokes the AI's sending permission.
8. Verifies that further sends are denied.

This demonstrates the project's defining value.

Fax and inbound mail should follow using the same connection and delegation architecture.

---

# 30. Open Design Questions

Questions to resolve during implementation:

1. Should the initial repository be called `paperwire` or `correspondence-mcp`? **Decided:** `paperwire`.
2. Which provider has the lowest-friction initial account-connection workflow? **Decided:** Lob, in its test environment, is the first provider.
3. Should the first control plane be a local web UI or CLI? **Decided:** CLI first; a local web UI may follow.
4. How should individual MCP clients be identified in local deployments? **Decided:** by a name set when the server is started (`serve --client NAME`). Grants are made to that name. It is configuration, not authentication (§5.2).
5. Which providers offer sufficiently reliable final-cost information for autonomous sending? **Partly decided:** Lob does not quote, so the user sets their own price per product with `admin price`. It is used as an estimated upper bound, and nothing is sent without one.
6. Should grant policies use a custom JSON schema, a policy engine library, or a capability-based authorization model? **Decided:** a custom schema evaluated in Rust.
7. How should grants be migrated when provider capabilities change?
8. Which operations require one-time approval even under a broader delegation?
9. What is the default document-retention policy?
10. Should a future hosted version allow users to share selected provider connections with multiple AI clients?

## Recommended initial choices

- Rust.
- Local MCP over stdio.
- SQLite.
- Minimal local command-line account/delegation interface.
- Provider-issued API keys where OAuth is unavailable.
- Mock provider first.
- One outbound postal provider.
- Default-deny delegation.
- Bounded autonomous sending.
- No unnecessary correspondence database.
- Remote MCP and multi-user authorization later.

---

# 31. Core Product Principles

The following principles should guide future design decisions.

### Principle 1 — The user owns the relationship

Provider accounts belong to users or their organizations.

An AI receives delegated access, not ownership.

### Principle 2 — Capabilities are explicit

A connected provider account does not imply unrestricted AI access.

### Principle 3 — Delegation is independently enforced

The model cannot authorize itself.

### Principle 4 — Autonomy is bounded

The user may authorize autonomous execution within clear limits.

### Principle 5 — Providers remain responsible for fulfillment

The server does not impersonate a carrier, mailroom, or fax provider.

### Principle 6 — MCP is the integration interface

The system should not become a giant application simply because additional features are possible.

### Principle 7 — Security is architectural

Permissions, identity, and budgets cannot depend on prompt instructions.

### Principle 8 — Interoperability is the product

Provider-specific behavior is isolated behind adapters.

### Principle 9 — Operations must be observable

Return actual statuses, identifiers, and available evidence.

### Principle 10 — Start small

Build one real connection and one delegated real-world capability before broadening the platform.

---

# 32. Reference Documentation

## Model Context Protocol

- [Official MCP documentation](https://modelcontextprotocol.io/)
- [MCP specification — July 28, 2026](https://modelcontextprotocol.io/specification/2026-07-28)
- [MCP authorization](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization)
- [MCP tools](https://modelcontextprotocol.io/specification/2026-07-28/server/tools)
- [MCP elicitation](https://modelcontextprotocol.io/specification/2026-07-28/client/elicitation)
- [MCP security best practices](https://modelcontextprotocol.io/docs/2026-07-28/tutorials/security/security_best_practices)
- [MCP Rust SDK (`rmcp`)](https://github.com/modelcontextprotocol/rust-sdk)

## Postal Providers

- [Lob](https://docs.lob.com/)
- [PostGrid](https://www.postgrid.com/)
- [Click2Mail](https://developers.click2mail.com/)
- [PostalMethods](https://www.postalmethods.com/postal-api/)
- [Mailform](https://www.mailform.io/docs/api/)
- [PostalForm Projects](https://postalform.com/developer-mail-api)
- [MailStream](https://mailstream.app/developers/api)
- [Pingen](https://www.pingen.com/en/post-api/)

## Fax Providers

- [Phaxio](https://www.phaxio.com/docs/api)
- [Sinch Fax](https://developers.sinch.com/docs/fax/)
- [Fax.Plus](https://www.fax.plus/fax-api)

## Virtual Mailbox Providers

- [Stable](https://docs.usestable.com/)
- [Earth Class Mail](https://reference.earthclassmail.com/)
- [PostScan Mail API](https://github.com/PostScanMail/api-docs)

---

# 33. Final Product Definition

**Product:** Universal Correspondence MCP

**Working repository:** `paperwire`

**Category:** Open-source, provider-agnostic MCP integration server.

**Primary users:** Independent developers, AI agent builders, hobbyists, startups, researchers, small businesses, and organizations using physical correspondence services.

## Mission

Enable users to connect their postal, fax, and virtual mailbox accounts, delegate specific capabilities to AI agents, and securely exercise those capabilities through a universal MCP interface.

## Core capabilities

- Provider account connections.
- Secure account onboarding.
- Capability discovery.
- User-controlled delegation.
- Permission enforcement.
- Financial and usage limits.
- Outbound physical mail.
- Inbound physical mail.
- Outbound fax.
- Inbound fax.
- Document preparation.
- Provider-independent execution.
- Status and evidence retrieval.
- Revocation and auditability.

## One-sentence pitch

**Universal Correspondence MCP lets you connect your postal mail, fax, and virtual mailbox accounts and give AI agents exactly the authority they need to use them—safely, securely, and across providers.**

## North-star experience

A user connects one or more provider accounts and decides which capabilities to delegate.

The AI discovers those capabilities through MCP and performs authorized operations without needing provider-specific integration knowledge.

The server checks each operation against the user's delegation policy and actual account capabilities.

Actions outside those boundaries are denied or require independent approval.

The external provider performs the physical operation.

**The user controls the authority. The AI uses the capabilities. The MCP enforces the boundary.**

---

*End of Universal Correspondence MCP Design Specification — v0.4*