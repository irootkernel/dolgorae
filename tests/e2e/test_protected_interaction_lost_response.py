#!/usr/bin/env python3
"""Verify protected resolution recovery across a real gateway replacement."""
from run_native_gateway_case import run_case

if __name__ == "__main__":
    run_case("gateway_interaction_native", "secret_canary_and_fault_barrier")
    run_case(
        "gateway_semantic_native",
        "native_pending_interaction_parity_after_lost_interrupt_response",
    )
