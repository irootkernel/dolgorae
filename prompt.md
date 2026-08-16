# Dolgorae Sticky Dedicated Run Server Contract Correction and Evidence Reconciliation Request

## 1. Objective

Perform another complete correction, probe, and consistency pass on the Dolgorae architecture after the adoption of the **Sticky Dedicated Run Server** topology.

The preferred direction remains:

```text
Runtime Profile
├── Shared Read-Only Server
│   └── permanently shared_readonly Runs
│
└── Dedicated Run Servers
    └── dedicated Runs that keep one logical server lane
        for their entire lifetime
```

The following core decisions should remain the preferred candidate unless live evidence disproves them:

1. A persistent thread must not move between the shared server and a dedicated server.
2. A `shared_readonly` Run remains permanently read-only.
3. A `dedicated` Run remains assigned to its logical dedicated execution lane.
4. A dedicated physical App Server generation may stop and restart, but the thread remains bound to the same logical lane.
5. Writer authority remains scoped to one canonical Workspace.
6. Different Workspaces may have concurrent writers.
7. Direct Interactive Mode and Managed Agent Mode are both supported.
8. An interactive client remains an LLM-free interface.
9. A managed LLM never receives Controller capability for peer Run control.

The current revision made substantial progress, but the documentation, schemas, interface surface, and probe evidence are not yet internally consistent.

Resolve every finding in this request before closing the architecture task.

---

# 2. Task and Status Constraints

Use the repository's existing status vocabulary, but make all status authorities agree.

Required current state:

```text
TASK-000-D: ACTIVE or IN_PROGRESS
ADR-019: UNDER_REVIEW
codex-0.147.0 required subset:
  release_eligible = false
closure_ready = false
TASK-001 and all later production tasks: PLANNED
```

Do not move `TASK-000-D` to `IN_REVIEW` until:

1. Every P0 finding in this request is resolved.
2. Every P1 finding is resolved or deliberately narrowed by an accepted ADR.
3. Every P2 inconsistency is corrected.
4. Required live probes pass.
5. Probe source and evidence are reproducible.
6. Direct Interactive Mode and Managed Agent Mode have executable public contracts.
7. Run-state semantic validation rejects unsafe combinations.
8. A new independent review package is complete.

Do not mark `TASK-000-D` as `COMPLETED` until an independent read-only review finds no unresolved P0 or P1 issue.

Do not begin `TASK-001` before this closure.

---

# 3. Files and Artifacts in Scope

Review and update at least:

```text
docs/specs.md
docs/architecture.md
docs/architecture-decisions.md
docs/roadmap.md
docs/implementation-notes.md

docs/protocol/dolgorae-machine-v1.schema.json
docs/protocol/dolgorae-run-state-v1.schema.json
docs/protocol/dolgorae-event-v1.schema.json
docs/protocol/dolgorae-event-delivery-v1.schema.json
docs/protocol/dolgorae-interaction-v1.schema.json
docs/protocol/dolgorae-error-contract-v1.json
docs/protocol/codex-0.147.0-required-subset.json

tools/probes/
docs/probes/
docs/probes/results/
docs/reviews/
```

Also search the entire repository for stale assumptions.

A correction must be propagated through:

```text
Specifications
Architecture
ADRs
Roadmap
Schemas
Examples
Error contract
Probe code
Probe reports
Review records
Implementation notes
```

Do not update only the cited paragraph.

---

# Part I. P0 Blocking Findings

## P0-1. Correct the Native-Subagent Probe and Reconcile It With the Wire Evidence

### Problem

The current native-subagent result reports:

```json
{
  "collaboration_item_count": 0,
  "descendant_count": 0
}
```

for both enabled and disabled cases.

The corresponding report states that neither case produced:

```text
A child thread
A receiver-thread ID
A collaboration item
A native-agent lifecycle item
```

However, the retained bounded wire evidence contains:

```text
subAgentActivity items: 4
collabAgentToolCall items: 2
```

The wire objects include fields such as:

```text
subAgentActivity:
- agentPath
- agentThreadId
- kind

collabAgentToolCall:
- senderThreadId
- receiverThreadIds
- agentsStates
- status
- tool
```

The semantic result therefore contradicts its own raw evidence.

This means at least one of the following is wrong:

```text
The probe parser does not recognize the item type.
The parser attributes items to the wrong test case.
The parser does not distinguish root and child threads.
The result generator drops receiver thread information.
The report interprets an observed item as absent.
```

### Required Correction

Fix the probe parser before drawing any architecture conclusion from the result.

For each test case, retain bounded non-secret semantic evidence:

```text
case:
- disabled
- enabled

root_thread_id:
- probe-local alias or salted hash

root_turn_id:
- probe-local alias or salted hash

subAgentActivity:
- item_id alias
- agent_thread_id alias
- agent_path
- kind
- item lifecycle
- containing thread alias
- whether agent thread differs from root thread

collabAgentToolCall:
- item_id alias
- sender_thread_id alias
- receiver_thread_id aliases
- tool
- status
- bounded agentsStates shape
```

Do not store prompt content or secret-bearing model output.

### Required Questions

The corrected probe must answer:

