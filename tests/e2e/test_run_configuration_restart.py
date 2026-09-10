#!/usr/bin/env python3
"""Run the native accepted configuration and process restart contract."""
from run_native_gateway_case import run_case

if __name__ == "__main__":
    run_case("gateway_configuration_native", "accepted_configuration_restart_e2e")
