#!/usr/bin/env python3
"""Run an extension's smoke.sql through the sqlink host CLI + output assertions.

THIN DELEGATOR. The engine is the shared `datalink_tooling.smoke` (pip-from-git
dependency on tegmentum/datalink); the DB-specific behaviour (the host CLI argv,
the wasm cli component, prompt regex, `.nullvalue` preamble, panic markers, the
smoke.expected diff) is driven by `tooling/datalink.config.json`. This wrapper
just points the engine at that config and forwards the original CLI args. The
heavy logic now lives in datalink_tooling.

  pip install -r requirements-dev.txt   # or: pip install -e ../datalink/tooling

Usage (unchanged):
    tooling/smoke.py <name>            # smoke one extension
    tooling/smoke.py --all [-j N]      # smoke every ext that has a smoke.sql
    tooling/smoke.py --list            # list extensions with smoke.sql
    tooling/smoke.py --seed-expected NAME
    tooling/smoke.py --dry-run <name>  # print the resolved argv, run nothing
"""
from pathlib import Path

from datalink_tooling import smoke

CONFIG = str(Path(__file__).resolve().parent / "datalink.config.json")

if __name__ == "__main__":
    smoke.main(config=CONFIG)
