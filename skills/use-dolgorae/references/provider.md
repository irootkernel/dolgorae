# v0.1.3 provider workflow

Use this workflow only after the user authorizes the exact live operation. Keep
gateway lifecycle, Profile lifecycle, Controller credential creation, model
execution, cancellation, and whole-session closure as distinct mutations.

## Establish the public boundary

1. Resolve one exact Dolgorae executable and verify `version --json` against
   `runtime capabilities`.
2. Inspect the workspace and selected global Profiles. Run `profile doctor`
   before live use. Do not start or repair a Profile Server without authority.
3. Validate and install the Specialist Policy only when requested. The policy
   must admit the intended approval mode, Role, access, Profile, lane, and
   reuse behavior. v0.1.3 rejects `reuse_any_compatible`, collaboration-enabled
   Roles, and automatic on-mail activation before allocating a Run.
4. Create a generation-1 interactive-client Controller carrier with the exact
   `--orchestration-policy`. Keep the bearer bytes in its owner-only file and
   pass only the checked public carrier reference to gRPC.
5. Start `dolgorae serve` on an absolute private Unix socket. Retain its
   readiness envelope and process identity. Call protocol-zero
   `GetCapabilities`; require protocol 1, the expected descriptor SHA-256, and
   all 27 required methods before creating a Session.

## Drive the Session

Start one parentless direct-interactive Run with the protected Controller,
selected Profile, shared-readonly or dedicated lane, explicit assurance, and
an operation-scoped idempotency key. The matching Specialist Policy snapshot is
captured during `StartRun`; changing or removing the registry entry later does
not rewrite the Session.

Submit sequential human input with a fresh expected Run revision. Reuse a
`SubmitTurn` key only for byte-equivalent input. A transport timeout does not
prove rejection. Refresh `GetRun`, inspect events, and reconcile an unknown
outcome before issuing conflicting input. Fresh input while a Turn is active is
rejected; v0.1.3 has no steering queue.

For `user_approval_required`, list the root Run's pending interactions, fetch
the full interaction with the root Controller, and resolve the exact
`specialist_approval` answer through `ResolveInteraction`. For
`fully_delegated`, automatic approval applies only when the immutable Role
permits it. In either mode, a rejected or unresolved request does not allocate
a usable member.

The Primary may request a Specialist, wait for its operation, list ready
members, assign one task, await or collect the result, and read the immutable
result in pages. Retire members through whole-session `CloseRun`; direct
`release_specialist` through the live Primary tool returns
`ORCHESTRATION_NOT_AVAILABLE`. A wait timeout leaves accepted work running.
Exact accepted-task replay returns the original receipt; a fresh assignment to
a busy member fails before effects. Never provide the Primary with a child
Controller, private socket, database path, host artifact path, or hidden
identifier.

## Observe results and reconnect

Use the root Controller with `GetOrchestratedSession` for lifecycle, policy,
member, task, result, and close counts. Use
`ListOrchestratedSessionResults` for captured-head result pages. Discover each
Primary-owned artifact reference from that query before calling `GetArtifact`
or `ReadArtifactChunk`. Concatenate bounded chunks, verify the exact byte
length and SHA-256, and reject authorization, range, integrity, malformed
cursor, or unsupported-version errors by their typed code.

Resume `WatchRunEvents` from the last durable cursor after a disconnect or
gateway restart. A stream ending with `SERVER_SHUTDOWN` is not a Run failure.
Refresh snapshots before tokenless mutations and follow the method-specific
retry action; never parse status prose to decide whether to repeat work.

Close the root Run only with explicit authority. `CloseRun` records one durable
whole-session operation, stops new admission, and retires owned Specialists.
`SESSION_CLOSE_IN_PROGRESS` means observe or recover that operation, not submit
another close intent. Unknown owned effects prevent successful closure. A
gateway restart preserves the Session, history, results, workers, and close
identity.

Provider completion proves `MILESTONE-BH1-P` only. Actual Gul integration,
stable release, tag, publication, installation, and Personal Alpha acceptance
have separate owners and evidence.