1. Why did `subAgentActivity` appear in the disabled case?
2. Did the enabled case create a receiver thread different from the root thread?
3. Does the receiver thread appear in `thread/list`, `thread/read`, or another supported query?
4. Does the receiver thread progress through a terminal lifecycle?
5. Does `agentThreadId` identify a real child thread?
6. Is `collabAgentToolCall` merely a root activity item, or evidence of an actual child Run?
7. Does the parent-child relation survive App Server restart?
8. Does notification opt-out affect the observed lifecycle?
9. Is the item shape stable across repeated runs?

### Product Decision Until Corrected

It is acceptable to keep Codex-native subagents disabled by policy.

The reason must be recorded as:

```text
Native-subagent lifecycle evidence is internally inconsistent and remains
unsupported pending a corrected exact-version probe.
```

Do not state that no lifecycle item was observed.

### Important Conceptual Separation

The documents must explicitly distinguish:

```text
Independent Managed Dolgorae Runs:
- Supported architecture
- Created by an external Supervisor Broker
- Each is an independent Dolgorae Run

Codex-native subagents:
- Internal descendants of one Codex Run
- Disabled or under re-evaluation for codex-cli 0.147.0
```

Failure to support Codex-native subagents must not imply that Managed Agent Mode is unsupported.

---

## P0-2. Add Public Inputs for `control_mode`, `execution_lane`, and `required_assurance`

### Problem

The specifications require every Run to record immutable values for:

```text
control_mode
execution_lane
required_assurance
```

The current public `run start` interface does not provide corresponding options.

Without these inputs, external clients cannot request:

```text
Direct Interactive + dedicated
Direct Interactive + explicitly shared read-only
Managed planning + shared read-only
Managed implementation + dedicated
Managed high-assurance implementation
```

The current default of:

```text
purpose = interactive
```

for all Run starts also conflicts with the requirement that managed Runs explicitly provide purpose and lane.

### Required Public Interface

Add a machine interface similar to:

```text
dolgorae run start \
  --workspace <path> \
  --profile <runtime-profile> \
  [--control-mode direct-interactive|managed-agent] \
  [--execution-lane shared-readonly|dedicated] \
  [--required-assurance best-effort-personal-alpha|verified-thread-scoped-control|strong-process-containment] \
  [--purpose <purpose>] \
  [--purpose-label <label>] \
  [--parent-namespace <namespace>] \
  [--parent-kind <kind>] \
  [--parent-id <id>] \
  ...
```

Use the repository's normal naming convention if it differs, but expose equivalent semantics.

### Validation Matrix

Recommended rules:

| Controller kind         | Allowed control mode | Defaults                         |
| ----------------------- | -------------------- | -------------------------------- |
| `human_cli`             | `direct_interactive` | mode and purpose may default     |
| `interactive_client`    | `direct_interactive` | mode and purpose may default     |
| `workflow_orchestrator` | `managed_agent`      | mode, purpose, and lane required |
| `automation`            | `managed_agent`      | mode, purpose, and lane required |

Recommended Direct Interactive defaults:

```text
control_mode = direct_interactive
purpose = interactive
execution_lane = dedicated
required_assurance = best_effort_personal_alpha
```

Managed Agent Mode must not silently default to `interactive`.

### Required Machine Output

Run output must expose:

```text
control_mode
purpose
execution_lane
requested_assurance
achieved_assurance
controller kind
```

### Required Errors

Add or reuse stable errors for:

```text
CONTROL_MODE_CONTROLLER_MISMATCH
CONTROL_MODE_REQUIRED
EXECUTION_LANE_REQUIRED
EXECUTION_LANE_UNSUPPORTED
ASSURANCE_LEVEL_UNAVAILABLE
PURPOSE_REQUIRED
```

### Required Fixtures

Add positive and negative fixtures for:

```text
Valid direct interactive default
Valid direct shared-read-only Run
Valid managed planning Run
Valid managed implementation Run
Managed Run missing purpose
Managed Run missing lane
Interactive Controller requesting managed mode
Workflow Controller requesting direct mode
Requested assurance above profile capability
```

---

## P0-3. Add a Public Shared-to-Dedicated Successor Operation

### Problem

The architecture states that a `shared_readonly` Run cannot become a writer.

When such a Run needs write access, the current design requires:

```text
A lineage-linked dedicated successor
```

However, no public operation creates that successor with all required metadata.

The existing `run fork` surface does not expose enough information to request:

```text
dedicated lane
control mode
purpose
required assurance
source terminal turn
handoff summary
artifact references
controller behavior
```

This leaves the normative successor workflow unusable by an interactive client or workflow orchestrator.

### Required Direction

Choose one public design.

#### Option A: Explicit Successor Command

Recommended:

```text
dolgorae run create-successor \
  --from <source-run-id> \
  --from-turn <source-terminal-turn-id> \
  --execution-lane dedicated \
  --control-mode <mode> \
  --purpose <purpose> \
  --required-assurance <level> \
  [--handoff-summary-fd <fd>] \
  [--artifact-ref <id>]... \
  --controller-fd <fd>
```

#### Option B: Extend `run fork`

For example:

```text
dolgorae run fork \
  --from <source-run-id> \
  --from-turn <turn-id> \
  --execution-lane dedicated \
  --control-mode managed-agent \
  --purpose implementation \
  --required-assurance ...
```

### Required Successor Semantics

Define:

