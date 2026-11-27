# Dolgorae v2 Tier-1 Driver Protocol Mappings

This document records the verified local CLI wire protocols and behavioral
quirks for the three Tier-1 structured drivers implemented in `crates/drivers/`.
Every driver implements `dolgorae_spi::AgentDriver` and `DriverSession`, owns
all native ID and permission-grant mappings privately, and exposes only opaque
`String` IDs and `NormalizedEvent` values across `dolgorae-spi`.

## 1. `driver-codex-app` — Codex (`codex-cli 0.172.0`)

- **Invocation**: `codex app-server --stdio`
- **Framing gotcha**: Codex omits `"jsonrpc": "2.0"` on stdout responses and notifications (e.g., `{"id":1,"result":{...}}` and `{"method":"configWarning","params":...,"emittedAtMs":...}`). Parsers MUST NOT require a `"jsonrpc"` field.
- **Handshake**:
  1. Request `initialize`: `{"id":1,"method":"initialize","params":{"clientInfo":{"name":"dolgorae","version":"0.2.0"},"capabilities":{"experimentalApi":true}}}` -> `{userAgent, codexHome, platformFamily, platformOs}`.
  2. Notification `initialized`: `{"method":"initialized"}`.
- **Session Lifecycle**:
  - `thread/start`: optional `model`, `cwd`, `sandbox` (`"read-only" | "workspace-write" | "danger-full-access"`), `approvalPolicy` (`"untrusted" | "on-request" | "never"` or `{"granular": {...}}`), `developerInstructions`, `baseInstructions`, `config`, `ephemeral`, `runtimeWorkspaceRoots`. Note: response `sandbox` uses camelCase `SandboxPolicy` (`{type: "readOnly" | "workspaceWrite" | "dangerFullAccess", ...}`).
  - `thread/resume`: required `threadId`; optional `model`, `cwd`, `sandbox`, `approvalPolicy`, `developerInstructions`, `baseInstructions`, `config`, `excludeTurns`.
- **Turn Execution**:
  - `turn/start`: required `threadId`, `input: [{"type":"text","text":"..."}]`; optional `effort` (`"low" | "medium" | "high" | "xhigh" | "ultra"`), `outputSchema` (native JSON Schema enforcement -> `native_output_schema = true`), `model`, `cwd`, `approvalPolicy`, `sandboxPolicy`.
  - `turn/interrupt`: `{threadId, turnId}`.
- **Streaming Notifications**:
  - `turn/started`, `turn/completed`, `item/started`, `item/completed`, `item/agentMessage/delta`, `item/commandExecution/outputDelta`, `item/fileChange/outputDelta`, `item/fileChange/patchUpdated`, `item/reasoning/textDelta`, `item/reasoning/summaryTextDelta`, `item/plan/delta`, `item/mcpToolCall/progress`, `thread/tokenUsage/updated`, `thread/compacted`, `error` (`{error, threadId, turnId, willRetry}` -> `NormalizedEvent::Error`), `serverRequest/resolved`, `configWarning`. Process termination emits `NormalizedEvent::SessionClosed`.
  - Tool notifications (`commandExecution`, `fileChange`, `mcpToolCall`) map into `NormalizedEvent::ToolActivity { command?, exit_status?, paths, ... }`.
- **Reverse ServerRequests** (native integer/string JSON-RPC `id` mapped to opaque `interaction_id: String` inside the driver; reply `{"id": req_id, "result": {...}}`):
  - `item/commandExecution/requestApproval` -> `InteractionKind::CommandApproval { command: Vec<String>, cwd, reason }` -> `{decision: "accept" | "acceptForSession" | "decline" | "cancel"}`
  - `item/fileChange/requestApproval` -> `InteractionKind::FileChangeApproval { changes: [FileChangeEntry], reason }` -> `{decision: "accept" | "acceptForSession" | "decline" | "cancel"}`
  - `item/permissions/requestApproval` -> `InteractionKind::PermissionRequest { permissions, reason }` (human-readable permission labels only; native `GrantedPermissionProfile` kept inside driver map!) -> `{permissions, scope: "turn" | "session"}`
  - `item/tool/requestUserInput` -> `InteractionKind::UserQuestion { questions: [UserQuestionItem] }` (`isSecret` sets `is_secret = true` and `InteractionRequest.protected = true`) -> `{answers: {<q_id>: {answers: [...]}}}`
  - `mcpServer/elicitation/request` -> `InteractionKind::McpElicitation { server_name, message, requested_schema }` -> `{action: "accept" | "decline" | "cancel", content?}`
- **Stdio MCP Injection (Zero Secrets in `argv`)**:
  - Actor tokens and MCP server secrets MUST NEVER be passed via CLI flags (`-c`). `driver-codex-app` sends `mcp_servers` exclusively over stdin JSON-RPC inside `thread/start` / `thread/resume` `config.mcp_servers`.

## 2. `driver-claude-wire` — Claude Code (`2.1.332`)

- **Invocation**:
  ```text
  claude -p --verbose \
    --input-format stream-json \
    --output-format stream-json \
    --include-partial-messages \
    --replay-user-messages \
    --permission-prompts host \
    --permission-prompt-tool stdio \
    --permission-mode manual \
    --strict-mcp-config --mcp-config '<0600-file-in-0700-dir>'
  ```
