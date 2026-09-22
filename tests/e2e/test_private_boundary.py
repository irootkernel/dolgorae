#!/usr/bin/env python3
"""Run the deterministic TASK-026 public-provider acceptance matrix.

Every selected case starts the production ``dolgorae serve`` process and uses
generated public-v1 clients against its private Unix socket.  The upstream
Codex boundary is deliberately fake in this default-gate driver; the separate
opt-in live campaign owns actual pinned Codex evidence.
"""

from __future__ import annotations

import json
from pathlib import Path

from run_native_gateway_case import run_case as execute_native_case


ROOT = Path(__file__).resolve().parents[2]
CONFORMANCE = ROOT / "docs/protocol/dolgorae-grpc-conformance-v1.json"

# Each method is assigned to a case that actually invokes that RPC.  A case may
# verify additional behavior beyond the method inventory named here.
CASES: tuple[tuple[str, str, frozenset[str]], ...] = (
    (
        "gateway_semantic_native",
        "public_bootstrap_and_machine_parity",
        frozenset(
            {
                "ControllerService.VerifyController",
                "RunService.GetRun",
                "RunService.ListRuns",
                "RunService.StartRun",
                "RunService.SubmitTurn",
                "RuntimeService.GetCapabilities",
                "RuntimeService.GetProfile",
                "RuntimeService.InspectWorkspace",
                "RuntimeService.ListProfiles",
                "WriterService.GetWorkspaceWriterStatus",
            }
        ),
    ),
    (
        "gateway_configuration_native",
        "threadless_submit_writer_activation_e2e",
        frozenset({"WriterService.AcquireWriter"}),
    ),
    (
        "gateway_semantic_native",
        "native_writer_release_and_reacquire",
        frozenset({"WriterService.ReleaseWriter"}),
    ),
    (
        "gateway_semantic_native",
        "native_resume_accepts_next_turn",
        frozenset({"RunService.PauseRun", "RunService.ResumeRun"}),
    ),
    (
        "gateway_semantic_native",
        "native_interrupt_invalidates_interaction",
        frozenset(
            {
                "InteractionService.GetControllerInteraction",
                "InteractionService.ListPendingInteractions",
                "ObservationService.ListRunTimelineItems",
                "RunService.InterruptTurn",
            }
        ),
    ),
    (
        "gateway_interaction_native",
        "secret_canary_and_fault_barrier",
        frozenset(
            {
                "InteractionService.ResolveInteraction",
                "ObservationService.WatchRunEvents",
            }
        ),
    ),
    (
        "gateway_semantic_native",
        "public_reconcile_rejects_a_healthy_run_without_effect",
        frozenset({"RunService.ReconcileRun"}),
    ),
    (
        "gateway_semantic_native",
        "root_recovery_resumes_a_close_intent_committed_before_gateway_restart",
        frozenset({"RunService.RecoverRun"}),
    ),
    (
        "gateway_semantic_native",
        "public_orchestrated_session_observation_survives_gateway_restart_without_mutation",
        frozenset(
            {
                "ArtifactService.GetArtifact",
                "ArtifactService.ReadArtifactChunk",
                "OrchestrationService.GetOrchestratedSession",
                "OrchestrationService.ListOrchestratedSessionResults",
            }
        ),
    ),
    (
        "gateway_semantic_native",
        "root_close_persists_one_operation_and_survives_gateway_restart",
        frozenset({"RunService.CloseRun"}),
    ),
)


def required_methods() -> frozenset[str]:
    document = json.loads(CONFORMANCE.read_text(encoding="utf-8"))
    profile = document["consumer_profiles"]["dolgorae.gul-consumer/v1"]
    methods = frozenset(profile["required_methods"])
    if len(methods) != profile["required_method_count"]:
        raise AssertionError("consumer profile method count is inconsistent")
    return methods


def main() -> int:
    covered = frozenset(method for _, _, methods in CASES for method in methods)
    required = required_methods()
    if covered != required:
        raise AssertionError(
            "private boundary method coverage drifted: "
            f"missing={sorted(required - covered)!r} extra={sorted(covered - required)!r}"
        )
    print(
        "private_boundary deterministic campaign: production gateway and semantics; "
        "fake Codex boundary; 27 required public-v1 methods"
    )
    for target, case, _ in CASES:
        execute_native_case(target, case)
    print("private_boundary deterministic campaign passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