```text
Successor receives a new Run ID.
Successor receives a new Codex thread.
Source Run remains shared_readonly.
Source execution lane does not change.
Successor execution lane is dedicated.
Lineage is durable and immutable.
Controller rules are revalidated.
Writer authority is not inherited automatically.
```

### Required Lineage Data

Persist at least:

```text
source Run ID
source terminal Turn ID
source thread ID
creation reason
source controller kind
destination controller kind
bounded handoff summary digest
artifact references
workspace baseline
timestamp
```

### Handoff Content

The successor may receive:

```text
A bounded human-readable summary
Selected artifact references
Selected source files
Source final response
Explicit implementation request
```

Do not copy hidden reasoning.

### Interaction With Direct Interactive Mode

When an interactive shared Run requests write access, return either:

```text
SHARED_RUN_WRITE_FORBIDDEN
```

with an allowed action:

```text
create_dedicated_successor
```

or support an explicit atomic successor creation request.

The interactive client should be able to present:

```text
This session is permanently read-only.
Create a new write-capable session using the current context?
```

### Interaction With Managed Agent Mode

A workflow orchestrator may create a dedicated implementation successor from a planning or research Run.

The Supervisor Broker, not the managed LLM, performs the operation.

---

## P0-4. Reconcile the Required-Subset Manifest, ADR Status, Roadmap, and Closure State

### Problem

The repository currently contains contradictory release authorities.

The pinned required-subset manifest states:

```json
{
  "release_eligible": false,
  "failure": "TASK-000-D_BLOCKED_ON_CONCRETE_DEDICATED_LANE_BEHAVIOR"
}
```

It also retains unverified items such as:

```text
access-policy transition
configuration classification
```

Other documents claim:

```text
ADR-019: Accepted
closure_ready = true
No unresolved P0 or P1 findings
Architecture evidence complete
```

These claims cannot coexist.

### Required Immediate State

Until all findings and probes in this request are resolved, synchronize the repository to:

```text
TASK-000-D = ACTIVE or IN_PROGRESS
ADR-019 = UNDER_REVIEW
required_subset.release_eligible = false
closure_ready = false
TASK-001 = PLANNED
```

### Required Closure Authority

Define one latest closure authority.

Recommended:

```text
roadmap.md:
- owns task status

codex-0.147.0-required-subset.json:
- owns exact pinned compatibility eligibility

architecture-decisions.md:
- owns accepted or provisional topology decisions

latest follow-up closure report:
- owns independent review conclusion
```

These four authorities must agree.

### Final Synchronization

After all gates pass, update in one change:

```text
codex-0.147.0-required-subset.json
specs.md
architecture.md
architecture-decisions.md
roadmap.md
implementation-notes.md
closure-status report
consistency report
review index
```

Do not mark an ADR `Accepted` while its required live capability remains unverified.

---

## P0-5. Narrow the Same-`CODEX_HOME` Multi-Server Conclusion and Extend the Probe

### Problem

The current Sticky Dedicated evidence demonstrates useful feasibility:

```text
Three App Servers initialized
All reported the same CODEX_HOME
Model catalog digests matched
Ten idle App Servers remained alive
Two writers in different Workspaces completed
Closed-generation thread history remained readable
```

However, the evidence also states:

```text
preexisting_home_files_inspected = false
codex_home_may_have_been_modified_by_app_server = true
```

The probe did not fully validate:

```text
Model cache stability
State database integrity
Config or trust mutations
Authentication refresh behavior
Long-duration concurrency
Repeated crash and restart
Thread-index consistency under stress
```

Therefore the current evidence proves:

```text
Basic same-home coexistence feasibility
```

It does not yet prove:

```text
Storage-level and long-duration same-home safety
```

### Required Documentation Change

Replace broad conclusions such as:

```text
Same-home multi-server safety: Passed
```

with:

```text
Basic same-home coexistence: Passed
Storage-level and long-duration safety: Pending
```

Keep topology acceptance provisional until the extended campaign passes.

### Required Extended Probe

Use a dedicated authenticated test `CODEX_HOME` whenever possible.

Capture secret-free before and after metadata for relevant state:

```text
File path or category
Size
Device and inode
Modification time
SHA-256 where safe
SQLite integrity result
JSON parse result
```

Test at least:

#### Model and Cache State

```text
Repeated model/list from 1, 2, 5, and 10 servers
Model capability stability
models_cache or equivalent cache integrity
No oscillation between server instances
Consistent clientInfo.name and clientInfo.version
```

#### Thread and Rollout Storage

```text
Repeated thread creation
Concurrent thread starts
Concurrent thread/read
Thread close or delete where supported
Rollout parseability
Thread-index consistency
Restart consistency
```

#### State Database

```text
SQLite integrity checks
No unexplained lock failures
No partial or lost records
No malformed state after crash
```

#### Configuration and Trust

```text
Read thread start
Write thread start
Project trust mutation
Configuration file changes
Server-key stability
```

#### Authentication

```text
Concurrent account access
Token refresh behavior
No unexpected logout
No corrupted authentication metadata
```

#### Stress and Failure

```text
Long-running concurrent writers in different Workspaces
Server crash during a turn
Server restart
Several rounds of start, stop, and resume
Five or more concurrent dedicated Runs
```

### Decision Gate

If storage-level safety cannot be demonstrated:

