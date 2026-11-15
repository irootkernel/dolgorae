use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rust_sources(directory: &Path) -> Vec<PathBuf> {
    let mut files = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
        .collect::<Vec<_>>();
    files.sort();
    files
}

// Source guards are recursive; integration-test enumeration remains top-level.
fn source_modules(root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            paths.extend(source_modules(&path));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            paths.push(path);
        }
    }
    paths.sort();
    paths
}

fn direct_crate_dependencies(source: &str) -> BTreeSet<String> {
    source
        .match_indices("crate::")
        .filter_map(|(index, _)| {
            let suffix = &source[index + "crate::".len()..];
            let module = suffix
                .chars()
                .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
                .collect::<String>();
            (!module.is_empty()).then_some(module)
        })
        .collect()
}

fn parent_defined_items(source: &str) -> BTreeSet<String> {
    source
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let first = words.next()?;
            let kind = if first == "pub" || first.starts_with("pub(") {
                words.next()?
            } else {
                first
            };
            if !matches!(
                kind,
                "fn" | "struct" | "enum" | "trait" | "type" | "const" | "static" | "mod"
            ) {
                return None;
            }
            let name = words
                .next()?
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect::<String>();
            (!name.is_empty()).then_some(name)
        })
        .collect()
}

#[test]
fn review_output_production_dependencies_exclude_its_callers() {
    let source = fs::read_to_string(repository_root().join("src/review_output.rs")).unwrap();
    let (production, _) = source.split_once("#[cfg(test)]\nmod tests {").unwrap();
    assert_eq!(
        direct_crate_dependencies(production),
        BTreeSet::from(["jcs".to_owned(), "machine".to_owned()])
    );
}

#[test]
fn orchestration_production_depends_on_the_adapter_trait() {
    check_orchestration_adapter(&repository_root().join("src"));
}

fn check_orchestration_adapter(root: &Path) {
    let source = fs::read_to_string(root.join("orchestration.rs")).unwrap();
    let (production, _) = source
        .split_once("#[cfg(test)]\npub(crate) mod tests {")
        .unwrap();
    assert!(!direct_crate_dependencies(production).contains("semantic"));
    for path in source_modules(&root.join("orchestration")) {
        let source = fs::read_to_string(&path).unwrap();
        let production = source.split("#[cfg(test)]\nmod tests {").next().unwrap();
        let directory = path.parent().unwrap();
        let parent = if path.file_name().unwrap() == "mod.rs" {
            directory.parent().unwrap()
        } else {
            directory
        };
        let owner = parent.with_extension("rs");
        let owner = if owner.is_file() {
            owner
        } else {
            parent.join("mod.rs")
        };
        let defined = parent_defined_items(&fs::read_to_string(owner).unwrap());
        for (offset, _) in production.match_indices("super::") {
            let suffix = &production[offset + "super::".len()..];
            let names = if let Some(block) = suffix.strip_prefix('{') {
                block
                    .split_once('}')
                    .unwrap()
                    .0
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            } else {
                vec![
                    suffix
                        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                        .next()
                        .unwrap(),
                ]
            };
            for name in names {
                assert!(
                    defined.contains(name),
                    "{} imports non-parent item {name} through super",
                    path.display()
                );
            }
        }
        assert!(
            !direct_crate_dependencies(production).contains("semantic"),
            "{} bypasses the orchestration adapter",
            path.display()
        );
    }
}

