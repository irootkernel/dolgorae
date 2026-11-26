# Dolgorae v2 MCP Protocol Contract

`dolgorae mcp` exposes a stdio JSON-RPC 2.0 Model Context Protocol server in two
distinct security modes backed by the local `dolgorae serve` Unix socket daemon.

## 1. External Host Mode (`dolgorae mcp`)

Used by external editors, CLI hosts, or orchestrators running under the user's
local account. Authenticated to `dolgorae serve` via same-user `LOCAL_PEERCRED`.

### Exposed Tools

| Tool | Required Arguments | Description |
| --- | --- | --- |
| `dolgorae_capabilities` | _(none)_ | Probe registered drivers, models, and normative size bounds. |
| `dolgorae_role_list` | `workspace?` | List resolved Role / Persona definitions. |
| `dolgorae_session_start` | `workspace`, `driver`, `role?`, `model?`, `effort?`, `access?`, `approval_policy?`, `output_schema?`, `allow_delegation?`, `policy?` | Start a root session (`depth = 0`) with optional `SessionPolicy { max_delegation_depth (default 3, max 8), allow_actor_collaboration (default true) }`. |
| `dolgorae_session_get` | `session_id` | Return current session snapshot including `depth`, `policy`, `revision`, `head_cursor`, and `interaction_revision`. |
| `dolgorae_session_close` | `session_id` | Gracefully close a session and terminate its driver. |
| `dolgorae_turn_submit` | `session_id`, `text`, `effort?`, `attachments?`, `output_schema_override?` | Submit a turn to an active session. |
| `dolgorae_turn_interrupt` | `session_id`, `turn_id` | Request interruption of an in-flight turn. |
| `dolgorae_timeline_read` | `session_id`, `page_token?`, `limit?` | Read bounded timeline items (`highest_cursor` distinct from `next_page_token`). |
| `dolgorae_interaction_answer` | `session_id`, `interaction_id`, `turn_id`, `session_generation`, `expected_interaction_revision`, `expected_session_revision`, `decision_id`, `answers?`, `payload?` | Resolve a pending interaction with optimistic concurrency checks (`SNAPSHOT_CONFLICT` / HTTP `409` on mismatch). |
| `dolgorae_mailbox_send` | `session_id`, `body`, `subject?`, `artifact_ids?` | Post a durable message to a session's mailbox. |
| `dolgorae_mailbox_read` | `session_id`, `after?`, `limit?` | Read durable mailbox entries strictly after cursor `after`. |
| `dolgorae_artifact_read` | `artifact_id`, `offset?`, `max_bytes?` | Read a bounded slice of an immutable artifact. |

## 2. Scoped Actor Mode (`dolgorae mcp --scoped`)

Injected automatically into child/delegating driver sessions via
`McpServerInjection` with `DOLGORAE_ACTOR_TOKEN` in the process environment.

### Security and Delegation Invariants

1. **Mandatory Actor Token**: `dolgorae mcp --scoped` fails closed at startup if `DOLGORAE_ACTOR_TOKEN` is missing or empty, and sends the token as a bearer credential on every UDS request.
2. **Delegation Depth Limit (`DELEGATION_DEPTH_EXCEEDED`, HTTP `409`)**:
   - Root sessions start at `depth = 0`.
   - Every delegated child computes `depth = parent.depth + 1` along the `parent_session_id` chain.
   - `actor_delegate` is rejected with `DELEGATION_DEPTH_EXCEEDED` whenever `self.depth + 1 > SessionPolicy.max_delegation_depth` (default `3`, hard cap `8`).
3. **Cycle Detection (`DELEGATION_CYCLE`, HTTP `409`)**:
   - Rejects direct or transitive ancestor re-entry (same actor or same active `(role, workspace)` in the ancestor delegation chain).
   - Rejects cyclic wait edges (`A` waiting on `B` while `B` transitively waits on `A`).
