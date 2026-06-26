#!/usr/bin/env python3
"""Scaffold a new SQLite-wasm extension.

THIN DELEGATOR. The engine is the shared `datalink_tooling.scaffold` (pip-from-git
dependency on tegmentum/datalink); the DB-specific behaviour (the extensions dir,
name rules, per-world lib.rs templates, the compat-registry crate-status check,
the `cargo check` build gate) is driven by `tooling/datalink.config.json`. This
wrapper just points the engine at that config and forwards the original CLI args.
The heavy logic now lives in datalink_tooling.

  pip install -r requirements-dev.txt   # or: pip install -e ../datalink/tooling

Usage (unchanged):
    tooling/scaffold.py <name> [--crate crate1,crate2,...] [--description "..."]
                        [--world W]
    tooling/scaffold.py --list-broken     # show crates flagged broken/needs-bootstrap
    tooling/scaffold.py --list-worlds     # show the available WIT worlds
    tooling/scaffold.py <name> --dry-run  # print the plan, write nothing
"""
from pathlib import Path

from datalink_tooling import scaffold

CONFIG = str(Path(__file__).resolve().parent / "datalink.config.json")

if __name__ == "__main__":
    scaffold.main(config=CONFIG)
