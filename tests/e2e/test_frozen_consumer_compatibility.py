#!/usr/bin/env python3
"""Run the unchanged TASK-053 and pre-extension clients against the candidate."""

from run_native_gateway_case import run_case


if __name__ == "__main__":
    run_case(
        "gateway_semantic_native",
        "frozen_generated_consumers_run_unchanged_against_candidate_and_restart",
    )