#[test]
fn source_module_dependencies_match_the_approved_graph() {
    let approved = BTreeMap::from([
        // Parent-owned SQL/ledger bindings; no concrete semantic adapter.
        (
            "orchestration/approval",
            &[
                "audit",
                "domain",
                "interaction",
                "ledger",
                "operation",
                "run",
                "snapshot",
                "jcs",
                "machine",
                "session_policy",
                "workspace",
            ][..],
        ),
        // Confidential projection reads checked owner captures, history and artifacts.
        (
            "orchestration/checked_state",
            &[
                "assignment_queue",
                "audit",
                "collaboration",
                "darwin",
                "delegation_identity",
                "engagement",
                "ledger",
                "ledger_generation",
                "operation",
                "run",
                "scheduler",
                "trusted_host",
                "domain",
                "jcs",
                "machine",
                "session_policy",
            ][..],
        ),
        // Worker projects primitives; this owner retains task/claim/recovery checks.
        (
            "orchestration/policy_task",
            &[
                "assignment_queue",
                "collaboration",
                "controller",
                "domain",
                "operation",
                "run",
                "scheduler",
                "snapshot",
                "storage_observation",
                "turn",
                "workspace",
                "jcs",
                "machine",
                "primary_tool",
                "session_policy",
            ][..],
        ),
        (
            "collaboration",
            &[
                "artifact", // Source artifact quota and bounded content reads.
                "assignment_queue",
                "audit",  // Original target acceptance and terminal evidence.
                "darwin", // Same-UID private notification transport.
                "delegation_identity",
                "domain",
                "engagement",
                "jcs",
                "ledger", // Verified retained target history.
                "machine",
                "operation", // Original Submit namespace for recovery.
                "orchestration",
                "run",
                "scheduler",
                "semantic",
                "session_policy",
                "snapshot", // Current Run and accepted configuration fences.
                "storage_observation",
                "successor_capture",
                "task_request",
                "turn",
                "worker", // Central-runtime observation-only waiter hint.
                "workspace",
                "writer", // Blocking-wait admission revalidates writer ownership.
            ][..],
        ),
        (
            "research_group",
            &[
                "assignment_queue",
                "caller_scope",
                "controller",
                "delegation_identity",
                "group_quota",
                "jcs",
                "operation",
                "orchestration",
                "paths",
                "provisioning",
                "result_component_artifact",
                "run",
                "specialist_facade",
                "storage_observation",
                "successor_capture",
                "task_request",
                "workspace",
            ][..],
        ),
        // Delegation child owns exact merge/publication and synthesis input
        // binding; ordinary assignment and execution retain their existing owners.
        (
            "research_merge",
            &[
                "artifact", // Genuine Run quota lease and immutable output reads.
                "caller_scope",
                "controller",
                "delegation_identity",
                "domain",
                "host_invocation",
                "operation",
                "orchestration",
                "result_component_artifact", // Authentic original Result/manifest proof.
                "review", // AssignmentSource target-selection type; no review execution.
                "run",
                "specialist_facade",
                "storage_observation",
                "structured_findings",
                "successor_capture",
                "task_request",
                "turn", // Shared complete-input limit before acceptance and on recovery.
                "workspace",
            ][..],
        ),
        (
            "specialist_stdio",
            &[
                "consumer_bridge",
                "native_consumer",
                "darwin",
                "delegation_service",
                "engagement",
                "host_invocation",
                "jcs",
                "machine",
                "paths",
                "specialist_facade",
                "trusted_host",
            ][..],
        ),
        (
            "specialist_facade",
            &[
                "native_preparation",
                "consumer_bridge",
                "native_consumer",
                "podway_client",
                "caller_scope",
                "controller",
                "delegation_identity",
                "delegation_service",
                "engagement",
                "host_invocation",
                "machine",
                "operation",
                "paths",
                "semantic",
                "source_binding",
                "task_request",
                "trusted_host",
            ][..],
        ),
        (
            "managed_specialist",
            &[
                "projection",
                "native_preparation",
                "consumer_bridge",
                "native_consumer",
                "podway_client",
                "projection",
                "workspace",
                "controller",
                "delegation_identity",
                "delegation_service",
                "host_invocation",
                "machine",
                "operation",
                "paths",
                "primary_tool",
                "run",
                "semantic",
                "specialist_facade",
                "trusted_host",
                "turn",
            ][..],
        ),
        (
            "scheduler",
            &[
                "assignment_queue",
                "caller_scope",
                "collaboration",
                "controller",
                "darwin",
                "delegation_identity",
                "delegation_service",
                "domain",
                "engagement",
                "gateway_socket",
                "jcs",
                "machine",
                "operation",
                "orchestration",
                "paths",
                "run",
                "semantic",
                "snapshot",
                "successor_capture",
                "worker", // Reuse the private per-UID socket-root validation.
                "workspace",
            ][..],
        ),
        (
            "assignment_queue",
            &["scheduler", "engagement", "jcs", "machine", "orchestration"][..],
        ),
        ("app_server", &["darwin", "jcs", "turn"][..]),
        (
            "trusted_host",
            &[
                "native_consumer",
                "caller_scope",
                "controller",
                "darwin",
                "delegation_identity",
                "delegation_service",
                "domain",
                "engagement",
                "host_invocation",
                "jcs",
                "machine",
                "orchestration",
                "paths",
                "run",
                "semantic",
                "session_policy",
                "source_binding",
                "workspace",
                "managed_specialist",
            ][..],
        ),
        (
            "host_invocation",
            &[
                "native_preparation",
                "caller_scope",
                "darwin",
                "delegation_identity",
                // Group mirror recovery verifies the original protected acceptance.
                "delegation_service",
                "engagement",
                "jcs",
                "machine",
                "operation",
                // Test-only: cold concurrent access under an isolated home.
                "paths",
                "trusted_host",
                "specialist_facade",
                "native_consumer", // Test-only original-owner witness controls.
            ][..],
        ),
        (
            "source_binding",
            &[
                "assignment_queue", // Test-only original queued execution.
                "scheduler",        // Test-only claim/dispatch reservation.
                "profile",
                // Test-only native completion, managed connection and legacy controls.
                "app_server",
                "managed_specialist",
                "primary_tool",
                "turn",
                "conformance",
                "writer",
                "caller_scope",
                "controller",
                "darwin",
                "delegation_identity",
                "delegation_service",
                "direct_review_target",
                "domain",
                "engagement",
                "global_runtime",
                "host_invocation",
                "instructions",
                "jcs",
                "machine",
                "operation",
                "orchestration",
                "paths",
                "provisioning",
                "review",
                "run",
                "semantic",
                "successor_capture",
                "task_request",
                "trusted_host",
                "workspace",
                "specialist_facade",
                "specialist_stdio",
                "ledger",
                "snapshot",
                "consumer_bridge",
                "native_consumer",
                "podway_client",
                "storage_observation",
            ][..],
        ),
        // Protected native consumer setup/claims, source composition and the
        // pinned public child invocation are separate owned boundaries.
        (
            "native_preparation",
            &[
                "turn",
                "worker",
                "semantic",
                "projection",
                "caller_scope",
                "consumer_bridge",
                "controller",
                "delegation_identity",
                "delegation_service",
                "domain",
                "engagement",
                "host_invocation",
                "instructions",
                "jcs",
                "machine",
                "managed_specialist",
                "markdown_policy",
                "native_consumer",
                "operation",
                "orchestration",
                "paths",
                "primary_tool",
                "provisioning",
                "run",
                "source_binding",
                "specialist_facade",
                "storage_observation",
                "trusted_host",
                "workspace",
            ][..],
        ),
        (
            "native_consumer",
            &[
                "specialist_facade",
                "darwin",
                "delegation_identity",
                "engagement",
                "host_invocation",
                "jcs",
                "machine",
                "paths",
                "podway_client",
                "run",
                "trusted_host",
                "turn",
                "workspace",
                "delegation_service",
            ][..],
        ),
        (
            "consumer_bridge",
            &[
                "native_preparation",
                "host_invocation",
                "specialist_facade",
                "caller_scope",
                "delegation_identity",
                "delegation_service",
                "engagement",
                "machine",
                "native_consumer",
                "podway_client",
                "review_output",
                "specialist",
                "trusted_host",
            ][..],
        ),
        (
            "podway_client",
            &["machine", "native_consumer", "trusted_host"][..],
        ),
        ("codex_socket", &[][..]),
        ("global_runtime_observation", &["workspace"][..]),
        ("audit", &["event", "jcs"][..]),
        (
            "operation",
            &["audit", "delegation_identity", "domain", "jcs", "turn"][..],
        ),
        (
            "interaction",
            &[
                "operation",
                "public_resolution",
                "audit",
                "domain",
                "ledger",
                "machine",
                "snapshot",
                "jcs",
                "projection",
                "run",
                "interaction_payload",
                "workspace",
            ][..],
        ),
        (
            "interaction_payload",
            &[
                "domain",
                "interaction",
                "machine",
                "workspace",
                "successor_interaction",
            ][..],
        ),
        (
            "public_resolution",
            &["interaction", "audit", "domain", "jcs", "operation", "turn"][..],
        ),
        ("successor_interaction", &["interaction", "turn"][..]),
        ("cli", &[][..]),
        (
            "conformance",
            &[
                "audit",
                "domain",
                "event",
                "fault",
                "jcs",
                "ledger",
                "ledger_generation",
                "machine",
                "operation",
                "projection",
                "turn",
                "workspace",
                "writer",
            ][..],
        ),
        (
            "controller",
            &[
                "trusted_host",
                "scheduler",
                "darwin",
                "domain",
                "jcs",
                "ledger",
                "machine",
                "paths",
                "projection",
                "run",
                "workspace",
            ][..],
        ),
        ("darwin", &["providers"][..]),
        (
            // `paths` is test-only: the unit-test capture fixture builds a home.
            "delegation_identity",
            &["domain", "instructions", "jcs", "machine", "paths"][..],
        ),
        ("domain", &[][..]),
        (
            "direct_review",
            &[
                "controller",
                "direct_review_target",
                "domain",
                "engagement",
                "global_runtime",
                "jcs",
                "machine",
                "projection",
                "review",
                "review_output",
                "review_recovery",
                "run",
                "semantic",
                "specialist",
                "task_request",
                "turn",
                "workspace",
            ][..],
        ),
        (
            "direct_review_target",
            &[
                "jcs",
                "machine",
                "review",
                "review_target",
                "turn",
                "workspace",
            ][..],
        ),
        (
            "engagement",
            &[
                "scheduler",
                "collaboration",
                "assignment_queue",
                "controller",
                "delegation_identity",
                "domain",
                "global_runtime",
                "instructions",
                "jcs",
                // `ledger_generation` is test-only: the maintenance-lease test
                // holds the exclusive lease a maintenance owner would hold.
                "ledger_generation",
                "machine",
                // `orchestration` is test-only: the generation-gate test asserts
                // that the observer and the store refuse an unsupported schema.
                "orchestration",
                // `operation` verifies the retained mutation envelope a lookup
                // relies on (its only producer owns the closed envelope shape).
                "operation",
                "review_output",
                "run",
                "specialist",
                "storage_observation",
                "task_request",
                "workspace",
                "structured_findings",
            ][..],
        ),
        (
            "external_engagement",
            &[
                "cli",
                "controller",
                "darwin",
                "direct_review",
                "direct_review_target",
                "domain",
                "engagement",
                "global_runtime",
                "instructions",
                "jcs",
                "machine",
                "paths",
                "review",
                "review_output",
                "run",
                "semantic",
                "specialist",
                "task_request",
                "turn",
                "worker",
                "workspace",
                "writer",
            ][..],
        ),
        (
            "event",
            &[
                "interaction",
                "audit",
                "domain",
                "jcs",
                "projection",
                "workspace",
            ][..],
        ),
        ("fault", &[][..]),
        (
            "global_profile",
            &["darwin", "jcs", "machine", "paths", "profile", "workspace"][..],
        ),
        (
            "global_runtime",
            &[
                "darwin",
                // `direct_review` is test-only: compare new review admission
                // with the retained pin's qualification.
                "direct_review",
                "global_profile",
                // `instructions` is test-only: compare new discovery admission
                // with the retained pin's qualification.
                "instructions",
                "jcs",
                "machine",
                "paths",
                "profile",
                "workspace",
            ][..],
        ),
        ("jcs", &[][..]),
        (
            "instructions",
            &[
                "darwin",
                "domain",
                "fault",
                "global_runtime",
                "jcs",
                "machine",
                "paths",
                "run",
                "semantic",
                "workspace",
            ][..],
        ),
        (
            "markdown_policy",
            &[
                "cli",
                "darwin",
                "domain",
                "engagement",
                "global_runtime",
                "instructions",
                "jcs",
                "machine",
                "orchestration",
                "paths",
                "profile",
                "run",
                "semantic",
                "specialist_policy",
                "workspace",
            ][..],
        ),
        (
            "ledger",
            &[
                "interaction",
                "audit",
                "darwin",
                "event",
                "fault",
                "jcs",
                "machine",
                "projection",
                "workspace",
                "ledger_generation",
            ][..],
        ),
        (
            "ledger_generation",
            &[
                "audit",
                "darwin",
                "jcs",
                "ledger",
                "maintenance",
                "workspace",
            ][..],
        ),
        (
            "maintenance",
            &[
                "result_component_artifact",
                "audit",
                "cli",
                "conformance",
                "controller",
                "darwin",
                "domain",
                "engagement",
                "fault",
                "global_profile",
                "global_runtime",
                "jcs",
                "ledger",
                "ledger_generation",
                "machine",
                "mutation_admission",
                "paths",
                "run",
                "storage_observation",
                "turn",
                "worker",
                "workspace",
                "writer",
            ][..],
        ),
        ("machine", &["paths", "workspace"][..]),
        (
            "process_inventory",
            &["codex_socket", "darwin", "jcs", "machine"][..],
        ),
        (
            "mutation_admission",
            &["audit", "domain", "fault", "jcs", "ledger", "machine"][..],
        ),
        // The orchestration regression fixture also checks the production
        // semantic adapter's pre-effect retirement boundary. Production
        // orchestration depends on its adapter trait, not the semantic module.
        (
            "orchestration",
            &[
                // These fixture edges exercise the
                // public adapters, original journals and task/Turn projections.
                // Production remains behind the adapter trait checked above.
                "conformance",
                "event",
                "gateway",
                "gateway_observation",
                // Test-only: project the same captured Caller approval view into v2.
                "gateway_projection",
                "gateway_projection_v2",
                "gateway_service",
                "interaction",
                "ledger",
                "operation",
                "paths",
                "protocol",
                "public_resolution",
                "scheduler",
                "snapshot",
                "turn",
                "collaboration",
                "assignment_queue",
                "artifact",
                "audit",
                // `caller_scope` is test-only: the successor fixture writes one
                // caller-scope accounting row so the close byte-identity test
                // covers that table.
                "caller_scope",
                "controller",
                "darwin",
                "domain",
                "engagement",
                // Production verifies accepted Controller group/synthesis origins;
                // concrete execution still stays behind the adapter trait.
                "delegation_service",
                // Test-only: seal the original historical policy binding before acceptance.
                "global_runtime",
                "jcs",
                "ledger_generation",
                "machine",
                "maintenance",
                "markdown_policy",
                "primary_tool",
                "run",
                "semantic",
                "session_policy",
                "specialist_policy",
                "storage_observation",
                "workspace",
                // Test-only: retain the writable member's original birth authority.
                "writer",
            ][..],
        ),
        (
            "primary_bridge",
            &["machine", "orchestration", "semantic"][..],
        ),
        (
            "primary_tool",
            &[
                "collaboration",
                "native_consumer",
                "machine",
                "specialist_facade",
            ][..],
        ),
        ("mcp_review", &[][..]),
        (
            "mcp_review_server",
            &["machine", "mcp_review", "semantic"][..],
        ),
        (
            "profile",
            &[
                "app_server",
                "codex_socket",
                "controller",
                "darwin",
                "global_profile",
                "global_runtime",
                "jcs",
                "machine",
                "paths",
                "process_inventory",
                "workspace",
            ][..],
        ),
        (
            "caller_scope",
            &[
                "native_preparation",
                "delegation_identity",
                "delegation_service", // Standalone Controller group accounting proof.
                // `engagement` is test-only: the accounting fixture opens one
                // generation-6 database through the real store.
                "engagement",
                "machine",
            ][..],
        ),
        (
            "delegation_service/accepted_readback",
            &[
                "delegation_identity",
                "delegation_service",
                "engagement",
                "jcs",
                "machine",
                "operation",
                "orchestration",
                "result_component_artifact",
                "scheduler",
                "storage_observation",
                "successor_capture",
            ][..],
        ),
        (
            "delegation_service",
            &[
                "native_preparation",
                "controller",
                // Test-only: native group/merge/synthesis binding and managed Worker authority.
                "host_invocation",
                "managed_specialist",
                "primary_tool",
                "group_quota", // Test-only: immutable source capture charges across merge placement.
                "result_component_artifact",
                "specialist_facade",
                "scheduler",
                "worker", // Test-only production contract selection and dispatcher qualification.
                "gateway_v2", // Test-only: managed result wire projection.
                "protocol", // Test-only: unchanged public-v2 result messages.
                // Native component qualification owns its test-only server group census.
                "darwin",
                "semantic",
                "projection",
                "collaboration",
                "artifact", // Test-only source-owned collaboration result reads.
                "assignment_queue",
                "caller_scope",
                "delegation_identity",
                // Direct-target metadata is admitted before a successor intent.
                "direct_review_target",
                "domain",
                "engagement",
                // Immutable member bindings and their admission qualification live here.
                "global_runtime",
                "instructions",
                "jcs",
                "machine",
                "maintenance", // Test-only: original ordinary lookup is a retirement last consumer.
                "operation",
                // `orchestration` carries the managed Broker services: the
                // session authority, the Broker execution rows and their reads
                // live in the orchestration extension.
                "orchestration",
                // `paths` is test-only: the unit fixture builds an instruction
                // home so one provisioning selection can be captured.
                "paths",
                // Test-only genuine qualified admission snapshots.
                "profile",
                "provisioning",
                "review",
                "run",
                // `specialist_policy` is test-only: the managed fixture prepares a
                // session with a legacy Specialist Policy record.
                "specialist_policy",
                "storage_observation",
                "successor_capture",
                "task_request",
                "workspace",
                "writer",
                "ledger",
                "snapshot",
                "audit",
                "trusted_host",
                "turn",
            ][..],
        ),
        (
            "successor_capture",
            &[
                "delegation_identity",
                "host_invocation", // Check the original public task body before projection.
                // `instructions` is test-only: the fixture builds one closed
                // instruction snapshot to admit a configuration.
                "instructions",
                "jcs",
                "machine",
                // `review` is test-only: the fixture supplies a target selection.
                "review",
                "review_target",
                "run",
                "task_request",
            ][..],
        ),
        (
            "provisioning",
            &[
                "delegation_identity",
                "global_runtime",
                "instructions",
                "jcs",
                "machine",
                // `paths` is test-only: the unit fixture builds an instruction
                // home to capture one provisioning selection.
                "paths",
                "run",
                "workspace",
            ][..],
        ),
        ("projection", &["audit", "domain", "jcs"][..]),
        ("protocol", &[][..]),
        ("providers", &[][..]),
        ("paths", &[][..]),
        // The one-shot review coordinator is the product composition boundary:
        // it binds trusted adapter state, the durable engagement, and the
        // ordinary semantic Run service without moving product logic into main.
        (
            "review",
            &[
                "cli",
                "controller",
                "direct_review",
                "domain",
                "engagement",
                "global_runtime",
                "instructions",
                "machine",
                "paths",
                "profile",
                "review_output",
                "review_recovery",
                "review_target",
                "run",
                "semantic",
                "specialist",
                "task_request",
                "workspace",
            ][..],
        ),
        (
            "review_recovery",
            &[
                "cli",
                "controller",
                "domain",
                "direct_review",
                "direct_review_target",
                "engagement",
                "global_runtime",
                "instructions",
                "jcs",
                "machine",
                "paths",
                "profile",
                "review",
                "review_target",
                "run",
                "semantic",
                "specialist",
                "workspace",
            ][..],
        ),
        // The immutable target coordinator owns source capture and settlement
        // while delegating canonical workspace discovery and Machine errors to
        // their existing authorities.
        (
            "review_target",
            &["cli", "machine", "paths", "workspace"][..],
        ),
        // `run` names `projection` because the Run record and the Run's
        // durable state projection are two halves of the same durable state: an
        // observer that may not take the ledger still has to read the
        // projection the ledger commits beside the manifest.
        (
            "run",
            &[
                "audit",
                "darwin",
                "delegation_identity",
                "domain",
                "global_runtime",
                "instructions",
                "jcs",
                "ledger",
                "ledger_generation",
                "machine",
                "markdown_policy",
                "operation",
                // `profile` is test-only: the unit-test helper builds a binding.
                "profile",
                "projection",
                "workspace",
            ][..],
        ),
        (
            "runtime",
            &[
                "global_profile",
                "global_runtime",
                "instructions",
                "jcs",
                "machine",
                "paths",
                "protocol",
            ][..],
        ),
        // `semantic` composes Run startup and operations; `collaboration`
        // composes accepted exchanges, results and waits through their owners.
        // Adapters (cli, main) still carry no product logic.
        (
            "semantic",
            &[
                "native_preparation",
                "result_component_artifact",
                "collaboration",
                "scheduler",
                "public_resolution",
                "app_server",
                "assignment_queue",
                "artifact",
                "audit",
                "caller_scope",
                "cli",
                "codex_socket",
                "conformance",
                "controller",
                "darwin",
                "delegation_identity",
                "delegation_service",
                "direct_review_target",
                "domain",
                "engagement",
                "event",
                "global_profile",
                "global_runtime",
                "instructions",
                "interaction",
                "interaction_payload",
                "jcs",
                "ledger",
                "machine",
                "markdown_policy",
                "mutation_admission",
                "operation",
                "orchestration",
                "paths",
                // The locator-missing owned-cleanup proof enumerates this
                // Run's retained process-inventory registrations.
                "process_inventory",
                "profile",
                "projection",
                "protocol",
                // `provisioning` is test-only here: the delegated-start
                // regression publishes its Run from a retained snapshot.
                "provisioning",
                // Original executable identity and typed review-drift preflight.
                "review",
                // The changes producer bounds one Turn's file-change
                // snapshots by the existing workspace enumeration bound.
                "review_target",
                "run",
                "runtime",
                "session_policy",
                "snapshot",
                "specialist",
                "specialist_policy",
                "storage_observation",
                "successor_capture",
                // `task_request` names the accepted task the checked-output
                // validator (specialist::parse_reviewer_output_v3) inspects.
                "task_request",
                "timeline",
                // Test-only: protected administrative recovery of real member fixtures.
                "trusted_host",
                "turn",
                "worker",
                "workspace",
                "writer",
                "ledger_generation",
                "pool",
                "structured_findings",
            ][..],
        ),
        (
            "specialist",
            &[
                "domain",
                "jcs",
                "machine",
                "review_output",
                "run",
                "task_request",
                "turn",
                "workspace",
            ][..],
        ),
        (
            "task_request",
            &["machine", "review_output", "structured_findings"][..],
        ),
        (
            "structured_findings",
            &["delegation_identity", "jcs", "machine"][..],
        ),
        (
            "review_output",
            &["jcs", "machine", "specialist", "task_request"][..],
        ),
        (
            "specialist_policy",
            &[
                "darwin",
                "domain",
                "jcs",
                "machine",
                "paths",
                "run",
                "semantic",
                "workspace",
            ][..],
        ),
        (
            "turn",
            &[
                "native_preparation",
                "run",
                "collaboration",
                "public_resolution",
                "consumer_bridge",
                "native_consumer",
                "app_server",
                "audit",
                "conformance",
                "darwin",
                "direct_review",
                "direct_review_target",
                "domain",
                "fault",
                "interaction_payload",
                "jcs",
                "ledger",
                "machine",
                "operation",
                "primary_tool",
                "worker",
                "workspace",
                "writer",
                "host_invocation",
                "specialist_facade",
                "event",
                "successor_interaction",
            ][..],
        ),
        (
            "workspace",
            &["darwin", "jcs", "machine", "paths", "writer"][..],
        ),
        // Writer status reuses the semantic Run status projection; its local
        // regression fixture also constructs a RunStateProjection directly.
        (
            "writer",
            &[
                "audit",
                "darwin",
                "domain",
                "jcs",
                "machine",
                "projection",
                "run",
                "semantic",
                "workspace",
            ][..],
        ),
        // `worker` names `controller` because ADR-016 makes the hidden worker
        // the authoritative consumer of a Controller credential: it rereads
        // the descriptor it received over SCM_RIGHTS and revalidates it under
        // the Run mutation lock immediately before effects. That authority
        // check cannot be delegated to a caller without giving up the very
        // property the ADR requires.
        //
        // It names `profile` because a foreign-thread observation "is never a
        // Run event and uses the separate profile diagnostic schema"
        // (docs/specs/README.md), and the worker is the only place that knows both the
        // Run's coordinator and, from its own session bootstrap, which Runtime
        // Profile the Run is pinned to. `turn` stays free of it: the
        // coordinator is handed a writer rather than deriving one.
        // It names `writer` to refresh the current holder after control-socket
        // recovery, within the worker's writer-authority transaction boundary.
        // It names `orchestration` to check durable Session close intent before
        // fresh Primary Turn admission, after accepted receipt replay.
        // It names `semantic` and `snapshot` to restore retained close
        // reconciliation before a paused Primary takes a lifecycle no-op path.
        (
            "worker",
            &[
                "scheduler",
                // Reservation regression uses owning queue and caller fixtures.
                "assignment_queue",
                "caller_scope",
                "collaboration",
                "operation",
                "public_resolution",
                "native_consumer",
                "app_server",
                "artifact",
                "audit",
                "codex_socket",
                "conformance",
                "controller",
                "darwin",
                // `delegation_service` names the settlement owner type: a member
                // Run settles through whichever aggregate its own manifest names.
                "delegation_service",
                "direct_review",
                "direct_review_target",
                "domain",
                "engagement",
                "event",
                "external_engagement",
                "fault",
                // Recovery tests verify membership retirement through its owner.
                "global_runtime",
                "jcs",
                "ledger",
                "machine",
                "mutation_admission",
                "orchestration",
                "paths",
                "primary_bridge",
                "primary_tool",
                "process_inventory",
                "profile",
                "projection",
                "providers",
                // Quota regressions verify publication bytes through their owner.
                "result_component_artifact",
                "review",
                "run",
                "semantic",
                "snapshot",
                // `successor_capture` binds a generation-4 dispatched Turn to the
                // preserved accepted bytes.
                "successor_capture",
                "task_request",
                "turn",
                "workspace",
                "writer",
                "managed_specialist",
                "interaction",
            ][..],
        ),
        (
            "gateway",
            &[
                "scheduler",
                "darwin",
                "gateway_socket",
                "machine",
                "paths",
                "protocol",
                "gateway_v2",
                "gateway_observation_v2",
                // Test-only: the deadline backend implements workspace/Controller traits.
                "controller",
                "workspace",
            ][..],
        ),
        (
            "gateway_observation_v2",
            &[
                "gateway",
                "gateway_observation",
                "gateway_service",
                "gateway_v2",
                "machine",
                "protocol",
                "jcs",
                "ledger",
                "timeline",
            ][..],
        ),
        (
            "gateway_socket",
            &["darwin", "machine", "paths", "protocol"][..],
        ),
        (
            "gateway_projection",
            &[
                "domain",
                "machine",
                "profile",
                "protocol",
                "run",
                "runtime",
                "snapshot",
                "turn",
                "worker",
                "workspace",
                "writer",
            ][..],
        ),
        (
            "gateway_event",
            &[
                "audit",
                "domain",
                "event",
                "gateway_projection",
                "machine",
                "protocol",
                "workspace",
                "writer",
            ][..],
        ),
        (
            "gateway_service",
            &[
                "controller",
                "darwin",
                "domain",
                "gateway",
                "gateway_event",
                "gateway_observation",
                "gateway_projection",
                "jcs",
                "ledger",
                "machine",
                "orchestration",
                "paths",
                "profile",
                "protocol",
                "run",
                "runtime",
                "semantic",
                "snapshot",
                "timeline",
                "turn",
                "worker",
                "workspace",
                "writer",
                "gateway_v2",
            ][..],
        ),
        (
            "snapshot",
            &[
                // Test-only public projection checks use the captured observation.
                "gateway_projection",
                "gateway_projection_v2",
                "protocol",
                "interaction",
                "orchestration",
                "audit",
                "controller",
                "darwin",
                "domain",
                "jcs",
                "ledger",
                "machine",
                "projection",
                "run",
                "semantic", // Test-only authentic member fixture for closed policy diagnostics.
                "turn",
                "worker",
                "workspace",
                "writer",
                "ledger_generation",
            ][..],
        ),
        (
            "gateway_observation",
            &[
                "artifact",
                "controller",
                "domain",
                "event",
                "gateway",
                "gateway_event",
                "gateway_projection",
                "interaction",
                "interaction_payload",
                "ledger",
                "machine",
                "protocol",
                "semantic",
                "snapshot",
                "turn",
                "workspace",
            ][..],
        ),
        (
            "result_component_artifact",
            &[
                "artifact",
                "controller",
                "delegation_identity",
                "delegation_service",
                "engagement",
                "machine",
                "operation",
                "orchestration",
                "specialist",
                "trusted_host",
                "workspace",
            ][..],
        ),
        (
            "group_quota",
            &[
                "artifact",
                "darwin",
                "delegation_identity",
                "engagement",
                "jcs",
                "machine",
                "run",
                "workspace",
            ][..],
        ),
        (
            "artifact",
            &[
                "result_component_artifact",
                "delegation_service", // Retained merge charges share the Run artifact quota.
                "audit",
                "controller",
                "darwin",
                "engagement",
                "event",
                "interaction_payload",
                "jcs",
                "ledger",
                "machine",
                "orchestration",
                "snapshot",
                "workspace",
                "group_quota",
            ][..],
        ),
        (
            "timeline",
            &[
                "artifact",
                "controller",
                "event",
                "gateway_event",
                "gateway_projection",
                "interaction",
                "ledger",
                "machine",
                "protocol",
                "snapshot",
                "turn",
            ][..],
        ),
        (
            "migration",
            &[
                "cli",
                "darwin",
                "instructions",
                "jcs",
                "machine",
                "markdown_policy",
                "paths",
                "specialist_policy",
                "workspace",
            ][..],
        ),
        (
            "session_policy",
            &["machine", "markdown_policy", "run", "specialist_policy"][..],
        ),
        (
            "storage_observation",
            &[
                "darwin",
                "jcs",
                "ledger",
                "ledger_generation",
                "machine",
                "run",
                "workspace",
            ][..],
        ),
        (
            "gateway_v2",
            &[
                "delegation_service",
                "public_resolution",
                "controller",
                "darwin",
                "delegation_identity",
                "domain",
                "gateway",
                "gateway_event",
                "gateway_observation",
                "gateway_projection",
                "gateway_projection_v2",
                "gateway_service",
                "global_runtime",
                "instructions",
                "jcs",
                "ledger",
                "machine",
                "markdown_policy",
                "operation",
                "orchestration",
                "paths",
                "profile",
                "protocol",
                "provisioning",
                "run",
                "semantic",
                "snapshot",
                "turn",
                "worker",
                "workspace",
                "writer",
                "audit",
                "interaction",
                "interaction_payload",
                "successor_interaction",
            ][..],
        ),
        (
            "gateway_broker",
            &["delegation_identity", "delegation_service", "semantic"][..],
        ),
        (
            "gateway_groups",
            &[
                "semantic",
                "delegation_service",
                "delegation_identity",
                "engagement", // Read the authenticated merge's retained publication.
                "jcs",
            ][..],
        ),
        (
            "gateway_projection_v2",
            &[
                "delegation_identity",
                "domain",
                "gateway_projection",
                "machine",
                "protocol",
                "run",
                "snapshot",
                "turn",
                "workspace",
                "writer",
            ][..],
        ),
        (
            "pool",
            &["darwin", "domain", "machine", "paths", "run", "workspace"][..],
        ),
        ("worker/companion", &["darwin", "global_runtime"][..]),
    ]);

    check_source_dependencies(&repository_root().join("src"), &approved);
}

