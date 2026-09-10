#!/usr/bin/env python3
"""Verify the public preparse bound and protected interaction authority."""
from run_native_gateway_case import run_case

if __name__ == "__main__":
    run_case("gateway_interaction_native", "preparse_bound_and_no_secret_replay_e2e")
    run_case("gateway_interaction_native", "artifact_download_preserves_bytes_and_reports_range_and_integrity_failures")
    run_case("gateway_interaction_native", "file_change_artifact_requires_current_controller")
