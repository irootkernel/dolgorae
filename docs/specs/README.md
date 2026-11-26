# Dolgorae v2 Product Specification

This document owns the externally observable behavior and semantic invariants of
Dolgorae v2.

## 1. Product Purpose

Dolgorae v2 is a local, multi-provider, durable agent actor control layer. It
runs an independent local daemon (`dolgorae serve`) over a same-user Unix domain
socket (HTTP/1.1 JSON + Server-Sent Events) and exposes a dual-mode stdio MCP
server (`dolgorae mcp`) for external hosts and delegated actors.

Dolgorae v2 leaves native model execution and provider conversation history with
Tier-1 structured CLI runtimes while owning:

- Reusable **Role / Persona** definitions (`.dolgorae/roles/*.md` and `~/.dolgorae/roles/*.md`).
- Unified **Session / Actor / Delegation / Mailbox** lifecycle (**Model 1-A**).
- Monotonic per-session **Cursor / Revision** event ordering and durable replay.
- Interactive **Approval & User-Input** routing with optimistic concurrency.
- Bounded inline payloads with content-addressed **Artifact** fallback.
- Local process authentication and workspace access policy enforcement.

## 2. Role and Persona Model

A **Role** is a named Markdown file with YAML frontmatter describing reusable
agent behavior:

- `name`: stable identifier.
- `driver`: preferred driver ID (e.g., `codex-app`, `claude-wire`, `agy-acp`).
- `model`, `effort`: optional model and reasoning effort defaults.
- `access`: `read_only` or `workspace_write`.
- `approval_policy`: `interactive`, `auto_deny`, or `auto_approve_scoped`.
- `output_schema`: optional JSON Schema constraining final structured output.
- Markdown body: system/developer instructions injected into the driver session.

Resolution searches project-local `.dolgorae/roles/<name>.md` before user-global
`~/.dolgorae/roles/<name>.md`. Callers may override `driver`, `model`, `effort`,
`access`, `approval_policy`, or `output_schema` per session or turn.

## 3. Unified Session, Actor, Delegation, and Mailbox (1-A)

Every managed execution unit is a **Session** (actor instance) backed by one
driver process and bound to a `root_session_id`:

- A **Root Session** is started by an external caller (CLI, HTTP/SSE client, or External Host MCP), with `parent_session_id = null`, `root_session_id = session_id`, and `depth = 0`.
- A **Child Actor** is a Session created via delegation from a parent Session (`parent_session_id` set, `root_session_id` inherited from parent). It inherits workspace scoping constraints and never receives privileges exceeding its parent's `access` grant.
- **Scoped MCP Injection**: When delegation is permitted, Dolgorae injects a stdio MCP server (`dolgorae mcp --scoped`) carrying a per-actor bearer token (`DOLGORAE_ACTOR_TOKEN`). All delegation, mailbox, and artifact calls from that child are authenticated to that actor's scope.

### 3.1 Session Policy, Delegation Depth, and Cycle Detection (Invariant 4)

Every root session carries a `SessionPolicy` inherited by all descendant actors
in its session tree:

- `max_delegation_depth: u8` — default `3`, configurable at root session creation between `0` and a hard cap of `8`.
- `allow_actor_collaboration: bool` — default `true`. Controls whether sibling/peer actors sharing the same `root_session_id` tree may message each other directly.

Depth and cycle enforcement rules:

1. **Depth Calculation**:
   - Root / externally spawned sessions have `depth = 0`.
   - Every delegated child computes `depth = parent.depth + 1` strictly along the `parent_session_id` ancestor chain.
   - Any delegation attempt whose resulting `depth > SessionPolicy.max_delegation_depth` is rejected with HTTP `409 Conflict` and error code `DELEGATION_DEPTH_EXCEEDED`.
2. **Cycle Detection**:
   - **Ancestor re-entry**: A session MUST NOT delegate to itself or re-enter an active ancestor `(role, workspace)` delegation along its `parent_session_id` chain.
   - **Cyclic wait graph**: Any wait/join operation that would introduce a cycle in the active delegation wait graph (`A` waiting on `B` while `B` transitively waits on `A`) MUST be rejected.
   - Both cycle violations fail closed with HTTP `409 Conflict` and error code `DELEGATION_CYCLE`.

### 3.2 Unified Mailbox Delivery Rules