```text
Block multiple App Servers sharing one CODEX_HOME
```

and re-evaluate:

```text
Single shared server
Separate verified home isolation
All Runs sharing one process
Another supported topology
```

Do not silently copy authentication or `CODEX_HOME` state per Run.

---

## P0-6. Enforce Write-Safety Invariants in the Run-State Contract

### Problem

The current run-state schema validates field shapes but permits unsafe cross-field combinations.

Examples currently accepted include:

```text
writer_authority = none
effective_policy = write and verified

writer_authority = active
policy epoch does not match server epoch

shared_readonly lane
background mechanism = dedicated_lane_process_census

requested assurance = strong
achieved assurance = best_effort
```

The most dangerous case is:

```text
No writer authority
+
Write-effective Codex policy
```

This allows actual write capability without the authoritative workspace writer grant.

### Required Direction

Use a state-discriminated union or a mandatory semantic validator.

Recommended state variants:

```text
SharedReadonlyState
DedicatedUnstartedState
DedicatedReaderState
DedicatedWriterReservedState
DedicatedWriterActiveState
DedicatedWriterReleasingState
DedicatedWriterBlockedUnknownState
PausedDedicatedState
FailedDedicatedGenerationState
```

### Required Invariants

#### Shared Read-Only State

```text
execution_lane = shared_readonly
writer_authority.state = none
effective_policy.access = read
effective_policy.verification = verified
dedicated server lane is absent
background mechanism is valid for shared lane
```

#### Dedicated Unstarted State

```text
execution_lane = dedicated
thread_id = null
server lane may be absent
writer_authority.state = none
effective_policy.access = unknown or read
```

#### Dedicated Reader State

```text
execution_lane = dedicated
writer_authority.state = none
effective_policy.access = read
effective_policy.verification = verified
```

#### Dedicated Writer Reserved

```text
execution_lane = dedicated
writer_authority.state = reserved
effective_policy.access must not yet be treated as write-effective
transaction ID is present
writer generation is present
```

#### Dedicated Writer Active

```text
execution_lane = dedicated
writer_authority.state = active
effective_policy.access = write
effective_policy.verification = verified
server lane state = ready
effective policy server epoch = dedicated server epoch
writer generation = effective policy writer generation
achieved assurance satisfies requested assurance
```

#### Dedicated Releasing

```text
execution_lane = dedicated
writer_authority.state = releasing
new turns are fenced
policy transition operation ID exists
```

#### Dedicated Blocked Unknown

```text
writer_authority.state = blocked_unknown
write acquisition by another Run is forbidden
reconciliation action is present
```

### Assurance Ordering

Define a normative order:

```text
best_effort_personal_alpha
< verified_thread_scoped_control
< strong_process_containment
```

Enforce:

```text
achieved_assurance >= requested_assurance
achieved_assurance <= runtime profile maximum
```

### Required Artifacts

If JSON Schema cannot express all constraints cleanly, provide:

```text
dolgorae-run-state-v1.schema.json
run_state_semantic_validator_v1
positive fixtures
negative fixtures
```

The semantic validator must be normative and versioned.

---

# Part II. P1 Major Findings

## P1-1. Generalize the Product Boundary Beyond “Persistent Subagents”

### Problem

The top-level product definition still describes Dolgorae primarily as:

```text
A persistent Codex subagent system
```

Direct Interactive Mode is not a subagent relationship.

```text
User
→ LLM-free interactive client
→ Dolgorae Run
→ Codex
```

There is no supervisor AI above the direct interactive Run.

### Required Product Definition

Recommended:

> Dolgorae provides persistent, controller-owned Codex Runs for direct interactive sessions and externally managed agents.

A Run may be:

```text
Direct Interactive Run
Managed Agent Run
Managed Workflow-Stage Run
```

### Instruction Composition

Define:

```text
Common immutable safety prefix
+
control-mode-specific prefix
+
purpose-specific prefix
+
bounded controller instructions
```

Version the instruction contract.

Example:

```text
instruction_schema = dolgorae.instructions/v1
common_prefix_version = 1
direct_interactive_prefix_version = 1
managed_agent_prefix_version = 1
```

### Direct Interactive Prefix

Conceptually:

```text
This is a user-controlled interactive development session.
The user communicates through an external interface.
Answer the user's requests directly.
Do not claim to be a subordinate workflow agent.
Do not create or control peer Dolgorae Runs.
```

### Managed Agent Prefix

Conceptually:

```text
This is a managed agent Run controlled by an external orchestrator.
Perform the assigned bounded role.
Do not create or control peer Dolgorae Runs.
Return bounded results to the orchestrator.
Do not claim workflow, closure, or sibling-agent authority.
```

Remove universal wording that labels every Run a subagent.

---

## P1-2. Explicitly Separate Managed Agent Mode From Codex-Native Subagents

### Problem

Readers may incorrectly interpret:

```text
native_subagents = unavailable
```

as meaning that Managed Agent Mode is unavailable.

These are different features.

### Required Capability Matrix

Add a normative table:

| Capability                                   | v1 status                                   |
| -------------------------------------------- | ------------------------------------------- |
| Independent managed Dolgorae Runs            | Supported                                   |
| External Supervisor Broker                   | Supported contract                          |
| Multiple managed Runs under one orchestrator | Supported                                   |
| Peer Run mutation by an LLM                  | Forbidden                                   |
| Controller capability visible to LLM         | Forbidden                                   |
| Codex-native subagents inside one Run        | Disabled or under re-evaluation for 0.147.0 |

