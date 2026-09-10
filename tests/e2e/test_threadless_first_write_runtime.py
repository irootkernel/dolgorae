#!/usr/bin/env python3
"""Run the native threadless first-write admission contract."""
from run_native_gateway_case import run_case

if __name__ == "__main__":
    run_case("gateway_configuration_native", "threadless_submit_writer_activation_e2e")
