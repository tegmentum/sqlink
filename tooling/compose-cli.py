#!/usr/bin/env python3
"""Build a custom sqlite-wasm cli with a chosen set of extensions
baked in at compile time.

Each baked extension is registered against the cli's sqlite3
connection via `sqlite3_create_function_v2` at startup. No wasi
component load at runtime, no `.load` needed; the extension's
scalar surface is just there. Per-call cost drops from the
measured ~2.7 us WIT boundary to a few hundred ns (native
sqlite scalar dispatch).

Trade-offs vs the WIT `.load` path:
  pro  no WIT boundary on the hot path; static call sites
  pro  no separate .wasm to ship; one binary, one cli
  con  baked-in extensions can't be hot-reloaded
  con  binary size grows with each bake-in
  con  the extension must implement the optional `bake` feature
       (`pub mod bake; register_into(*mut sqlite3) -> c_int`)

Usage:
    tooling/compose-cli.py --bake sha3                  # bake sha3
    tooling/compose-cli.py --bake sha3,hyperloglog      # multiple
    tooling/compose-cli.py --bake sha3 --output PATH    # custom path
    tooling/compose-cli.py --bake sha3 --precompile     # also AOT
    tooling/compose-cli.py --list                       # see what's bakeable
"""

from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_OUT = REPO_ROOT / "target" / "wasm32-wasip2" / "release" / "sqlite_cli_baked.component.wasm"
TARGET_DIR = REPO_ROOT / "target" / "wasm32-wasip2" / "release"


def list_bakeable() -> list[str]:
    """Scan extensions/ for crates with a `bake` feature. The contract
    is just: Cargo.toml has `bake = [...]` under `[features]` and
    src/bake.rs (or src/lib.rs's `pub mod bake;`) exposes
    `register_into(db: *mut sqlite3) -> c_int`."""
    bakeable: list[str] = []
    ext_root = REPO_ROOT / "extensions"
    for ext_dir in sorted(ext_root.iterdir()):
        cargo = ext_dir / "Cargo.toml"
        bake_rs = ext_dir / "src" / "bake.rs"
        if not cargo.exists():
            continue
        text = cargo.read_text()
        # Crude but adequate: look for a `bake = ` line under
        # [features]. Skip if there's no bake.rs to back it.
        if "\nbake = " in text and bake_rs.exists():
            bakeable.append(ext_dir.name)
    return bakeable


def cli_feature_for(ext_name: str) -> str:
    """Map the extension folder name to the cli's cargo feature
    flag. `sha3` extension  `bake-sha3` cli feature. Direct
    convention; matches the bake-* family in cli/Cargo.toml."""
    return f"bake-{ext_name.replace('_', '-')}"


def cargo_build(features: list[str]) -> Path:
    feature_str = ",".join(features)
    print(f"  cargo build --release -p sqlite-cli --target wasm32-wasip2 --features {feature_str}",
          file=sys.stderr)
    subprocess.check_call(
        ["cargo", "build", "--release", "-p", "sqlite-cli", "--target", "wasm32-wasip2",
         "--features", feature_str],
        cwd=REPO_ROOT,
    )
    return TARGET_DIR / "sqlite_cli.wasm"


def wrap_component(core_wasm: Path, out: Path) -> None:
    print(f"  wasm-tools component new  {out.name}", file=sys.stderr)
    subprocess.check_call(
        ["wasm-tools", "component", "new", str(core_wasm), "-o", str(out)],
        cwd=REPO_ROOT,
    )


def precompile(component_wasm: Path, out: Path) -> None:
    runner = REPO_ROOT / "target" / "release" / "sqlite-wasm-run"
    if not runner.exists():
        sys.exit(f"sqlite-wasm-run not built; cargo build --release -p sqlite-wasm-host first")
    print(f"  precompile  {out.name}", file=sys.stderr)
    subprocess.check_call(
        [str(runner), "precompile", str(component_wasm), str(out)],
        cwd=REPO_ROOT,
    )


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--bake", help="Comma-separated extensions to bake in (sha3,hyperloglog,...)")
    ap.add_argument("--output", default=str(DEFAULT_OUT),
                    help=f"Component output path (default: {DEFAULT_OUT.relative_to(REPO_ROOT)})")
    ap.add_argument("--precompile", action="store_true",
                    help="Also produce a .cwasm (AOT-compiled, ~37x startup win)")
    ap.add_argument("--list", action="store_true",
                    help="List the extensions that currently support bake-in")
    args = ap.parse_args()

    if args.list:
        bakeable = list_bakeable()
        if not bakeable:
            print("(no extensions currently expose a bake feature)")
            return 0
        for name in bakeable:
            print(name)
        return 0

    if not args.bake:
        ap.print_usage()
        return 2

    requested = [s.strip() for s in args.bake.split(",") if s.strip()]
    available = list_bakeable()
    missing = [n for n in requested if n not in available]
    if missing:
        print(f"Error: not bakeable: {', '.join(missing)}", file=sys.stderr)
        print(f"  Currently bakeable: {', '.join(available) or '(none)'}", file=sys.stderr)
        return 1

    if not shutil.which("wasm-tools"):
        print("Error: wasm-tools not on PATH (cargo install wasm-tools)", file=sys.stderr)
        return 1

    features = [cli_feature_for(n) for n in requested]
    print(f"Baking: {', '.join(requested)}", file=sys.stderr)

    core_wasm = cargo_build(features)
    out_component = Path(args.output)
    out_component.parent.mkdir(parents=True, exist_ok=True)
    wrap_component(core_wasm, out_component)

    print(f"\nwrote {out_component.relative_to(REPO_ROOT)}")

    if args.precompile:
        out_cwasm = out_component.with_suffix(".cwasm")
        precompile(out_component, out_cwasm)
        print(f"wrote {out_cwasm.relative_to(REPO_ROOT)}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
