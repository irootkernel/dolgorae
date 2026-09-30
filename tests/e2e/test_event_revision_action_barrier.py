"""Run the native public revision and accepted-response replay scenario."""

from run_native_gateway_case import run_case

if __name__ == "__main__":
    run_case("gateway_semantic_native", "event_revision_action_barrier")
    run_case("gateway_semantic_native", "public_bootstrap_and_machine_parity")
    run_case(
        "gateway_semantic_native",
        "orchestration_queries_reject_a_low_level_run_after_controller_authentication",
    )
    run_case(
        "gateway_semantic_native",
        "public_orchestrated_session_observation_survives_gateway_restart_without_mutation",
    )
    run_case(
        "gateway_semantic_native",
        "root_close_persists_one_operation_and_survives_gateway_restart",
    )
    run_case(
        "gateway_semantic_native",
        "root_recovery_resumes_a_close_intent_committed_before_gateway_restart",
    )
    run_case(
        "gateway_semantic_native",
        "root_close_requires_explicit_interrupt_before_committing_active_work",
    )
    run_case(
        "gateway_semantic_native",
        "root_close_preserves_unknown_primary_until_matching_history_is_terminal",
    )
    run_case(
        "gateway_semantic_native",
        "root_close_cannot_clear_unknown_effect_by_pausing_primary",
    )
    run_case(
        "gateway_semantic_native",
        "closed_run_rejects_fresh_submission_without_changing_the_terminal_history",
    )
    run_case(
        "gateway_semantic_native",
        "controller_timeline_preserves_input_and_pages_without_replay_duplicates",
    )
    run_case("gateway_semantic_native", "native_interrupt_invalidates_interaction")
    run_case("gateway_semantic_native", "native_close_and_pause_interrupt")
    run_case("gateway_semantic_native", "native_writer_release_and_reacquire")
    run_case("gateway_semantic_native", "native_killed_worker_recovery")
    run_case("gateway_semantic_native", "native_watch_input_rejection")
    run_case("gateway_semantic_native", "native_concurrent_identical_admission")
    run_case("gateway_semantic_native", "native_resume_accepts_next_turn")
    run_case("gateway_semantic_native", "native_context_and_unavailable_method_rejection")
    run_case(
        "gateway_semantic_native",
        "public_reconcile_rejects_a_healthy_run_without_effect",
    )