### Required Terminology

Use:

```text
Managed Agent Run:
An independent Dolgorae Run controlled by an external orchestrator.

Codex-Native Subagent:
A descendant created internally by Codex within one parent Run.
```

Do not use the word `subagent` without qualification where the distinction matters.

---

## P1-3. Define Command and Background-Process Policy for `shared_readonly`

### Problem

A shared read-only Run may still cause Codex to execute commands such as:

```text
grep
git status
compiler
test
file watcher
background shell
```

These commands run under the shared App Server process tree.

Dolgorae cannot reliably attribute or clean them per Run using the dedicated-lane process census.

The current shared lane therefore lacks a defined command and background-process lifecycle.

### Required Product Decision

Choose one explicit v1 policy.

#### Recommended Safe Policy

A `shared_readonly` Run may not execute arbitrary commands.

```text
shared_readonly:
- read-only file and reasoning operations
- no command execution approval
- no background terminal
- command request automatically declined
```

Any Run requiring shell commands should use a dedicated lane, even when it does not write files.

This may include:

```text
Planning with repository search commands
Review requiring tests
Research requiring local tooling
```

#### Alternative Limited Policy

Allow foreground command execution but explicitly declare:

```text
No per-Run process absence guarantee
No background process support
Run pause or close cannot prove descendant cleanup
Profile stop owns aggregate shared-server cleanup
```

This is weaker and requires clear capability reporting.

### Lane-Specific Capability Reporting

Do not expose one global statement:

```text
background_execution_control = supported
```

Use:

```text
shared_readonly.command_execution
shared_readonly.background_control
dedicated.command_execution
dedicated.background_control
```

### Required Tests

```text
Shared command request
Shared command approval policy
Shared background attempt
Dedicated foreground command
Dedicated background process
Shared Run close
Profile-level shared-server cleanup
```

---

## P1-4. Avoid Eager Physical Server Startup for Empty Dedicated Runs

### Problem

The current design may start a Dedicated Run Server before publishing a newly created Run.

Direct Interactive Mode defaults to `dedicated`.

Creating several empty sessions may therefore immediately start several App Servers.

Observed PoC resource use is approximately:

```text
1 dedicated server: about 215 MiB RSS
5 dedicated servers: about 758 MiB RSS
10 dedicated servers: about 1.21 GiB RSS
```

This conflicts with the expected interactive use case where users may create several sessions per Workspace.

### Recommended Direction

Separate logical lane allocation from physical server startup.

At `run start`:

```text
Allocate Run ID
Persist execution_lane = dedicated
Allocate logical lane ID
Set physical server state = absent
Set thread_id = null
Set server_epoch = null or not allocated
Publish the logical Run
```

At first `send` or `submit`:

```text
Start physical Dedicated Run Server
Publish server epoch
Create or resume thread
Start first turn
```

### Alternative UI Contract

An interactive UI may keep a draft session locally and create a Dolgorae Run only when the first prompt is submitted.

If this is the chosen approach, document it as an integration recommendation, not as a hidden dependency.

### Required State Model

Add a state such as:

```text
execution_lane = dedicated
server_lane.state = absent
thread_id = null
lifecycle = idle
```

This must be a valid first-class state.

### Resource Policy

Document:

```text
Recommended live dedicated-server count
Pause behavior
On-demand restart
Optional idle shutdown policy
Resource warning thresholds
```

Do not trade correctness for aggressive idle shutdown without live evidence.

---

## P1-5. Remove Historical Writer Capsule Requirements From Normative Specifications

### Problem

Historical transient Writer Capsule text remains inside normative specifications.

The section is labeled historical or non-normative but still contains uppercase:

```text
MUST
MUST NOT
SHOULD
```

The document header states that uppercase requirement keywords are normative.

This creates ambiguity.

### Required Correction

Move superseded Writer Capsule material to:

```text
Rejected alternative in an ADR
Historical architecture appendix
Review disposition
```

Remove it from the normative specification body.

If historical text remains in `specs.md`, do not use normative keywords and clearly mark it as non-authoritative, but moving it out is preferred.

---

## P1-6. Correct the “Every Run Starts and Resumes as a Reader” Rule

### Problem

The specification states:

```text
Every Run starts and resumes as a reader.
```

This conflicts with the dedicated first-write protocol and writer recovery.

A threadless dedicated Run may begin its first thread under write policy after durable writer reservation.

A dedicated writer may also resume after process restart under policy derived from durable writer authority.

### Recommended Wording

```text
Every newly allocated Run begins with no writer authority.

A shared_readonly Run is always read-effective.

A dedicated Run begins without writer authority and is read-effective unless
its first operation completes the durable first-write activation protocol.

A resumed dedicated generation uses the policy required by its durable
writer-authority state and reconciliation result.

A Run must never be write-effective without active durable writer authority.
```

Update every state table and example accordingly.

---

## P1-7. Make All Closure-Relevant Probe Evidence Reproducible

### Problem

The review package includes probe results and checksums but not the corresponding probe source.

The recorded Git revision was not independently retrievable from the public repository, and the probe environment indicates a dirty working tree.

