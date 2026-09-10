"""Run the native public allocation reservation and response-loss scenario."""

from run_native_gateway_case import run_case

if __name__ == "__main__":
    run_case("gateway_semantic_native", "start_run_allocation_replay")
    run_case(
        "gateway_semantic_native",
        "orchestrated_start_run_pins_policy_and_replays_without_registry_source",
    )
