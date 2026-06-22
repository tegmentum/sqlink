#!/usr/bin/env python3
"""Auto-derive smoke fixtures from inventory.json.

For each plugin in inventory.json, pick ONE function per surface
kind (scalar/aggregate/vtab/collation) and synthesize a probe.

Strategy: most extensions are scalar-only. We pick the function
whose name + arity matches a known shape (hash, regex, validator,
emoji, etc.) and emit a probe. For the long tail we emit a
minimal "SELECT fn(<placeholder>)" that just verifies the
function dispatches (no-output-match → matches non-empty by
default).

The output is fixtures.toml. Hand-rolled entries in
HANDROLLED below override the auto-derived ones.
"""

import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent

with open(ROOT / "inventory.json") as f:
    inv = json.load(f)

# Group by (plugin, kind)
groups = {}
for row in inv:
    groups.setdefault((row["plugin"], row["kind"]), []).append(row)


# ─── hand-rolled fixtures ──────────────────────────────────
# These take precedence over auto-derived ones. Keys are plugin
# names; values are TOML-table dicts following the same shape as
# the generated output.
#
# Pattern: for each kind, give `sql` + (`expects` OR
# `expects_regex`). `setup` (a list of pre-probe SQL stmts) is
# optional and used for aggregates / vtabs / collations.