Each session has a durable, ordered mailbox (`POST /v2/sessions/{id}/mailbox`,
`GET /v2/sessions/{id}/mailbox`, and scoped MCP `actor_mailbox_send` /
`actor_mailbox_read`). Every actor is a Session linked via `parent_session_id`
under one `root_session_id` delegation tree. Mailbox deliveries append to the
target session's event log and wake waiting turns or subscribers.

Authorization rules for scoped actor delivery (`actor_mailbox_send`):

1. **Parent <-> Direct Child**: Always allowed within the same `root_session_id` tree.
2. **Same-Tree Sibling / Peer Delivery**: Allowed to any other session sharing the same `root_session_id` delegation tree when `SessionPolicy.allow_actor_collaboration == true`, subject to the `256 KiB` inline bound (oversized bodies spill to immutable artifacts).
3. **Collaboration Disabled**: When `SessionPolicy.allow_actor_collaboration == false`, only parent <-> direct child delivery is allowed; sibling/peer delivery is rejected with HTTP `403 Forbidden` (`FORBIDDEN_SCOPE`).
4. **Cross-Tree Rejection**: Delivery to any session outside `self.root_session_id` is always rejected with HTTP `403 Forbidden` (`FORBIDDEN_SCOPE`).

## 4. Independent Daemon and Local Authentication

- `dolgorae serve` binds a Unix domain socket under `~/.dolgorae/run/dolgorae.sock` (or an explicit `--socket` path) with `0600` permissions inside a `0700` directory.
- Every incoming UDS connection is authenticated via OS peer credentials (`LOCAL_PEERCRED` / `tokio::net::UnixStream::peer_cred()`); connections whose effective UID does not match the daemon UID are rejected before request dispatch.
- Scoped Actor MCP bridges authenticate actor-scoped daemon requests with the single-actor token issued at session launch.

## 5. Cursor, Revision, and Pagination Semantics

- **Cursor**: Canonical unsigned 64-bit decimal ASCII string (`"0"`, `"1"`, `"42"`, ...), with no leading zeros (except `"0"`) and no sign prefix. Cursors are strictly numerically ordered.
- **Revision**: Each session maintains a monotonic `revision: u64` incremented on every durable state transition or timeline item append.
- **Stamp Invariant**: Every session snapshot exposes `revision`, `head_cursor`, and `interaction_revision` such that:
  - `head_cursor == decimal(revision)`
  - `interaction_revision <= revision`
- **Timeline Pagination**: Timeline responses return `highest_cursor` (the numeric decimal cursor of the newest timeline item) as a field **separate** from `next_page_token` (opaque pagination token for older/additional pages). Clients use `highest_cursor` for stream baselines and never treat `next_page_token` as a cursor.

## 6. Server-Sent Events (SSE) Contract

`GET /v2/sessions/{id}/events` streams `text/event-stream` frames:

