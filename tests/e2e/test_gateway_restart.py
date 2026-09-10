#!/usr/bin/env python3
"""A killed/replaced foreground gateway leaves an active Run unchanged."""
from run_native_gateway_case import run_case

if __name__ == "__main__":
    run_case("gateway_native", "active_run_restart_e2e")