HANDROLLED = {
    # Pure scalars where we know exact output
    "sha3": {"scalar": {
        "sql": "SELECT sha3_256('test')",
        "expects": "36f028580bb02cc8272a9a020f4200e346e276ae664e45ee80745574e2f5ab80",
    }},
    "uuid": {"scalar": {
        "sql": "SELECT length(uuid_v4())",
        "expects": "36",
    }},
    "emoji": {"scalar": {
        "sql": "SELECT emoji_count('hello 👋 world')",
        "expects": "1",
    }},
    "case": {"scalar": {
        "sql": "SELECT to_snake_case('HelloWorld')",
        "expects": "hello_world",
    }},
    "regexp": {"scalar": {
        "sql": "SELECT regexp('^a', 'apple')",
        "expects": "1",
    }},
    "json1": {"scalar": {
        "sql": "SELECT json_array_length('[1,2,3]')",
        "expects": "3",
    }},
    "crc": {"scalar": {
        "sql": "SELECT printf('%08x', crc32('test'))",
        "expects_regex": r"^[0-9a-f]{8}$",
    }},
    "hexdump": {"scalar": {
        "sql": "SELECT hexdump(cast('AB' as blob))",
        "expects_regex": r"^[0-9A-Fa-f]+",
    }},
    "morse": {"scalar": {
        "sql": "SELECT morse_encode('SOS')",
        "expects": "... --- ...",
    }},
    "idna": {"scalar": {
        "sql": "SELECT idna_encode('münchen.de')",
        "expects_regex": r"^xn--",
    }},
    "faker": {"scalar": {
        "sql": "SELECT length(faker_name()) > 0",
        "expects": "1",
    }},
    "sentiment": {"scalar": {
        "sql": "SELECT typeof(sentiment_score('happy'))",
        "expects_regex": r"^(real|integer)$",
    }},
    "cron": {"scalar": {
        "sql": "SELECT cron_valid('* * * * *')",
        "expects": "1",
    }},
    "crypto": {"scalar": {
        "sql": "SELECT length(md5('test'))",
        "expects": "32",
    }},
    "mailto": {"scalar": {
        "sql": "SELECT mailto_user('mailto:a@b.com')",
        "expects": "a",
    }},
    "iso": {"scalar": {
        "sql": "SELECT iso3166_alpha2_to_alpha3('US')",
        "expects": "USA",
    }},
    "ssn": {"scalar": {
        "sql": "SELECT ssn_valid('078-05-1120')",
        "expects": "0",
    }},
    "numfmt": {"scalar": {
        "sql": "SELECT numfmt_comma(1234567)",
        "expects": "1,234,567",
    }},
    "ipaddr": {"scalar": {
        "sql": "SELECT ipaddr_valid('192.168.1.1')",
        "expects": "1",
    }},
    "aba": {"scalar": {
        # 011000015 is a valid ABA routing number (Federal
        # Reserve Bank of Boston)
        "sql": "SELECT aba_validate('011000015')",
        "expects": "1",
    }},
    "bic": {"scalar": {
        "sql": "SELECT bic_valid('DEUTDEFF')",
        "expects": "1",
    }},
    "cusip": {"scalar": {
        # Example CUSIP from the standard
        "sql": "SELECT cusip_valid('037833100')",
        "expects": "1",
    }},
    "creditcard": {"scalar": {
        # Test card from Visa docs
        "sql": "SELECT creditcard_valid('4111111111111111')",
        "expects": "1",
    }},
    "isin": {"scalar": {
        "sql": "SELECT isin_valid('US0378331005')",
        "expects": "1",
    }},
    "iban": {"scalar": {
        "sql": "SELECT iban_valid('GB82WEST12345698765432')",
        "expects": "1",
    }},
    "ean": {"scalar": {
        "sql": "SELECT ean_valid('5901234123457')",
        "expects": "1",
    }},
    "vin": {"scalar": {
        "sql": "SELECT vin_valid('1HGCM82633A004352')",
        "expects": "1",
    }},
    "postcode": {"scalar": {
        "sql": "SELECT postcode_valid('US', '90210')",
        "expects": "1",
    }},
    "dns": {"scalar": {
        # No actual DNS call — just a validation function
        "sql": "SELECT typeof(dns_version())",
        "expects": "text",
    }},
    "detect": {"scalar": {
        "sql": "SELECT detect_lang('Hello, world')",
        "expects_regex": r"^[a-z]{2,3}$",
    }},
    "ical": {"scalar": {
        "sql": "SELECT typeof(ical_now())",
        "expects": "text",
    }},
    "zorder": {"scalar": {
        "sql": "SELECT zorder_encode_2d(1, 2)",
        "expects_regex": r"^\d+$",
    }},
    "totype": {"scalar": {
        "sql": "SELECT toint('42')",
        "expects": "42",
    }},
    "color": {"scalar": {
        "sql": "SELECT color_hex_to_rgb('#ff0000')",
        "expects_regex": r"255",
    }},
    "humansize": {"scalar": {
        "sql": "SELECT humansize_format(1024)",
        "expects_regex": r"K(i)?B",
    }},
    "unitconv": {"scalar": {
        "sql": "SELECT unitconv_convert(100, 'cm', 'm')",
        "expects_regex": r"^1(\.0+)?$",
    }},
    "latlon": {"scalar": {
        "sql": "SELECT typeof(latlon_distance(0.0, 0.0, 1.0, 1.0))",
        "expects": "real",
    }},
    "radix": {"scalar": {
        "sql": "SELECT radix_to_base(255, 16)",
        "expects": "ff",
    }},
    "natsort": {"scalar": {
        # Just check it dispatches
        "sql": "SELECT typeof(natsort_key('a10'))",
        "expects": "text",
    }},
    "currency": {"scalar": {
        "sql": "SELECT currency_alpha3_name('USD')",
        "expects_regex": r"[Dd]ollar",
    }},
    "country": {"scalar": {
        "sql": "SELECT country_alpha2_name('US')",
        "expects_regex": r"United States",
    }},
    "geo-distance": {"scalar": {
        "sql": "SELECT typeof(geo_distance_haversine(0, 0, 1, 1))",
        "expects": "real",
    }},
    "setops": {"scalar": {
        "sql": "SELECT setops_intersect('a,b,c', 'b,c,d')",
        "expects_regex": r"b|c",
    }},
    # Aggregates
    "stats": {"aggregate": {
        "setup": [
            "CREATE TABLE t(x REAL)",
            "INSERT INTO t VALUES (1.0), (2.0), (3.0), (4.0), (5.0)",
        ],
        "sql": "SELECT stddev_samp(x) FROM t",
        "expects_regex": r"^1\.5811",
    }},
    # vtabs
    "series": {"vtab": {
        "sql": "SELECT count(*) FROM generate_series(1, 10)",
        "expects": "10",
    }},
    # collation: tested via ORDER BY
}

# ─── auto-derivation rules ──────────────────────────────────

PLACEHOLDER_BY_ARITY = {
    0: "",
    1: "'test'",
    2: "'test', 1",
    3: "'test', 1, 1",
    4: "'test', 1, 1, 1",
    -1: "'test'",  # variadic
}