This prevents independent reproduction.

The native-subagent contradiction also demonstrates why probe source is required.

### Required Evidence Package

For every closure-relevant probe, include:

```text
Probe source
Input fixtures
Expected semantic output
Actual semantic output
Bounded wire evidence
Exact command
Python version
Dependency versions
OS version and build
Codex version
Codex binary digest
Generated schema digest
Dolgorae Git revision
Working-tree state
Source script SHA-256
Result SHA-256
Timestamp
```

### Repository Requirement

Before closure:

```text
Commit the probe source.
Use an immutable Git commit.
Prefer a clean working tree.
```

If a dirty working tree is unavoidable for one experiment, include:

```text
git diff
git diff --cached
untracked-file manifest
aggregate patch SHA-256
```

Do not use a local-only uncommitted probe as final closure evidence.

---

## P1-8. Unify Review Status and Establish a Single Latest Review Package

### Problem

Current review documents report conflicting states:

```text
Independent review not complete
No unresolved P0/P1
closure_ready = true
TASK remains active
TASK status = IN_REVIEW
No closure artifact exists
```

The latest closure authority is unclear.

### Required Immediate State

Set:

```text
TASK-000-D = ACTIVE
ADR-019 = UNDER_REVIEW
required_subset.release_eligible = false
closure_ready = false
```

### New Review Package

Preserve prior reviews as historical and create:

```text
docs/reviews/task-000-d-third-follow-up-review.md
docs/reviews/task-000-d-third-follow-up-disposition.md
docs/reviews/task-000-d-third-follow-up-closure.md
```

The closure file must initially state:

```text
Not eligible for closure.
```

Update it only after every P0 and P1 finding is independently reviewed.

### Finding Table

List every finding from this request separately.

Do not merge or omit findings.

---

## P1-9. Decide Whether `purpose` Is Immutable

### Problem

One ADR describes `purpose` as mutable descriptive metadata.

The public interface contains no purpose-update operation, and the specification does not define purpose transitions.

### Recommended Decision

Make `purpose` immutable in v1.

```text
purpose:
- recorded at Run creation
- used for audit and orchestration context
- not changed during the Run lifetime
```

Display labels, aliases, and user-facing names should belong to the external UI or orchestrator.

### Alternative

If purpose must be mutable, add:

```text
run set-purpose
Controller authorization
Allowed transitions
Audit event
State revision
Orchestrator impact
Schema update
```

Do not continue to describe it as mutable without a mutation contract.

---

## P1-10. Add End-to-End Control-Mode Conformance Tests

### Problem

Direct Interactive Mode and Managed Agent Mode are present in the documents, but no complete client-level evidence demonstrates the behavioral contracts.

### Required Test Clients

Create minimal fake controllers:

```text
fake_interactive_controller
fake_workflow_orchestrator
```

They do not need production UI.

### Direct Interactive Scenario

Test:

```text
Create direct interactive dedicated Run
Perform several turns
Receive a user-input question
Receive command or file approval
Resolve interaction through Controller
Receive final response through minimal projection
Acquire writer
Write a file
Release writer
Remain in dedicated lane
Pause physical server
Resume same thread in same logical lane
```

Verify:

```text
No LLM exists in the interactive client
No reasoning content is delivered
Observer cannot resolve
Controller capability is not in prompt, environment, argv, or artifact
```

### Managed Agent Scenario

Test:

```text
Create managed planning shared Run
Create managed implementation dedicated Run
Create managed review shared Run
Route managed interaction to Orchestrator
Reject observer resolution
Reject LLM peer-Run mutation
Block Direct writer while Managed writer owns Workspace
Release Managed writer
Allow Direct writer acquisition afterward
Create a dedicated successor from a shared planning Run
```

### Mismatch Tests

```text
interactive_client + managed_agent
workflow_orchestrator + direct_interactive
managed mode without purpose
managed mode without lane
shared Run requesting writer
observer resolving interaction
cross-controller writer handoff
```

---

# Part III. P2 Documentation Findings

## P2-1. Resolve the Singleton stdout and stderr Topology Contradiction

### Problem

One section states:

```text
stdout and stderr are drained into bounded profile logs
```

Another states:

```text
singleton stdio is connected to /dev/null
```

### Required Correction

Document one exact descriptor topology based on live `0.147.0` behavior.

Recommended:

```text
stdin:
- /dev/null unless required by the selected launch mode

stdout:
- bounded redacting log-drainer pipe or /dev/null if verified unused

stderr:
- bounded redacting log-drainer pipe
```

Define:

```text
Maximum line size
Maximum file size
Rotation
Redaction
Non-UTF-8 handling
Behavior when the log sink fails
Non-authoritative status
```

---

## P2-2. Make the Roadmap Introduction Match the Actual Task Status

### Problem

The roadmap introduction says `TASK-000-D` is active while the task table marks it `IN_REVIEW`.

### Required Correction

Use one state.

Given the findings in this request:

```text
TASK-000-D = ACTIVE
EPIC-000-D = ACTIVE
```

Keep subsequent tasks `PLANNED`.

Update all status summaries and badges.

---

## P2-3. Use One Canonical `purpose` Shape

### Problem

The run-state schema represents:

```json
"purpose": "implementation"
```

while the machine schema represents:

