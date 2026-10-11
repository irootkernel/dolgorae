# Dolgorae H1 contract package

Scope: Checked successor contracts. The roadmap records Task and whole-Epic
acceptance and EPIC-018 handoff status. Capability negotiation governs runtime
admission.

The [immutable H1 lock](../protocol/dolgorae-gul-consumer-v2.lock.json) binds
the public-v2 source and descriptor, Gul consumer-v2 profile and Go module,
Machine/facade-v5 envelopes, identity and persisted-state schemas, policies,
fixtures, accepted consumer mappings and native feasibility inputs. Identify
the package by the lock's raw SHA-256 and its containing completion commit.
The lock excludes itself from its artifact list to avoid a recursive hash.

The [successor requirements](dolgorae-v014-successor-requirements.md),
[method and digest contract](dolgorae-v014-method-and-digest-contract.md),
[consumer acceptance specification](dolgorae-consumer-acceptance-v014.md) and
[accepted coordination decision](dolgorae-v014-contract-coordination.md)
own the requirements. The [roadmap](../roadmap/README.md) owns delivery status.

## Checked artifacts

| Surface | Checked owner | Consumer rule |
| --- | --- | --- |
| Public-v2 | [Proto source](../protocol/dolgorae/public/v2/dolgorae.proto) and [descriptor manifest](../protocol/dolgorae-public-v2.descriptor.json) | Separate `dolgorae.public.v2`; 64 methods, typed modern startup, operation lookup, interactions, task/group/results and maintenance |
| Gul consumer-v2 | [Named method profile](../protocol/dolgorae-gul-consumer-v2.json), generated Go module and [wire fixtures](../protocol/dolgorae-gul-consumer-v2.fixtures.json) | Negotiate the applicable subset per feature; immutable requested creation mode remains separate from effective access and verification |
| Method behavior | [Mutation policy](../protocol/dolgorae-rpc-mutation-policy-v2.json), [error/effect policy](../protocol/dolgorae-grpc-error-mapping-v2.json), [client policy](../protocol/dolgorae-grpc-client-policy-v2.json) and [capability policy](../protocol/dolgorae-capabilities-v2.json) | Every descriptor method has an effect, material failure and recovery rule; mutation retries and hedging remain disabled |
| Identity | [Closed identity schema](../protocol/dolgorae-v014-identities-v1.schema.json) and [digest vectors](../protocol/dolgorae-v014-digest-vectors-v1.json) | Eight domain-separated digests; trusted invocation authority stays outside model-visible arguments |
| Machine and facade | [Machine-v5](../protocol/dolgorae-machine-v5.schema.json) and [facade-v5](../protocol/dolgorae-external-specialist-facade-v5.schema.json) | Preserve accepted v4 meaning; unavailable capabilities cannot be inferred from schemas |
| Persistence | [Successor state](../protocol/dolgorae-successor-state-v1.schema.json) | Select each changed generation explicitly; old records retain their own supported readers and recovery |
| Acceptance | [Fixture inventory](../protocol/dolgorae-v014-acceptance-fixtures-v1.json) and three-consumer specification | Cover ID/OP/LIFE/OBS/INT/DELEG/FIND/SCHED/STORE/REL, all 45 feedback rows and Gul G4-1..7; later actual runtime cases remain mandatory |

The frozen public-v1 source, descriptor, policies, fixtures and generated clients
retain their exact lock bytes. Machine/facade-v4 and workspace-execution/v1
retain their accepted meanings. New contracts do not register public-v2 with
the running gateway or activate the dormant Aquarium review backend.

The selected persisted generations are manifest 4, configuration 4,
instructions 3, Run state 2 and intent 2. Audit 2, membership 2, workspace 1
and Profile-server state 2 retain their existing readers and meaning. New
sidecars carry group/merge, pool/queue, lifecycle and maintenance state.
Changed generations require explicit compatible readers and supported
recovery; publication does not authorize an in-place rewrite or downgrade.

| Meaning | JSON identity/persisted representation | Protobuf representation |
| --- | --- | --- |
| Immutable read-only creation | `creation_mode=read_only`; configuration `requested_access=read_only`; manifest `requested_access=read-only` | `CREATION_MODE_READ_ONLY` |
| Immutable writable creation | `creation_mode=writable`; configuration `requested_access=canonical_workspace_write`; manifest `requested_access=writable` | `CREATION_MODE_WRITABLE` |

