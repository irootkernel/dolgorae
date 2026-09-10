#!/usr/bin/env python3
"""Backpressure terminates one Run stream while another Run remains usable."""
from run_native_gateway_case import run_case

if __name__ == "__main__":
    run_case("gateway_native", "multi_run_pressure_e2e")
