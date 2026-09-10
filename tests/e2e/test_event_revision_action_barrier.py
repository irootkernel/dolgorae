"""Run the native public revision and accepted-response replay scenario."""

from run_native_gateway_case import run_case

if __name__ == "__main__":
    run_case("gateway_semantic_native", "event_revision_action_barrier")
    run_case("gateway_semantic_native", "public_bootstrap_and_machine_parity")
    run_case("gateway_semantic_native", "native_interrupt_invalidates_interaction")
    run_case("gateway_semantic_native", "native_close_and_pause_interrupt")
    run_case("gateway_semantic_native", "native_writer_release_and_reacquire")
    run_case("gateway_semantic_native", "native_killed_worker_recovery")
    run_case("gateway_semantic_native", "native_watch_input_rejection")
    run_case("gateway_semantic_native", "native_concurrent_identical_admission")
    run_case("gateway_semantic_native", "native_resume_accepts_next_turn")
    run_case("gateway_semantic_native", "native_context_and_unavailable_method_rejection")
