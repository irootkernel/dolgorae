"""Run the native public revision and accepted-response replay scenario."""

from run_native_gateway_case import run_case

if __name__ == "__main__":
    run_case("gateway_semantic_native", "observation_campaign::percentile_uses_nearest_rank_and_keeps_each_repetition")
    run_case("gateway_semantic_native", "observation_campaign::gateway_observation_reuses_sources_in_an_isolated_process")
    run_case("gateway_semantic_native", "observation_campaign::snapshot_projects_its_exact_captured_history_and_rejects_display_mutation_admission")
    run_case("gateway_semantic_native", "result_content_cli::native_result_content_cli_preserves_frozen_projection_and_retained_owner")
    run_case("gateway_semantic_native", "caller_close_cleanup_retains_terminal_history_and_checked_recovery")
    run_case("gateway_semantic_native", "native_profile_compaction_retains_original_scope_and_history")
    run_case("gateway_semantic_native", "native_profile_compaction_preserves_live_and_unknown_members")
    run_case("gateway_semantic_native", "native_profile_compaction_resumes_each_owned_crash_boundary")
    run_case("gateway_semantic_native", "public_close_retains_completed_receipt_during_membership_cleanup")
    run_case(
        "gateway_semantic_native",
        "failed_membership_release_preserves_close_and_retries_only_cleanup",
    )
    run_case(
        "gateway_semantic_native",
        "maintenance_recovers_every_switch_boundary_and_preserves_closed_run_history",
    )
    run_case("gateway_semantic_native", "event_revision_action_barrier")
    run_case("gateway_semantic_native", "public_bootstrap_and_machine_parity")
    run_case(
        "gateway_semantic_native",
        "lookup_requires_recovery_when_controller_binding_outran_reset_commit",
    )
    run_case(
        "gateway_semantic_native",
        "native_build_handoff_keeps_busy_workers_and_preserves_idle_receipts",
    )
    run_case(
        "gateway_semantic_native",
        "definitive_submit_refusal_survives_gateway_and_worker_restart",
    )
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
    run_case("gateway_semantic_native", "mixed_gateway_observes_modern_runs_without_legacy_mutation")
    run_case("gateway_semantic_native", "native_killed_worker_recovery")
    run_case("gateway_semantic_native", "native_watch_input_rejection")
    run_case("gateway_semantic_native", "native_concurrent_identical_admission")
    run_case("gateway_semantic_native", "native_resume_accepts_next_turn")
    run_case("gateway_semantic_native", "native_context_and_unavailable_method_rejection")
    run_case(
        "gateway_semantic_native",
        "public_reconcile_rejects_a_healthy_run_without_effect",
    )
