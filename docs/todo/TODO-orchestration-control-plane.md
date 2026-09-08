# EPIC-007 Orchestration Control Plane Execution Dossier

Consumer: [EPIC-007](../roadmap/README.md#epic-007-dolgorae-orchestration-control-plane-and-brokered-hierarchy-core).
This temporary execution map owns no product semantics or delivery status.
The [roadmap](../roadmap/README.md) alone owns membership, dependencies,
lifecycle, and completion; the [documentation authority map](../README.md)
owns document roles and closeout.

## Outcome and boundaries

Deliver the supervised local Gul Run gateway, then the transport-independent
Orchestration Session and Brokered Hierarchy core. TASK-023 makes ordinary
low-level Runs usable over the minimum public-v1 gateway; TASK-024 adds durable
Primary/Specialist aggregate behavior behind checked fake adapters. This Epic
does not establish the live Primary tool or MILESTONE-BH1 by itself.

Preserve completed global Profile, independent Run, writer, recovery, immutable
review-target, and External Specialist Engagement behavior. Product semantics
and semantic tests remain in Rust. Existing CLI parsing, generated stubs, and
checked target schemas are not proof that their runtime behavior is available.

## Authority and required artifact map

| Requirement | Canonical owner | Delivery and checked artifacts |
| --- | --- | --- |
| Gateway lifecycle, UDS, peer identity, capabilities, parity | [SPEC-015](../specs/README.md#spec-015-supervised-local-grpc-and-gul-integration), [Public gRPC Gateway](../architecture/README.md#public-grpc-gateway), ADR-021 and ADR-030 in the [ADR owner](../architecture-decision-records/README.md) | TASK-023; [conformance](../protocol/dolgorae-grpc-conformance-v1.json), [coverage](../protocol/dolgorae-grpc-validation-coverage-v1.json), [client policy](../protocol/dolgorae-grpc-client-policy-v1.json), [error map](../protocol/dolgorae-grpc-error-mapping-v1.json) |
| Controller, writer, interactions, artifacts and recovery | SPEC-006 through SPEC-010 and SPEC-013 in the [specification](../specs/README.md) | TASK-023 gateway projections and regressions; TASK-024 aggregate use of the same semantic services |
| Role sources, policy resolution and account separation | [Specialist Role Sources](../specs/README.md#specialist-role-sources), [Specialist Policy Registry](../specs/README.md#specialist-policy-registry), [Role Source and Policy Resolver](../architecture/README.md#role-source-and-policy-resolver), ADR-035 | TASK-024; checked Role-source and policy-authoring contracts, installed-policy and session-snapshot successors, examples, and Rust semantic rejection tests before activation |
| Bootstrap, membership, brokerage, task results and recovery | [SPEC-012](../specs/README.md#spec-012-orchestration-boundary-and-compatibility), [Aggregate Bootstrap Coordinator](../architecture/README.md#aggregate-bootstrap-coordinator), [Orchestration Broker](../architecture/README.md#orchestration-broker), ADR-023 and ADR-028 | TASK-024; [orchestration state baseline](../protocol/dolgorae-orchestration-state-v1.schema.json), [private tool payload](../protocol/dolgorae-orchestration-tool-v1.schema.json), affected exported-state successors and semantic fixtures |
| Validation, operation and public usage | [implementation tips](../implementation-tips/README.md), [operations](../ops/README.md), [product entrypoint](../../README.md), [release notes](../../CHANGELOG.md) | Each task promotes verified guidance to the corresponding owner; no unimplemented command is advertised as usable |

## Dependency order and task outcomes

The local order is TASK-038 -> TASK-023 -> TASK-024. TASK-038 owns the completed
global Profile cutover and hands Role sources and Specialist Policy resolution
to TASK-024. Before activation, the handler verifies the predecessor's committed
outcome and required evidence against current Git and roadmap authority; this
document does not freeze a machine-local reviewed revision or replace evidence.
No external repository mutation is required by this Epic.

### TASK-023: supervised gateway

Implement foreground `dolgorae serve`, single-instance lock/record ownership,
private Unix-socket lifecycle and peer-UID checks, readiness and shutdown,
pinned gRPC generation, and a reconstructable `ControlPlaneRuntime`. Advertise
exactly the 24 implemented BH1 methods from the checked conformance inventory;
unavailable methods fail closed. Route every method through shared semantic
services rather than building a second Run or authorization implementation.

Verify handshake, singleton and socket attacks, readiness, clean shutdown and
crash restart; semantic parity for every minimum method; StartRun allocation
response loss and protected Interaction response loss; event replay; artifact
metadata/chunk bounds, authorization, retention, range and digest failures;
writer recovery; carrier replacement races and secret canaries. Verify that
gateway death neither signals unrelated workers nor releases writer authority.
Use the SPEC-015 coverage matrix for required native OS cases; a fake alone
cannot prove peer credentials, inode ownership, or process cleanup.

Promote gateway ownership, operational diagnosis/recovery, public usage and
the implemented capability boundary to their owners. Complete the task's
verification, independent review and task-scoped commit before TASK-024 starts.

### TASK-024: durable aggregate and Role admission

First synchronize the Role-source, policy input, installed policy and session
snapshot contracts with the accepted global Profile generation. The existing
policy v1 schema embeds pre-cutover Agent Configuration v1; do not treat it as
the completed successor. Preserve historical fixtures, define the successors
under the owning specification, and align affected schema references, exported
state, examples, Machine output and Rust validators before enabling the path.
The public Protobuf source and descriptor remain byte-identical.

Implement prepared bootstrap across SQLite and the Primary Run ledger using
preallocated identities; policy and Role snapshots; one-active-aggregate
membership; write-ahead child reservation/spawn; separate broker-held child
Controller authority; aggregate idempotency; accepted tasks and separate
execution/delivery states; completion, abort and degraded Primary recovery.
Use the existing independent Run and hardened Specialist path, preserving the
External Specialist aggregate-owner authorization boundary.

Implement the transport-independent Primary Orchestration Service and checked
request, approval wait, list, assign, await, collect, cancel and graceful release
operations under both approval modes. Cross-Controller writer movement is
release, verify-none, acquire; a competing writer may win with `WRITER_BUSY`.
It is not atomic handoff.

Verify both sides of every roadmap-listed SQLite, ledger, reservation, Worker,
thread, dispatch, result and receipt durability boundary. Include same-key
replay and conflicting input; duplicate/orphan prevention; parent, role and
use-case denials; allowlists and both approval paths; credential canaries;
retained Specialists after Primary failure; completed-result redelivery without
Turn replay; and fail-closed unknown outcomes. Role admission additionally
covers same-name explicit scope selection, absent/malformed/unsafe source,
source replacement during read, policy installation failure, source deletion
after installation, and unchanged existing-session snapshots after registry
or Profile changes. Missing or changed required Profile bindings fail closed.

Promote policy/Role behavior to specs, resolver and persistence ownership to
architecture, verified developer/operator guidance to its owner, and supported
usage to the product entrypoint. Keep this dossier's later-task guidance current.

## Verification and completion boundary

Before each task, state an adversarial attack budget covering its normative
OS, concurrency, authorization and durability boundaries. Use deterministic
fault barriers and injected time for semantic cases, plus isolated native
process/UDS tests for OS claims. Tests must not use the user's real Dolgorae home,
credentials or account runtime. Run focused checks and the complete repository
gate `make PYTHON_BIN=.venv/bin/python test`; separate explicit authority is
required for opt-in live Codex checks.

Use the roadmap Task Completion Gate, task-owned review targets and task-ID
commits; final Epic acceptance additionally audits the requirement-to-owner-to-
production-to-test-to-document mapping across both tasks. Document validation
or schema examples alone do not prove runtime acceptance. Each shipped task
settles its release-note entry before review. Local runtime evidence stays
outside tracked documents; retain an approved bounded evidence package only
when a downstream consumer requires it.

### Downstream handoffs

- TASK-025 selects and proves live run-bound transport after TASK-023/024.
- TASK-026 integrates the actual Gul client and live Primary tool; it owns the
  live hierarchy acceptance needed for MILESTONE-BH1.
- TASK-027 owns lateral collaboration, mailboxes and virtual-actor scheduling.
- TASK-029 completes the remaining ten public-v1 RPCs and extended conformance.

Do not import these outcomes into EPIC-007 or claim them from fake adapters.
Unavailable required evidence, contradictory authority or an exact undefined
semantic requirement stops the owning task before dependent work begins.

## Dossier closeout

EPIC-007 is this dossier's sole consumer. Only after both tasks and the final
Epic audit pass, promote every durable requirement, rationale and verified
guide to its canonical owner. Recheck all roadmap references, replace this
Epic's `Detailed SOT` with `Canonical Outcomes`, and remove this dossier and
its adopted TODO entry in the approved Epic closeout diff. Retain it if another
consumer is adopted before closeout. Dossier creation itself changes no
roadmap status and authorizes no commit, installation, live test or publication.
