# Research merge storage

The [delegation runtime](../specs/dolgorae-v014-delegation-runtime.md#deterministic-findings-merge-and-explicit-synthesis)
owns this internal generation-8 storage contract. The frozen public merged
identity, digest domain, task bodies and wire messages remain their existing
contracts. These records retain the owners and dependencies needed for recovery.

The existing content store retains two representations under owner kind
`research_merge`, with the genuine UUIDv7 merge ID as owner ID:

- `publication` is complete JCS with schema
  `dolgorae.research-merge-publication/v1` and exactly `merge_ref`, `artifact_id`,
  `owner_run`, `workspace_id`, `workspace_path`, `scope_kind`, `scope_ref`,
  `group_ref`, `identity`, `digest`, `normalized` and `source_results`.
- `merged_output` is the complete checked JCS merged output, at most 32 MiB.
  Its exact byte length and hash must match `identity.merged_output`.

`merge_ref` and `artifact_id` are independently allocated UUIDv7 values.
`owner_run` names a genuine retained Run. `identity` is the existing
`dolgorae.merged-findings/v1` identity; `digest` re-derives from its existing
MDC domain. `normalized` retains the original complete merge input in sorted
entry-key order. The accepted operation envelope binds its complete bytes, and
the receipt binds the actual group, merge and artifact-owner Run.

`source_results` follows the sorted identity sources that have a result digest,
once per source. Each row contains the exact `source` and `publication` proof.
Publication proof contains the real `result_id`, original `result_digest` and
actual `source_manifest` ContentRef. A capacity-finalized body-free source instead
has a null manifest and explicit `discovery_unavailable_reason` of
`artifact_capacity_exceeded`; that row additionally retains `body_free_identity`.
Its real UUID and exact CompleteResult identity are checked against the original
identity publication. Unknown and missing sources remain in `identity.sources`
and output coverage without a fabricated result proof.

The existing immutable metadata map retains `merge-artifact:v1:<artifact_id>`
with its merge ID and `merge-run:v1:<owner_run>:<merge_ref>` with the complete
output byte length. Publication, output, both indices and the accepted receipt
commit in one transaction under the genuine owner's quota lease. The new
logical artifact shares the existing Run quota with ordinary artifacts,
captures, result components and earlier merges. Content-addressed physical reuse
does not remove a distinct logical artifact charge.

An accepted synthesis Task has one `input` representation under owner kind
`synthesis_task`, keyed by its actual Task UUID. Its complete JCS record has schema
`dolgorae.synthesis-input/v1` and exactly `merge_ref`, `merged_findings_digest`,
`merged_input`, `normalized`, `request_schema`, `controller`, `workspace_id`,
`scope_kind`, `scope_ref` and `member_ref`. It binds the original public or facade
normalized request, actual aggregate and member, and the immutable merge
identity/output reference. It commits in the ordinary task-acceptance transaction
with the original operation receipt and caller accounting. A Controller-origin
managed task also retains its actual prepared operation reference under
`dolgorae.controller-synthesis-origin/v1`; it has no native Turn or tool call.

Readers verify the current aggregate Controller and retained accepted owners
before returning content. They check the entire identity and original source
proofs, then the complete output hash and length. Artifact chunks use the existing
1 MiB maximum. Maintenance protects these known owners; unknown owners retain
the existing fail-closed preservation policy. No collect, lookup or Artifact
read publishes records or changes charges. Generation-8 writer admission prevents
older writers from ignoring the new quota and task-recovery owners; supported
read-only generation-6 and generation-7 observations remain intact.