- Every domain event sets `id: <cursor>` where `<cursor>` equals the event's canonical decimal `u64` cursor.
- Reconnection via `Last-Event-ID: <cursor>` (or query `?after=<cursor>`) is **strictly exclusive** (`event_cursor > after`) and replays from the durable SQLite event log before switching to live delivery.
- **Gap / Expiration Signaling**: If the requested `Last-Event-ID` falls behind the retained log window or cannot be replayed contiguously, the server MUST emit an explicit `event: refresh_required` frame containing a structured refresh mask (`scopes: ["session", "interaction", "timeline", "artifacts", "mailbox"]`) rather than silently skipping events.
- **Heartbeat**: Periodic `event: heartbeat` frames keep idle streams alive, carry NO `id:` line (never advancing the client's last cursor), and do not increment `revision`.

## 7. Interactions, Tolerant Reader Policy, and Optimistic Concurrency

Pending driver interactions carry structured, provider-neutral payloads inside
`InteractionKind` (discriminated by `kind`):

- `command_approval`: `{ command: [argv...], cwd?, reason? }`
- `file_change_approval`: `{ changes: [{ path, move_path?, kind, unified_diff? }], reason? }`
- `permission_request`: `{ permissions: [label...], reason? }` (human-readable labels only; native permission grant objects stay driver-side)
- `user_question`: `{ questions: [{ id, header?, question, options: [{ label, description? }], allows_other, is_secret }] }` (`id` keys `answers`)
- `mcp_elicitation`: `{ server_name, message, requested_schema? }`
- `unsupported`: `{ original_kind, reason?, payload }`

**Tolerant Reader Policy**: SPI and API deserializers ignore unrecognized fields
and are **strict on known tags, tolerant on unknown tags**. Unrecognized
`InteractionKind` or `NormalizedEvent` tags deserialize cleanly to
`unsupported` / `unknown` without failing, whereas a recognized tag with
missing or malformed required fields fails deserialization explicitly.

Persisted interaction records additionally carry:

- `interaction_id` (opaque string; native wire IDs remain driver-internal)
- `turn_id` (non-empty string)
- `session_generation` (`u64`, strictly `>= 1`)
- `interaction_revision` (`u64`)
- `session_revision` (`u64`)
- `decisions[]` (opaque `decision_id`, human `label`, semantic `kind`)
- `protected` (boolean indicating sensitive/secret input or protected approval)
- `response_schema` (optional JSON Schema)
- `max_response_bytes` (optional `usize`; `null` means the runtime default inline ceiling of `256 KiB`)

Answering an interaction (`POST /v2/sessions/{id}/interactions/{interaction_id}/answer`) requires echoing `turn_id`, `session_generation`, `expected_interaction_revision`, and `expected_session_revision`. If `turn_id` is empty, `session_generation == 0`, or persisted state no longer matches, the daemon rejects with HTTP `409 Conflict` (`error.code = "SNAPSHOT_CONFLICT"`).

## 8. Payload Size Bounds and Artifact Fallback

Size ceilings are normative contract bounds enforced in `dolgorae-core` and `dolgorae-api`:

| Bound | Limit | Behavior when exceeded |
| --- | --- | --- |
| Inline payload | `256 KiB` (`262,144` bytes) | Store body as immutable content-addressed Artifact and emit an `ArtifactRef { artifact_id, sha256, media_type, byte_len }` inline |
| Single Artifact | `64 MiB` (`67,108,864` bytes) | Reject with HTTP `413 Payload Too Large` (`PAYLOAD_TOO_LARGE`) |
| Timeline Page | `4 MiB` (`4,194,304` bytes) | Truncate page cleanly and return `next_page_token` |
| HTTP Request Body | `8 MiB` (`8,388,608` bytes) | Reject with HTTP `413 Payload Too Large` (`PAYLOAD_TOO_LARGE`) |

Artifacts are immutable, SHA-256 content-addressed, and readable in bounded byte slices via `GET /v2/artifacts/{artifact_id}`.

## 9. Output Schema and Approval Policy

- `approval_policy`:
  - `interactive`: surface driver approval/permission requests to callers.
  - `auto_deny`: decline interactive approval requests automatically.
  - `auto_approve_scoped`: permit workspace-scoped read/edit operations allowed by `access` while still refusing or escalating out-of-scope requests.
- `output_schema`:
  - Passed natively when the selected driver advertises `native_output_schema == true`.
  - When `native_output_schema == false` (for example, `agy-acp`), Dolgorae appends schema instructions to the role/turn prompt and validates the final assistant output in `dolgorae-core`, recording validation status truthfully rather than claiming native enforcement.

## 10. Canonical Error Codes

All HTTP/SSE endpoints (`docs/protocol/openapi-v2.yaml`) and MCP tools
(`docs/protocol/mcp-v2.md`) share one canonical error code table:

| Error Code | HTTP Status | Meaning |
| --- | --- | --- |
| `BAD_REQUEST` | `400` | Malformed request body, invalid cursor, empty `turn_id`, or `session_generation == 0` |
| `UNAUTHORIZED` | `401` | Peer credential mismatch or missing/invalid `DOLGORAE_ACTOR_TOKEN` |
| `FORBIDDEN_SCOPE` | `403` | Privilege escalation, workspace escape, cross-tree access, or disallowed sibling mailbox delivery |
| `NOT_FOUND` | `404` | Unknown role, session, turn, interaction, or artifact |
| `SNAPSHOT_CONFLICT` | `409` | Optimistic concurrency mismatch on `turn_id`, `session_generation`, or revision stamps |
| `DELEGATION_DEPTH_EXCEEDED` | `409` | Delegated child `depth = parent.depth + 1` exceeds `SessionPolicy.max_delegation_depth` |
| `DELEGATION_CYCLE` | `409` | Ancestor `(role, workspace)` delegation re-entry or cyclic delegation wait graph |
| `PAYLOAD_TOO_LARGE` | `413` | HTTP body `> 8 MiB` or artifact `> 64 MiB` |
| `DRIVER_UNAVAILABLE` | `503` | Selected Tier-1 driver binary missing or failed capability probe |
| `INTERNAL_ERROR` | `500` | Unexpected local daemon or storage error |