```json
{
  "purpose": {
    "kind": "implementation",
    "external_label": "..."
  }
}
```

The same field name has two incompatible shapes.

### Required Correction

Choose one canonical representation.

Recommended:

```json
{
  "purpose": {
    "kind": "implementation",
    "external_label": null
  }
}
```

Use the same shape in:

```text
Run state
Machine output
Run manifest
Events
Schemas
Examples
```

If the compact run-state field is intentionally only the kind, name it:

```text
purpose_kind
```

Do not use `purpose` for both a string and an object.

---

# Part IV. Direct Interactive Mode Contract

## 1. Definition

Direct Interactive Mode is:

```text
User
→ LLM-free interactive client
→ Dolgorae Direct Interactive Run
→ Codex
```

The external client provides:

```text
Authentication
Remote connectivity
Session navigation
Prompt input
Question and approval UI
Final-response rendering
File and image browsing
Event replay
```

The external client does not provide:

```text
LLM inference
Codex App Server
Writer authority
Thread lifecycle
Agent orchestration
```

## 2. Defaults

Recommended:

```text
control_mode = direct_interactive
controller.kind = interactive_client or human_cli
purpose = interactive
execution_lane = dedicated
required_assurance = best_effort_personal_alpha
```

An explicitly permanent read-only Direct Run may select:

```text
execution_lane = shared_readonly
```

It must not be promoted in place.

## 3. Interaction Routing

```text
Codex
→ Dolgorae interaction journal
→ interactive Controller
→ user-facing UI
→ Controller response
→ Dolgorae
→ Codex
```

Only the Controller may resolve the interaction.

## 4. Writer Behavior

A Direct dedicated Run may:

```text
Acquire writer authority
Become write-effective after verification
Release writer authority
Remain bound to the same dedicated lane
Become read-effective
Later reacquire writer authority
```

Writer release must not move its thread into the shared server.

---

# Part V. Managed Agent Mode Contract

## 1. Definition

Managed Agent Mode is:

```text
Trusted external Supervisor Broker
→ one or more Dolgorae Managed Agent Runs
→ Codex
```

Example:

```text
Workflow Orchestrator
├── Planning Run
├── Implementation Run
├── Review Run
└── Research Run
```

## 2. Defaults

Managed mode must not use implicit interactive defaults.

The Controller must provide:

```text
control_mode = managed_agent
purpose
execution_lane
required_assurance
parent reference where applicable
```

Recommended lanes:

| Purpose                             | Lane                                                            |
| ----------------------------------- | --------------------------------------------------------------- |
| Planning without command execution  | `shared_readonly`                                               |
| Research without command execution  | `shared_readonly`                                               |
| Review without command execution    | `shared_readonly`                                               |
| Implementation                      | `dedicated`                                                     |
| Documentation update                | `dedicated`                                                     |
| Any Run requiring command execution | `dedicated`, unless shared command policy explicitly permits it |

## 3. Supervisor Broker Rule

The managed LLM may request:

```text
Create child agent
Interrupt child agent
Inspect child result
Create implementation successor
```

Only the trusted external Supervisor Broker may execute the corresponding Dolgorae machine operation.

Controller capability must never enter:

```text
LLM prompt
LLM environment
Workspace
Command argv
Tool result
Artifact
Client-safe event
```

## 4. Interaction Routing

```text
Codex interaction
→ Dolgorae
→ workflow_orchestrator Controller
→ Orchestrator policy
   ├── automatic response
   ├── rejection
   └── explicit user escalation
```

An observer may receive a redacted summary but cannot resolve the interaction.

---

# Part VI. Capability and Assurance Reporting

Expose lane-specific capabilities.

Example:

```json
{
  "shared_readonly": {
    "command_execution": "disabled",
    "background_control": "not_applicable",
    "writer_support": false
  },
  "dedicated": {
    "command_execution": "supported",
    "background_control": "best_effort_personal_alpha",
    "writer_support": true
  }
}
```

Do not publish one global capability that implies stronger guarantees than one lane actually provides.

Expose:

```text
requested_assurance
achieved_assurance
profile maximum assurance
```

Reject a Run when the requested level cannot be met.

---

# Part VII. Required Error Contract Updates

Add or reuse stable errors for:

```text
CONTROL_MODE_REQUIRED
CONTROL_MODE_CONTROLLER_MISMATCH
PURPOSE_REQUIRED
EXECUTION_LANE_REQUIRED
EXECUTION_LANE_UNSUPPORTED
EXECUTION_LANE_IMMUTABLE

SHARED_RUN_WRITE_FORBIDDEN
SUCCESSOR_SOURCE_NOT_TERMINAL
SUCCESSOR_LINEAGE_INVALID
SUCCESSOR_CONTROLLER_INVALID

ASSURANCE_LEVEL_UNAVAILABLE
RUN_STATE_INVARIANT_VIOLATION
WRITER_POLICY_WITHOUT_AUTHORITY
WRITER_EPOCH_MISMATCH

NATIVE_SUBAGENT_EVIDENCE_INCONSISTENT
NATIVE_SUBAGENT_UNSUPPORTED

SAME_HOME_STORAGE_SAFETY_UNVERIFIED
SAME_HOME_MULTI_SERVER_UNSAFE

PROBE_EVIDENCE_UNREPRODUCIBLE
CLOSURE_AUTHORITY_CONFLICT
```