- **Verified Gotchas**:
  1. `--verbose` is **mandatory** whenever `-p --output-format stream-json` is used (omitting it exits with code 1).
  2. Always pass `--strict-mcp-config --mcp-config <path>` pointing to a `0600` config file inside a `0700` private temporary directory (removed on `close()` and `Drop`) so Claude neither loads unrelated user-global MCP servers nor exposes `DOLGORAE_ACTOR_TOKEN` in process `argv` (`ps`). Inline JSON secrets in `argv` are **FORBIDDEN**. Because `--mcp-config` is variadic (`<configs...>`), never place a positional prompt directly after it.
  3. Do NOT pass `--bare` for subscription/OAuth users (it disables OAuth/keychain and forces `ANTHROPIC_API_KEY`).
  4. Native `--json-schema <schema>` is supported (`native_output_schema = true`), alongside `--append-system-prompt`, `--system-prompt`, `--session-id <uuid>` (for fresh sessions so `provider_session_id` is stable before the first turn), `--resume <session_id>`, `--effort <low|medium|high|xhigh|max>`, `--model <alias|id>`, and `--permission-mode <acceptEdits|auto|bypassPermissions|manual|dontAsk|plan>`.
- **Wire NDJSON**:
  - **Stdin user turn**: `{"type":"user","message":{"role":"user","content":"..."},"parent_tool_use_id":null,"session_id":"..."}`
  - **Stdin control requests**: `{"type":"control_request","request_id":"<id>","request":{"subtype":"initialize"|"interrupt"|"set_permission_mode"|"set_model"|"mcp_status"|...}}`
  - **Stdout stream**: `system` (`subtype: "init"`, ignore unknown hook subtypes), `stream_event` (Anthropic raw message deltas), `assistant`, `user` (replayed/tool results), `result` (`subtype: "success" | "error_*"`, `structured_output?`, `usage`, `total_cost_usd`, `session_id`), `keep_alive`.
  - **Reverse `control_request` (`can_use_tool`)**: normalized to `CommandApproval`/`FileChangeApproval`/`PermissionRequest`; can be withdrawn by `control_cancel_request`.
  - **Reply `control_response`**:
    - Allow: `{"type":"control_response","response":{"subtype":"success","request_id":"<id>","response":{"behavior":"allow","updatedInput":<object>}}}` (**Gotcha**: `updatedInput` is required on allow; echo original `input` when unmodified).
    - Deny: `{"type":"control_response","response":{"subtype":"success","request_id":"<id>","response":{"behavior":"deny","message":"<reason>","interrupt":false}}}`.

## 3. `driver-agy-acp` — Agy (`1.5.18`)

- **Invocation**: `agy acp --stdio --process-per-session`
- **Framing**: Standard JSON-RPC 2.0 NDJSON.
- **Handshake & Session Lifecycle**:
  - `initialize` (`{protocolVersion: 1, clientCapabilities: {...}, clientInfo: {name, version}}`) -> `protocolVersion: 1`, `agentInfo`, `agentCapabilities` (`loadSession`, `sessionCapabilities: {resume, close, load, list, additionalDirectories}`, `_meta.antigravity: {tier: true, systemPrompt: {append: true}}`).
  - `session/new`: `{cwd: "<abs>", mcpServers: [...], _meta?: {"antigravity": {"systemPrompt": {"append": "..."}}}}` -> `{sessionId, models, modes, configOptions: [model, thought_level, mode]}`. Observed `agy 1.5.18` `modes` (`currentModeId: "default"`, `availableModes`):
    - `default`: "Ask before edits and commands, per your permission settings."
    - `accept-edits`: "Approve file edits in the workspace without asking."
    - `auto`: "Approve edits and safe commands; ask before irreversible actions."
    - `plan`: "Plan carefully before executing a task."
    Under `AccessMode::ReadOnly`, `driver-agy-acp` calls `session/set_mode` to select a safe advertised mode in preference order `["read-only", "read_only", "ask", "default"]` (selecting `"default"` on `agy 1.5.18`, never `"plan"`, `"auto"`, or `"accept-edits"`) and fails closed with `DriverError::Unsupported` if none is advertised. Residual risk: user-level Agy permission settings ("per your permission settings") may pre-allow edits before ACP prompts; any edit permission request that reaches `session/request_permission` is hard-denied by `evaluate_interaction`.
  - `session/set_config_option`, `session/set_mode`, `session/set_model`, `session/resume`, `session/close`.
- **Turn, Cancellation, and Permissions (ACP v1)**:
  - `session/prompt`: `{sessionId, prompt: [{type: "text", text: "..."}]}` -> `{stopReason, usage?}`
  - `session/cancel`: notification `{sessionId}`; client MUST answer any pending `session/request_permission` with `{"outcome": {"outcome": "cancelled"}}`.
  - Reverse `session/request_permission`: options carry `optionId` and kinds `allow_once | allow_always | reject_once | reject_always`; reply `{"outcome": {"outcome": "selected", "optionId": "..."}}` or `{"outcome": {"outcome": "cancelled"}}`.
  - Streaming notifications: `session/update`.
- **Verified Gotchas**:
  1. **Stdio MCP `env` shape**: `mcpCapabilities` (`http`/`sse`) is not advertised; pass stdio entries in `session/new` `mcpServers` where `env` is an **array** of `{"name": "...", "value": "..."}` objects (NOT a JSON object map!).
  2. **Interleaved notifications**: `session/new` emits interleaved `session/update` notifications (`available_commands_update`, `usage_update`) before/alongside the JSON-RPC response; the client dispatcher must route notifications concurrently while awaiting request responses.
  3. **No native `outputSchema`**: ACP v1 has no `outputSchema` field on `session/prompt`. `driver-agy-acp` advertises `native_output_schema = false`, injects schema guidance via `_meta.antigravity.systemPrompt.append`, and `dolgorae-core` validates the final output.