fn check_source_dependencies(root: &Path, approved: &BTreeMap<&str, &[&str]>) {
    let mut observed_modules = BTreeSet::new();
    for path in source_modules(root) {
        let relative = path.strip_prefix(root).unwrap().with_extension("");
        let module = relative.to_str().unwrap();
        if matches!(module, "lib" | "main") {
            continue;
        }
        observed_modules.insert(module.to_owned());
        let source = fs::read_to_string(&path).unwrap();
        if module == "delegation_service" {
            let (production, _) = source.split_once("pub(crate) mod tests {").unwrap();
            let production = direct_crate_dependencies(production);
            for test_only in ["gateway_v2", "protocol"] {
                assert!(
                    !production.contains(test_only),
                    "delegation service production depends on its wire test consumer {test_only}"
                );
            }
        }
        if module == "source_binding" {
            let (production, _) = source.split_once("#[cfg(test)]\nmod tests {").unwrap();
            let production = direct_crate_dependencies(production);
            for test_only in [
                "app_server",
                "managed_specialist",
                "primary_tool",
                "turn",
                "profile",
                "assignment_queue",
                "scheduler",
            ] {
                assert!(
                    !production.contains(test_only),
                    "source binding production depends on its native test consumer {test_only}"
                );
            }
        }
        let observed = direct_crate_dependencies(&source);
        let expected = approved
            .get(module)
            .unwrap_or_else(|| panic!("missing architecture entry for {module}"))
            .iter()
            .map(|dependency| (*dependency).to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            observed,
            expected,
            "direct module dependencies changed for {}",
            path.display()
        );
    }
    assert_eq!(
        observed_modules,
        approved.keys().map(|name| (*name).to_owned()).collect(),
        "approved architecture entries must match the recursive source inventory"
    );
}