Use existing errors where the semantics already match.

Each error must define:

```text
Exit class
Retryable flag
Closed details schema
Permitted next actions
Relevant IDs and generations
```

---

# Part VIII. Required Validation

## 1. Static Validation

Run:

```text
Markdown link validation
Schema-reference validation
Error-code validation
Roadmap single-active-task validation
Status consistency validation
Terminology consistency validation
Repository-wide stale Writer Capsule search
```

Search for:

```text
Every Run is a subagent
Every Run starts and resumes as reader
Transient Writer Capsule MUST
Same-home safety passed
closure_ready true
ADR-019 Accepted
purpose mutable
```

---

## 2. Schema and Semantic Validation

Validate:

```text
Machine schema
Run-state schema
Event schemas
Interaction schema
Error contract
Required-subset manifest
```

Add negative fixtures for:

```text
Writer authority none + write-effective policy
Active writer + mismatched server epoch
Shared lane + dedicated process-census mechanism
Requested assurance above achieved assurance
Managed mode without purpose
Managed mode without lane
Invalid Controller and mode combination
Shared Run with writer authority
Dedicated successor without lineage
Conflicting purpose shapes
```

---

## 3. Probe Validation

Repeat or extend:

```text
Native-subagent probe with corrected parser
Basic same-home coexistence probe
Storage-level same-home safety probe
Long-duration multi-server probe
Direct Interactive conformance probe
Managed Agent conformance probe
Shared-to-dedicated successor probe
Run-state semantic validator tests
Lazy dedicated-server resource probe
```

For every probe, retain:

```text
Source
Exact command
Environment
Bounded wire evidence
Semantic result
Checksums
Clean immutable Git revision
```

---

# Part IX. Review and Closure Package

Preserve previous reviews as historical.

Create:

```text
docs/reviews/task-000-d-third-follow-up-review.md
docs/reviews/task-000-d-third-follow-up-disposition.md
docs/reviews/task-000-d-third-follow-up-closure.md
```

The new review must list every finding in this request.

Use:

| ID | Severity | Status | Selected correction | Evidence | Remaining risk |
| -- | -------- | ------ | ------------------- | -------- | -------------- |

The closure report must initially state:

```text
Not eligible for closure.
```

Update it only after the independent review passes.

---

# Part X. Expected Deliverables

Return:

## 1. Updated Source-of-Truth Documents

```text
specs.md
architecture.md
architecture-decisions.md
roadmap.md
implementation-notes.md
```

## 2. Updated Protocol Artifacts

```text
Machine schema
Run-state schema
Event schemas
Interaction schema
Error contract
Codex 0.147.0 required subset
Positive fixtures
Negative fixtures
Semantic validators
```

## 3. Updated Probe Evidence

```text
Corrected native-subagent campaign
Same-home storage campaign
Direct Interactive campaign
Managed Agent campaign
Successor campaign
Resource campaign
```

## 4. Decision Summary

Summarize:

```text
Native-subagent support status
Direct Interactive defaults
Managed Agent requirements
Shared command policy
Dedicated lazy-start policy
Successor API
Purpose immutability
Same-home safety level
Run-state invariant enforcement
Final ADR and release status
```

## 5. Consistency Report

List:

```text
Every changed requirement
Every changed ADR
Every new command
Every new field
Every schema change
Every error change
Every probe added
Every roadmap status change
```

---

# Part XI. Completion Criteria

`TASK-000-D` may move to `IN_REVIEW` only when:

1. Native-subagent semantic results agree with wire evidence.
2. Native-subagent support status is based on corrected evidence.
3. `run start` accepts or deterministically derives control mode, lane, and assurance.
4. Managed Runs cannot silently default to interactive.
5. A public dedicated-successor operation exists.
6. Successor lineage and Controller rules are complete.
7. Required-subset, ADR, roadmap, and closure state agree.
8. Same-home evidence is no longer overstated.
9. Storage-level same-home safety passes or the topology is narrowed.
10. Run-state validation rejects write capability without writer authority.
11. Direct Interactive product semantics are explicit.
12. Managed Agent semantics are explicit.
13. Managed Agent Mode is clearly separated from Codex-native subagents.
14. Shared-lane command policy is explicit.
15. Empty dedicated Runs do not consume unexpected resources without a documented product decision.
16. Historical Writer Capsule requirements are removed from normative specifications.
17. Resume and first-write wording matches Sticky Dedicated behavior.
18. Probe evidence is independently reproducible.
19. Review authorities agree.
20. Purpose mutability is resolved.
21. Mode conformance tests pass.
22. P2 documentation inconsistencies are fixed.
23. `TASK-000-D` is the only active task.

`TASK-000-D` may become `COMPLETED` only when:

1. An independent review finds no unresolved P0 or P1 finding.
2. Every finding has a documented disposition.
3. Every claimed Codex 0.147.0 behavior has reproducible evidence.
4. ADR-019 is accepted only after its live gates pass.
5. `release_eligible` becomes true only after the compatibility manifest is complete.
6. Closure status, roadmap, ADRs, schemas, and probes agree.
7. Both Direct Interactive Mode and Managed Agent Mode are executable through the public machine interface.
8. `TASK-001` has not started before closure.

Do not begin production implementation before these conditions are satisfied.