# Don't bother probing these — they're intentionally tricky or
# require external resources we can't provide in CI.
SKIP_PLUGINS = {
    "wasm-demo": "C-bundled demo; not a normal extension",
    "fts5": "bundled vtab; already in libsqlite3-sys",
    "rtree": "bundled vtab; already in libsqlite3-sys",
    "geopoly": "bundled vtab; already in libsqlite3-sys",
    # Bridge bindings compile against current WIT (see commit
    # restoring the vendored deps/sqlite-extension/). The standalone
    # component imports postgis:wasm/* + sfcgal:component/*, so .load
    # only succeeds after `wac plug` against postgis-composed.wasm
    # AND sfcgal.component.wasm. Until the smoke harness learns to
    # compose, this stays skipped. See TRIAGE.md → "postgis-bridge".
    "postgis-bridge": "needs wac-compose with postgis-composed.wasm + sfcgal-wasm before .load; bindings rebuilt against current sqlite-loader-wit",
}


def pick_scalar(plugin: str, fns: list[dict]) -> dict | None:
    """Pick a representative scalar function + synthesize a
    probe. Returns dict or None if we can't reasonably probe."""
    # Prefer arity-0 (no input ambiguity), then arity-1, then
    # arity-2. Skip functions that look like they need typed
    # input we can't synthesize (e.g. blob args).
    fns_sorted = sorted(fns, key=lambda r: (r["num_args"] if r["num_args"] >= 0 else 99, r["name"]))
    for fn in fns_sorted:
        name = fn["name"]
        n = fn["num_args"]
        # Skip helper / internal-looking functions
        if name.startswith("_") or name.endswith("_internal"):
            continue
        # Aggregates that leaked into scalar list
        if name.endswith("_step") or name.endswith("_finalize"):
            continue
        args = PLACEHOLDER_BY_ARITY.get(n, "'test'")
        return {
            "sql": f"SELECT {name}({args}) IS NOT NULL OR {name}({args}) IS NULL",
            "expects": "1",
        }
    return None


def emit_toml(fixtures: dict) -> str:
    """Serialize the fixture dict as TOML. Hand-rolled to avoid
    a TOML lib dep and to control quoting precisely."""
    lines = ["# Auto-generated by tests/extension-smoke/gen-fixtures.py",
             "# Hand-edits welcome  the script is rerunnable from",
             "# inventory.json but won't clobber hand-rolled entries",
             "# unless you remove the HANDROLLED block in gen-fixtures.py.",
             ""]
    for plugin in sorted(fixtures):
        entry = fixtures[plugin]
        # Sanitize TOML key
        key = plugin if all(c.isalnum() or c == "_" for c in plugin) else f'"{plugin}"'
        if entry.get("skip"):
            lines.append(f"[extension.{key}]")
            lines.append("skip = true")
            if "note" in entry:
                lines.append(f'note = {toml_str(entry["note"])}')
            lines.append("")
            continue
        for kind in ("scalar", "aggregate", "vtab", "collation"):
            if kind not in entry:
                continue
            probe = entry[kind]
            lines.append(f"[extension.{key}.{kind}]")
            if "setup" in probe:
                items = ", ".join(toml_str(s) for s in probe["setup"])
                lines.append(f"setup = [{items}]")
            lines.append(f'sql = {toml_str(probe["sql"])}')
            if "expects" in probe:
                lines.append(f'expects = {toml_str(probe["expects"])}')
            if "expects_regex" in probe:
                lines.append(f'expects_regex = {toml_str(probe["expects_regex"])}')
            lines.append("")
    return "\n".join(lines)


def toml_str(s: str) -> str:
    """Quote a string for TOML. Use basic strings; escape \\ and "."""
    if "\n" in s or '"' in s and "'" not in s:
        # Use triple-quoted literal string
        return "'''" + s + "'''"
    # Use double-quoted, escaped
    escaped = s.replace("\\", "\\\\").replace('"', '\\"')
    return f'"{escaped}"'


def main():
    # Build fixtures: handrolled first, then auto-derived for
    # the remaining scalar entries.
    fixtures = {}

    for plugin, reason in SKIP_PLUGINS.items():
        fixtures[plugin] = {"skip": True, "note": reason}

    for plugin, payload in HANDROLLED.items():
        fixtures[plugin] = payload

    # All plugins in inventory
    all_plugins = sorted(set(row["plugin"] for row in inv))
    for plugin in all_plugins:
        if plugin in fixtures:
            continue
        # Scalars
        scalars = groups.get((plugin, "scalar"), [])
        if scalars:
            probe = pick_scalar(plugin, scalars)
            if probe:
                fixtures.setdefault(plugin, {})["scalar"] = probe

    out = emit_toml(fixtures)
    sys.stdout.write(out)


if __name__ == "__main__":
    main()