4. **No Privilege Escalation (`FORBIDDEN_SCOPE`, HTTP `403`)**: A scoped actor cannot:
   - Spawn a child with `workspace_write` if the actor itself is `read_only`.
   - Escape its bound workspace root.
   - Read, cancel, or message sessions outside `self.root_session_id`.
   - Answer its own pending approvals or modify daemon-level configuration.
5. **Unified Mailbox Delivery Policy**:
   - **Parent <-> Direct Child**: Always allowed within the same `root_session_id` delegation tree.
   - **Same-Tree Sibling / Peer**: Allowed to any other session sharing `self.root_session_id` when `SessionPolicy.allow_actor_collaboration == true`, subject to the `256 KiB` inline cap (larger bodies spill to artifacts).
   - **Collaboration Disabled**: When `SessionPolicy.allow_actor_collaboration == false`, sibling/peer delivery is rejected with `FORBIDDEN_SCOPE` (HTTP `403`).
   - **Cross-Tree**: Always rejected with `FORBIDDEN_SCOPE` (HTTP `403`).
6. **Opaque Boundaries**: Native driver request IDs, native permission grant maps, and other sessions' actor tokens are never exposed in tool outputs.
7. **Bounded Outputs**: Inline tool responses respect the `256 KiB` inline cap and return `ArtifactRef` handles for larger bodies.

### Exposed Scoped Tools

| Tool | Required Arguments | Scope & Error Rule |
| --- | --- | --- |
| `actor_delegate` | `role?`, `driver?`, `model?`, `effort?`, `access?`, `prompt`, `output_schema?` | Spawns child with `parent_session_id = self`, `depth = self.depth + 1`, `access <= self.access`. Rejects with `DELEGATION_DEPTH_EXCEEDED`, `DELEGATION_CYCLE`, or `FORBIDDEN_SCOPE`. |
| `actor_child_status` | `child_session_id` | Allowed only when `child.parent_session_id == self` (else `FORBIDDEN_SCOPE`). |
| `actor_child_cancel` | `child_session_id` | Interrupts/closes only direct children of `self` (else `FORBIDDEN_SCOPE`). |
| `actor_mailbox_send` | `target_session_id`, `body`, `subject?` | Always allowed for parent <-> direct child; allowed for same-tree siblings/peers iff `SessionPolicy.allow_actor_collaboration == true`; cross-tree or disallowed peer rejected with `FORBIDDEN_SCOPE`. |
| `actor_mailbox_read` | `after?`, `limit?` | Reads only `self`'s own durable mailbox. |
| `actor_artifact_read` | `artifact_id`, `offset?`, `max_bytes?` | Reads only artifacts accessible to `self.root_session_id` (else `FORBIDDEN_SCOPE`). |

## 3. Canonical Error Code Mapping

MCP tool errors mirror the HTTP status and `error.code` contract from
`docs/specs/README.md` and `docs/protocol/openapi-v2.yaml` verbatim:

| Error Code | HTTP Status | Meaning |
| --- | --- | --- |
| `BAD_REQUEST` | `400` | Invalid arguments, empty `turn_id`, or `session_generation == 0` |
| `UNAUTHORIZED` | `401` | Peer credential mismatch or invalid `DOLGORAE_ACTOR_TOKEN` |
| `FORBIDDEN_SCOPE` | `403` | Privilege escalation, cross-tree access, or disallowed sibling/peer delivery |
| `NOT_FOUND` | `404` | Unknown role, session, turn, interaction, or artifact |
| `SNAPSHOT_CONFLICT` | `409` | Optimistic concurrency revision or generation mismatch |
| `DELEGATION_DEPTH_EXCEEDED` | `409` | `parent.depth + 1 > SessionPolicy.max_delegation_depth` |
| `DELEGATION_CYCLE` | `409` | Ancestor delegation re-entry or cyclic wait graph |
| `PAYLOAD_TOO_LARGE` | `413` | Request body `> 8 MiB` or artifact `> 64 MiB` |
| `DRIVER_UNAVAILABLE` | `503` | Selected driver unavailable |
| `INTERNAL_ERROR` | `500` | Unexpected daemon error |