#[test]
fn unsafe_implementation_remains_inside_the_darwin_adapter() {
    check_unsafe_boundary(&repository_root().join("src"));
}

fn check_unsafe_boundary(root: &Path) {
    for path in source_modules(root) {
        if path == root.join("darwin.rs") {
            continue;
        }
        let source = fs::read_to_string(&path).unwrap();
        for forbidden in ["unsafe {", "unsafe fn ", "unsafe impl ", "unsafe trait "] {
            assert!(
                !source.contains(forbidden),
                "{} contains {forbidden:?}; unsafe code belongs in src/darwin.rs",
                path.display()
            );
        }
    }
}

fn source_fixture(files: &[(&str, &str)], check: impl FnOnce(&Path)) {
    struct OwnedSource(PathBuf);
    impl Drop for OwnedSource {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let root =
        PathBuf::from("/tmp").join(format!("dolgorae-architecture-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    let owned = OwnedSource(fs::canonicalize(root).unwrap());
    for (name, content) in files {
        let path = owned.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    check(&owned.0);
}

#[test]
fn recursive_guards_reject_nested_dependency_and_unsafe_bypasses() {
    source_fixture(
        &[("owner/nested/leaf.rs", "use crate::jcs::canonicalize;")],
        |root| {
            let leaf = root.join("owner/nested/leaf.rs");
            assert_eq!(source_modules(root), vec![leaf.clone()]);
            let approved = BTreeMap::from([("owner/nested/leaf", &["jcs"][..])]);
            check_source_dependencies(root, &approved);
            assert!(
                std::panic::catch_unwind(|| check_source_dependencies(root, &BTreeMap::new()))
                    .is_err()
            );
            fs::write(&leaf, "use crate::semantic::CoreSemanticService;").unwrap();
            assert!(
                std::panic::catch_unwind(|| check_source_dependencies(root, &approved)).is_err()
            );
            check_unsafe_boundary(root);
            fs::write(root.join("owner/nested/darwin.rs"), "unsafe fn bypass() {}").unwrap();
            assert!(std::panic::catch_unwind(|| check_unsafe_boundary(root)).is_err());
        },
    );
}

#[test]
fn recursive_orchestration_guard_preserves_parent_and_adapter_boundaries() {
    source_fixture(
        &[
            (
                "orchestration.rs",
                "mod layer;\n#[cfg(test)]\npub(crate) mod tests {}",
            ),
            (
                "orchestration/layer.rs",
                "pub struct Owned;\nuse crate::jcs::canonicalize as Alias;\nmod nested;",
            ),
            ("orchestration/layer/nested.rs", "use super::Owned;"),
        ],
        |root| {
            check_orchestration_adapter(root);
            let nested = root.join("orchestration/layer/nested.rs");
            for source in [
                "use super::Alias;",
                "use super::*;",
                "use crate::semantic::CoreSemanticService;",
            ] {
                fs::write(&nested, source).unwrap();
                assert!(
                    std::panic::catch_unwind(|| check_orchestration_adapter(root)).is_err(),
                    "{source}"
                );
            }
        },
    );
}

#[test]
fn task_038_activates_one_global_runtime_selector() {
    let root = repository_root().join("src");
    let main = fs::read_to_string(root.join("main.rs")).unwrap();
    let semantic = fs::read_to_string(root.join("semantic.rs")).unwrap();
    assert!(main.contains("require_generation"));
    assert!(main.contains("execute_global"));
    assert!(semantic.contains("ResolvedGlobalProfile"));
    assert!(semantic.contains("global_profile_binding"));
}

#[test]
fn integration_tests_do_not_cross_the_external_system_boundary() {
    let forbidden = [
        "CARGO_BIN_EXE_dolgorae",
        "Command::new(\"git\")",
        "TcpListener",
        "TcpStream",
        "UdpSocket",
        "DATABASE_URL",
        "postgres://",
        "mysql://",
        "sqlite://",
        "http://",
        "https://",
    ];
    for path in rust_sources(&repository_root().join("tests")) {
        // These black-box executables belong to test-e2e and are never in INT_TESTS.
        let relative = path.strip_prefix(repository_root()).unwrap().to_path_buf();
        if [
            "tests/gateway_native.rs",
            "tests/gateway_semantic_native.rs",
            "tests/gateway_configuration_native.rs",
            "tests/gateway_interaction_native.rs",
            "tests/support/gateway_native.rs",
            "tests/support/observation_cases.rs",
            "tests/support/result_content_cases.rs",
        ]
        .iter()
        .any(|native| relative == Path::new(native))
        {
            continue;
        }
        if path
            .file_name()
            .is_some_and(|name| name == "architecture_contract.rs")
        {
            continue;
        }
        let source = fs::read_to_string(&path).unwrap();
        for marker in forbidden {
            assert!(
                !source.contains(marker),
                "{} crosses the test-int boundary with {marker:?}",
                path.display()
            );
        }
    }
}

fn collect_python_files(directory: &Path, output: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        if path.is_dir() {
            if !name.starts_with('.') && !matches!(name, "target" | "__pycache__") {
                collect_python_files(&path, output);
            }
        } else if path.extension().is_some_and(|extension| extension == "py") {
            output.push(path);
        }
    }
}

#[test]
fn python_is_limited_to_validators_fixtures_and_black_box_e2e() {
    let root = repository_root();
    let validator_root = root.join("tools/validators");
    // ADR-014 places exactly one Python fixture outside the validators: the
    // shared fake app-server, whose independence from the Rust ingest path is
    // the point of it existing.
    let fake_app_server_root = root.join("tools/fake_app_server");
    let e2e_root = root.join("tests/e2e");
    let mut files = Vec::new();
    collect_python_files(&root, &mut files);
    for path in files {
        assert!(
            path.starts_with(&validator_root)
                || path.starts_with(&fake_app_server_root)
                || path.starts_with(&e2e_root),
            "Python is limited to tools/validators, tools/fake_app_server, and tests/e2e: {}",
            path.display()
        );
        if path.starts_with(&e2e_root) || path.starts_with(&fake_app_server_root) {
            let source = fs::read_to_string(&path).unwrap();
            assert!(!source.contains("import dolgorae"));
            assert!(!source.contains("from dolgorae"));
        }
    }
}

#[test]
fn the_shared_fake_app_server_shares_no_parser_with_the_product() {
    let root = repository_root().join("tools/fake_app_server");
    let mut files = Vec::new();
    collect_python_files(&root, &mut files);
    assert!(
        files.iter().any(|path| path.ends_with("jsonlite.py")),
        "the fake app-server must carry its own strict JSON reader"
    );
    for path in files {
        if path.ends_with("jsonlite.py") {
            continue;
        }
        let source = fs::read_to_string(&path).unwrap();
        for forbidden in [
            "import json\n",
            "import json ",
            "from json import",
            "json.loads",
            "json.dumps",
        ] {
            assert!(
                !source.contains(forbidden),
                "{} reaches for the stdlib JSON module; ADR-014 requires parser diversity",
                path.display()
            );
        }
    }
}
