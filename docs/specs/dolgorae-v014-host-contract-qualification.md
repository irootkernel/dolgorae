# v0.1.4 native host contract qualification

Status: Both pinned actual hosts passed the isolated prototype scenarios below.
Independent Task review accepted bounded feasibility. The
[H1 package](dolgorae-v014-h1-package.md) binds these inputs and assurance limits;
the roadmap owns checked publication and handoff status.
The source selection and responsibility split are accepted. This document
defines the candidate boundary and reproduction;
it is not a production facade or final consumer runtime acceptance record.

The [successor requirements](dolgorae-v014-successor-requirements.md),
[method and digest contract](dolgorae-v014-method-and-digest-contract.md) and
[ADR-041](../architecture-decision-records/README.md#adr-041-qualify-a-protected-native-host-hook-binding-for-external-specialist-mcp)
own the selected behavior. The [roadmap](../roadmap/README.md) owns acceptance.
The historical Codex 0.149.0 SR1 negative disposition is unchanged.

## Selected prototype and host pins

| Component | Selected identity |
| --- | --- |
| Codex native host | Codex CLI 0.160.0; executable SHA-256 `112fae7a5a1223e673c8a1791d32338f37df8b527ff1159bb8adac6c4dbf1b4b` |
| Claude native host | Claude Code 2.1.295; executable SHA-256 `0116ee2e0a513900b633d9951367f18747686478e2b462805b8c31609f047f70` |
| Host hook, identity store, MCP backend and trusted lookup | [Rust prototype](../../examples/host_invocation_contract.rs), built with locked dependencies and Rust 1.97.1 |
| Native API driver and physical host-crash campaign | [Account-free fixture](../../tests/e2e/run_native_host_invocation_fixture.py) |
| Native-frame transport fault injection | [Stdio fault proxy](../../tests/e2e/host_invocation_fault_proxy.py) |
| Execution platform | Isolated macOS ARM environment; no provider account or production state |

The selected prototype bytes are pinned independently of the host pins:

| Prototype input/artifact | Raw-byte SHA-256 |
| --- | --- |
| `examples/host_invocation_contract.rs` | `dce7544eaa6249b831799342473b8c2544169a887516995f8d2b25072437fc5f` |
| `tests/e2e/run_native_host_invocation_fixture.py` | `553788b5d6a1f4432f36ce49f460094e4a5529d862df4a71d887f35ea8054b41` |
| `tests/e2e/host_invocation_fault_proxy.py` | `e0d15d5ee8e6eec27bd689f2988a604ae056e38cb0728d1fe524864f8fd03371` |
| Tested macOS ARM debug prototype executable | `c102900c494017c611a70ef248b8c7f99ef7154bcbbea743a92bfc9b109bffc3` |

The isolated campaign used the same executable and driver/proxy bytes as this
candidate. The debug executable identifies this feasibility experiment only;
H3 must separately pin the production adapter and intended-version artifact.
Changing these sources requires affected requalification and a new binding.

The host checksums are admission pins, not stable-release publication claims.
Source changes invalidate their affected scenario results. Final H1 must bind
the reviewed prototype sources and tested executable, alongside the contract
package. H3 supplies the later production adapter and consumer artifact pins.
Aquarium's separate native Codex 0.162.0/0.162.1 and Claude SDK 0.3.295 evidence
retains its original scope; it does not replace this qualification.

## Identity and state boundary

The hook receives a native session/tool-use identity. Before allowing dispatch,
it commits a UUIDv7, exact public-input digest and native-event binding in a
current-user-owned protected SQLite store. Repeated native identity and equal
input reuse that binding. Changed input conflicts. Equal text from a distinct
native invocation creates a distinct UUID. A different Turn cannot silently
rebind an existing native session/call pair.

Codex resolves the precommitted binding from native MCP `_meta.threadId` and
`_meta.callId`, leaving public arguments unchanged. Claude requires native
`mcp_server.name=dolgorae` and `mcp_server.source=dynamic`; its hook injects an
opaque private binding that the backend validates and removes. The model schema
does not contain this field and model-supplied bindings are refused.

The prototype commits acceptance and its receipt in one SQLite transaction
before acknowledging the native host. Its only effect is this test acceptance;
it does not dispatch a Specialist or implement production facade operations.
The fault proxy uses actual native request frames and actual Rust backend
processes. It never supplies a fabricated positive native invocation.

Initialization is explicit. Normal delivery and lookup cannot create missing
state. State roots are canonical, current-user-owned 0700 directories; database
files are current-user-owned 0600 regular single-link files. Initialization uses
exclusive no-follow creation. Corruption, wrong route, missing state or unknown
lookup identity refuses without replacement. The prototype cannot certify
authoritative absence without a retained namespace-generation fence.

Supported trusted-owner prototype commands are:

```text
host_invocation_contract init ROOT ROUTE
host_invocation_contract host-hook ROOT ROUTE
host_invocation_contract host-mcp ROOT ROUTE
host_invocation_contract lookup ROOT ROUTE EXTERNAL_REQUEST_REF
host_invocation_contract host-lookup ROOT ROUTE NATIVE_SESSION NATIVE_CALL
```

`ROOT` is an explicitly selected private absolute directory, not a discovered
production home. `ROUTE` is exactly `codex-0.160.0` or `claude-code-2.1.295`.
`host-lookup` recovers the original acceptance from protected native event
identity without reconstructing arguments, attachments or a lost receipt UUID.
Recovery uses this supported getter; direct database reads in the fixture are
test oracles only. Production Controller authorization and generation-scoped
lookup remain required TASK-070/077 implementations.

Claude stores hook stdout in its native conversation journal. That journal is
explicit protected identity state, under the isolated host configuration root
with umask 077 and current-user 0700/0600 ancestors/files. It cannot be exported
as consumer or review evidence. Codex's native runtime output echoes rewritten
arguments, which is why its selected route uses metadata instead. Both routes
must keep the binding out of model API requests, ordinary diagnostic output,
tool results and exported receipts. Account credentials are never used.

## Required observations and their tested layers

The following observations passed on both pinned hosts. Final independent
acceptance must verify their exact candidate and assurance limits.

| Scenario | Observable criterion | Tested layer |
| --- | --- | --- |
| Initial call | Real host advertises the tool, invokes the native hook and receives the committed receipt | Native host through stdio proxy to Rust backend |
| Exact replay | Original frame returns the identical receipt and UUID | Native-produced frame through a fresh actual backend process |
| Input conflict | Changed public input under the retained identity is refused without a new acceptance | Same backend path |
| Concurrent replay | Eight actual backend processes return the same receipt and one retained identity | Shared protected store and native-produced frame |
| Distinct equal-text calls | Two deliberate native calls with equal public payload have distinct UUIDs | Actual native host dispatch |
| Reconnect and MCP replacement | Fresh backend processes retain receipts; resumed exact native session starts a fresh outer stdio process | Backend replacement plus actual native host resume |
| Loss before acceptance | Prepared identity remains unresolved; lookup cannot claim acceptance or mint a replacement | Actual backend termination before request delivery |
| Loss after acceptance before acknowledgement | Discarded backend reply resolves to its committed original receipt | Actual backend acceptance and supported lookup |
| Loss after receipt commit | Repeated lost reply and exact replay preserve the original receipt | Same atomic commit boundary; no invented intermediate commit |
| Physical host loss before acknowledgement | Real native host and its MCP process group are killed while the committed reply is withheld; trusted native-key lookup returns the original receipt before exact-session resume | Actual native processes, transport fault injection and supported getter |
| Recovery after physical loss | Old receipt remains identical after resume; a separately requested new native call receives a fresh UUID | Actual resumed native host and fresh outer stdio process |
| Corrupted or missing state | Hook, delivery and lookup refuse; state is not recreated and no replacement UUID is minted | Actual prototype executables against isolated failure fixtures |
| Private binding exposure | Binding absent from actual model requests and native stdout/stderr; any Claude journal retention remains inside protected native state | Actual host API traffic, bounded local output and state checks |

The physical-loss campaign recovers by explicit trusted lookup. It does not
claim native automatic replay of an interrupted pending call. Backend loss tests
and actual native host loss have separate observations. Neither establishes
Phase 3 Specialist dispatch, complete production results, Broker operation,
Controller interactions or consumer acceptance.

## Bounded reproduction

Native runtime execution requires explicit authorization and opt-in. The target
is excluded from the default repository gate. Create an isolated private scratch
directory using the repository's temporary-file rules, then set:

```sh
export DOLGORAE_RUN_NATIVE_HOST_INVOCATION_FIXTURE=1
export DOLGORAE_HOST_FIXTURE_SCRATCH_ROOT="$PRIVATE_SCRATCH"
export DOLGORAE_CODEX_BIN="$PINNED_CODEX"
export DOLGORAE_CLAUDE_BIN="$PINNED_CLAUDE"
make PYTHON_BIN=.venv/bin/python test-native-host-invocation-fixture
```

On a separate isolated machine, transfer only the built prototype and the two
fixture scripts into caller-owned scratch, and invoke the driver with explicit
`--prototype`, `--codex`, `--claude` and `--scratch-root` arguments. No global
installation or production tool replacement is needed.

The driver checks both executable versions and complete SHA-256 values before
launch. It creates fresh homes and host configuration roots, inherits no account,
provider or proxy credentials, and uses only numeric loopback scripted Responses
and Messages endpoints. The Anthropic key is a non-credential fixture marker.
Host-generated calls and results are observed; scripted APIs never supply the
tool result. Each normal or physical-crash case has a 90-second deadline and
eight-POST ceiling; the observed two cases total eight API POSTs per host.
Oversized HTTP/MCP input, unexpected endpoints, missing receipts, changed native
session identity and secret exposure fail the campaign. Owned process groups
are terminated before the case ends. Raw native history/output stays private;
only bounded structured observations may be considered for approved promotion.

The fixture intentionally emits `full_matrix_qualified=false` until the owning
workflow accepts the complete reviewed qualification and external decisions.
Passing observed scenarios alone cannot publish H1 or mark TASK-067 complete.