These spellings map the same accepted choice across versioned carriers.
They do not change the separately observed effective access, verification or
fixed read-only pool restriction.

Keyed mutations retain the original principal, Controller generation, scope,
operation kind and key. Authorized lookup returns exactly `accepted`,
`not_accepted`, `unresolved` or authoritative `absent`. Corrupt, missing or
incompletely retained indexes cannot prove absence. Response loss preserves
uncertainty until that lookup or the method's prescribed authoritative state
read resolves it. Protected interaction input is excluded from retained
receipts and automatic replay.

Groups retain immutable per-entry coverage, including unknown and missing
sources. Exact findings merge does not perform semantic judgment. Synthesis is
a separate accepted task with its own identity and conservative round count.
A 50-task group is not one Podway Action under a 32-round ceiling.

## Qualification and reproduction

Both pinned actual native hosts passed the account-free isolated
[feasibility campaign](dolgorae-v014-host-contract-qualification.md), including
replay, conflict, concurrent replay, reconnect, MCP replacement, physical host
loss before acknowledgement and supported original-identity lookup. The lock
binds prototype/harness source hashes, the tested executable and both host
checksums. This qualification covers the native identity/receipt prototype.
Production facade/Broker dispatch, complete results, Controller interactions
and final consumer artifact acceptance retain their scheduled runtime gates.
The historical Codex 0.149.0 `mcp_unavailable` result is unchanged.

Use Rust 1.97.1, Buf 1.69.0, Go 1.26.6, protoc 36.2, protoc-gen-go 1.36.11,
protoc-gen-go-grpc 1.6.1 and the pinned Python validation dependencies.
The public generator records its exact inputs and commands in the descriptor
manifest. These commands compare checked bytes and run the complete gate:

```sh
python3 tools/validators/generate_public_v2.py
.venv/bin/python tools/validators/validate_h1_contract.py
make PYTHON_BIN=.venv/bin/python test
shasum -a 256 docs/protocol/dolgorae-gul-consumer-v2.lock.json
```

Intentional regeneration uses `generate_public_v2.py --write`, followed by
`generate_h1_policies.py` and `generate_h1_lock.py`. A changed locked artifact
creates a different package and requires impact assessment and affected
verification. Do not silently repin an accepted package. Frozen v1 reproduction
retains its historical protoc 35.1 identity.

Create a portable archive after validation with
`python3 tools/validators/package_h1.py --output <new-absolute-archive-path>`.
It includes the lock and all locked repository artifacts, verifies every
checksum before writing, and uses fixed archive metadata for reproducible
bytes. External consumer sources are identified by their selected repository
commit, path and raw SHA-256 in the lock. Their runtime acceptance stays with
the named consumer owners.
The archive's `delivery.json` binds the resolved committed package revision
and lock checksum. The packager refuses bytes that differ from that commit;
use `--revision <package-commit>` when reproducing a previously delivered H1.

## Handoff to Gul

Master delivers the qualified lock, all its repository-relative artifacts,
checked matrix and accepted external prerequisite decisions to Gul E15.
Aquarium and Podway require no new source change identified by this selection.
Dolgorae's TASK-076/077/086 own executor and adapter implementation;
consumer workflow agents retain their existing Primary/Judge, structural
admission and acceptance responsibilities. Prepared Podway contract bytes do
not claim released runtime support. Applicable corrections require assessment
before a changed contract selection is admitted.

Gul E15 returns H2 only after the whole contract/foundation Epic, including
E15-T21 G4-1..7, its assigned corrections and closeout. H2 binds this exact H1
and the completed foundation checks. EPIC-019/TASK-069 then independently
accepts H2 and Gate A. Neither starts during this execution.

H3 follows Dolgorae EPIC-021/TASK-087 and binds the intended runnable artifact,
production adapters and mandatory Aquarium/Podway qualification. Gul E16
returns H4 after its whole product/qualification Epic, including E16-T16,
actual runtime/account/device/soak/packaging cases. TASK-088/089 independently
accept the final combination and release provenance. Those future records
remain explicit obligations; they are not substitutes for H1 requirements.
