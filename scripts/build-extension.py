#!/usr/bin/env python3
"""
SQLite WASM Extension Builder

Downloads, analyzes, and builds SQLite extensions for the WASM component model.
Generates JavaScript callback implementations that work with our extension API.

Supported extension sources:
- sqlean (github.com/nalgeon/sqlean) - Popular collection of SQLite extensions
- sqlite.org/src/ext - Official SQLite extensions
- Custom GitHub repos

Usage:
    python build-extension.py <extension_name> [--source sqlean|sqlite|github] [--output dir]
    python build-extension.py --list                  # List available extensions
    python build-extension.py --list-sqlean           # List sqlean extensions
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path
from typing import Dict, List, Optional, Tuple

# ============================================================================
# Extension Registry - Known extensions and their sources
# ============================================================================

SQLEAN_EXTENSIONS = {
    "crypto": {
        "description": "Hashing, encoding, and encryption functions",
        "files": ["sqlite3-crypto.c", "crypto/base32.c", "crypto/base64.c", "crypto/base85.c",
                  "crypto/hex.c", "crypto/md5.c", "crypto/sha1.c", "crypto/sha2.c", "crypto/url.c",
                  "crypto/extension.c"],
        "headers": ["crypto/base32.h", "crypto/base64.h", "crypto/base85.h", "crypto/hex.h",
                    "crypto/md5.h", "crypto/sha1.h", "crypto/sha2.h", "crypto/url.h", "crypto/extension.h"],
        "functions": ["md5", "sha1", "sha256", "sha384", "sha512", "encode", "decode", "hex", "unhex"],
    },
    "define": {
        "description": "User-defined functions and table-valued functions",
        "files": ["sqlite3-define.c", "define/eval.c", "define/extension.c", "define/manage.c"],
        "headers": ["define/eval.h", "define/extension.h", "define/manage.h"],
        "functions": ["define", "define_free", "eval"],
    },
    "fileio": {
        "description": "File I/O functions (read, write, ls, mkdir)",
        "files": ["sqlite3-fileio.c", "fileio/extension.c", "fileio/fileio.c"],
        "headers": ["fileio/extension.h", "fileio/fileio.h"],
        "functions": ["readfile", "writefile", "lsdir", "mkdir"],
    },
    "fuzzy": {
        "description": "Fuzzy string matching (Levenshtein, Soundex, etc.)",
        "files": ["sqlite3-fuzzy.c", "fuzzy/caverphone.c", "fuzzy/caver.c", "fuzzy/cologne.c",
                  "fuzzy/dlevenshtein.c", "fuzzy/editdist.c", "fuzzy/extension.c",
                  "fuzzy/hamming.c", "fuzzy/jaro.c", "fuzzy/levenshtein.c", "fuzzy/osa.c",
                  "fuzzy/phonetic.c", "fuzzy/rsoundex.c", "fuzzy/soundex.c", "fuzzy/spellfix.c"],
        "headers": ["fuzzy/caverphone.h", "fuzzy/caver.h", "fuzzy/cologne.h", "fuzzy/dlevenshtein.h",
                    "fuzzy/editdist.h", "fuzzy/extension.h", "fuzzy/hamming.h", "fuzzy/jaro.h",
                    "fuzzy/levenshtein.h", "fuzzy/osa.h", "fuzzy/phonetic.h", "fuzzy/rsoundex.h",
                    "fuzzy/soundex.h", "fuzzy/spellfix.h"],
        "functions": ["edit_distance", "soundex", "phonetic_hash", "fuzzy_equal", "caverphone",
                      "dlevenshtein", "hamming", "jaro_winkler", "levenshtein", "osa_distance"],
    },
    "ipaddr": {
        "description": "IP address manipulation functions",
        "files": ["sqlite3-ipaddr.c", "ipaddr/extension.c", "ipaddr/ipaddr.c"],
        "headers": ["ipaddr/extension.h", "ipaddr/ipaddr.h"],
        "functions": ["ipfamily", "iphost", "ipmasklen", "ipnetwork", "ipcontains"],
    },
    "math": {
        "description": "Advanced math functions",
        "files": ["sqlite3-math.c", "math/extension.c", "math/math.c"],
        "headers": ["math/extension.h", "math/math.h"],
        "functions": ["ceil", "floor", "trunc", "ln", "log", "log2", "log10", "exp", "pow", "sqrt",
                      "acos", "asin", "atan", "atan2", "cos", "sin", "tan", "cot"],
    },
    "regexp": {
        "description": "Regular expression functions",
        "files": ["sqlite3-regexp.c", "regexp/extension.c", "regexp/regexp.c"],
        "headers": ["regexp/extension.h", "regexp/regexp.h"],
        "functions": ["regexp", "regexp_like", "regexp_substr", "regexp_replace", "regexp_capture"],
    },
    "stats": {
        "description": "Statistical aggregate functions",
        "files": ["sqlite3-stats.c", "stats/extension.c", "stats/stats.c"],
        "headers": ["stats/extension.h", "stats/stats.h"],
        "functions": ["stddev", "stddev_pop", "variance", "variance_pop", "median", "percentile"],
    },
    "text": {
        "description": "Text manipulation functions",
        "files": ["sqlite3-text.c", "text/extension.c", "text/bstring.c", "text/rstring.c", "text/runes.c"],
        "headers": ["text/extension.h", "text/bstring.h", "text/rstring.h", "text/runes.h"],
        "functions": ["reverse", "split_part", "repeat", "concat_ws", "lpad", "rpad", "ltrim", "rtrim",
                      "text_left", "text_right", "text_index", "text_slice"],
    },
    "unicode": {
        "description": "Unicode-aware text functions",
        "files": ["sqlite3-unicode.c", "unicode/extension.c"],
        "headers": ["unicode/extension.h"],
        "functions": ["nupper", "nlower", "unaccent", "translit"],
    },
    "uuid": {
        "description": "UUID generation functions",
        "files": ["sqlite3-uuid.c", "uuid/extension.c", "uuid/uuid.c"],
        "headers": ["uuid/extension.h", "uuid/uuid.h"],
        "functions": ["uuid4", "uuid7", "uuid_str", "uuid_blob"],
    },
    "vsv": {
        "description": "CSV/TSV virtual table",
        "files": ["sqlite3-vsv.c", "vsv/extension.c"],
        "headers": ["vsv/extension.h"],
        "functions": ["vsv"],
    },
}

SQLITE_OFFICIAL_EXTENSIONS = {
    "json1": {
        "description": "JSON functions (built into SQLite)",
        "url": "https://sqlite.org/src/raw/ext/misc/json1.c",
        "functions": ["json", "json_array", "json_object", "json_extract", "json_type"],
    },
    "fts5": {
        "description": "Full-text search (built into SQLite)",
        "note": "Already enabled in our build",
    },
    "rtree": {
        "description": "R-Tree spatial index (built into SQLite)",
        "note": "Already enabled in our build",
    },
    "series": {
        "description": "Generate series of integers",
        "url": "https://sqlite.org/src/raw/ext/misc/series.c",
        "functions": ["generate_series"],
    },
    "carray": {
        "description": "C array binding",
        "url": "https://sqlite.org/src/raw/ext/misc/carray.c",
        "functions": ["carray"],
    },
    "closure": {
        "description": "Transitive closure virtual table",
        "url": "https://sqlite.org/src/raw/ext/misc/closure.c",
        "functions": ["transitive_closure"],
    },
    "csv": {
        "description": "CSV virtual table",
        "url": "https://sqlite.org/src/raw/ext/misc/csv.c",
        "functions": ["csv"],
    },
    "sha1": {
        "description": "SHA1 hashing",
        "url": "https://sqlite.org/src/raw/ext/misc/sha1.c",
        "functions": ["sha1"],
    },
    "shathree": {
        "description": "SHA3 hashing",
        "url": "https://sqlite.org/src/raw/ext/misc/shathree.c",
        "functions": ["sha3", "sha3_query"],
    },
    "base64": {
        "description": "Base64 encoding/decoding",
        "url": "https://sqlite.org/src/raw/ext/misc/base64.c",
        "functions": ["base64", "base64url"],
    },
    "regexp": {
        "description": "Simple regexp function",
        "url": "https://sqlite.org/src/raw/ext/misc/regexp.c",
        "functions": ["regexp"],
    },
    "uuid": {
        "description": "UUID functions",
        "url": "https://sqlite.org/src/raw/ext/misc/uuid.c",
        "functions": ["uuid", "uuid_str", "uuid_blob"],
    },
}


# ============================================================================
# Data Classes
# ============================================================================

@dataclass
class FunctionInfo:
    """Represents a parsed SQLite function registration."""
    name: str
    num_args: int  # -1 for variadic
    func_type: str  # 'scalar', 'aggregate', 'window'
    deterministic: bool = True
    c_impl_name: Optional[str] = None
    description: str = ""
    arg_types: List[str] = field(default_factory=list)
    return_type: str = "any"


@dataclass
class ExtensionInfo:
    """Represents a parsed SQLite extension."""
    name: str
    source: str  # 'sqlean', 'sqlite', 'github'
    description: str
    functions: List[FunctionInfo]
    c_files: List[str] = field(default_factory=list)
    dependencies: List[str] = field(default_factory=list)


# ============================================================================
# Source Code Parsing
# ============================================================================

def parse_sqlite3_create_function(c_code: str) -> List[FunctionInfo]:
    """
    Parse C code to find sqlite3_create_function calls.

    Patterns we look for:
    - sqlite3_create_function(db, "name", nargs, flags, ...)
    - sqlite3_create_function_v2(db, "name", nargs, flags, ...)
    - sqlite3_create_aggregate(db, "name", nargs, ...)
    - sqlite3_create_window_function(db, "name", nargs, flags, ...)
    """
    functions = []

    # Pattern for sqlite3_create_function and variants
    patterns = [
        # sqlite3_create_function(db, "name", narg, enc, pApp, xFunc, xStep, xFinal)
        r'sqlite3_create_function\s*\(\s*\w+\s*,\s*"([^"]+)"\s*,\s*(-?\d+)\s*,\s*([^,]+)',
        r'sqlite3_create_function_v2\s*\(\s*\w+\s*,\s*"([^"]+)"\s*,\s*(-?\d+)\s*,\s*([^,]+)',
        # sqlite3_create_aggregate(db, "name", narg, ...)
        r'sqlite3_create_aggregate\s*\(\s*\w+\s*,\s*"([^"]+)"\s*,\s*(-?\d+)',
        # sqlite3_create_window_function(db, "name", narg, enc, ...)
        r'sqlite3_create_window_function\s*\(\s*\w+\s*,\s*"([^"]+)"\s*,\s*(-?\d+)\s*,\s*([^,]+)',
    ]

    seen_names = set()

    for pattern in patterns:
        for match in re.finditer(pattern, c_code):
            name = match.group(1)
            if name in seen_names:
                continue
            seen_names.add(name)

            nargs = int(match.group(2))

            # Check for SQLITE_DETERMINISTIC flag
            flags = match.group(3) if len(match.groups()) >= 3 else ""
            deterministic = "SQLITE_DETERMINISTIC" in flags

            # Determine function type
            if "create_aggregate" in pattern:
                func_type = "aggregate"
            elif "create_window" in pattern:
                func_type = "window"
            else:
                func_type = "scalar"

            functions.append(FunctionInfo(
                name=name,
                num_args=nargs,
                func_type=func_type,
                deterministic=deterministic,
            ))

    return functions


def parse_function_implementations(c_code: str, functions: List[FunctionInfo]) -> None:
    """
    Try to find the implementation functions and infer types.

    Looks for patterns like:
    - static void my_func(sqlite3_context *ctx, int argc, sqlite3_value **argv)
    - sqlite3_result_text(ctx, ...)
    - sqlite3_result_int(ctx, ...)
    """
    for func in functions:
        # Look for function implementation
        impl_pattern = rf'static\s+void\s+(\w*{re.escape(func.name)}\w*)\s*\(\s*sqlite3_context'
        match = re.search(impl_pattern, c_code, re.IGNORECASE)
        if match:
            func.c_impl_name = match.group(1)

        # Look for result type hints
        result_patterns = [
            (r'sqlite3_result_text', 'text'),
            (r'sqlite3_result_int64', 'integer'),
            (r'sqlite3_result_int', 'integer'),
            (r'sqlite3_result_double', 'real'),
            (r'sqlite3_result_blob', 'blob'),
            (r'sqlite3_result_null', 'null'),
        ]

        # Try to find result type in nearby code
        for pattern, ret_type in result_patterns:
            if func.c_impl_name:
                # Look within the function implementation
                func_start = c_code.find(func.c_impl_name)
                if func_start >= 0:
                    # Search next 2000 chars for result call
                    snippet = c_code[func_start:func_start + 2000]
                    if pattern in snippet:
                        func.return_type = ret_type
                        break


# ============================================================================
# Extension Downloading
# ============================================================================

SQLEAN_BASE_URL = "https://raw.githubusercontent.com/nalgeon/sqlean/main/src"
SQLITE_BASE_URL = "https://sqlite.org/src/raw"


def download_file(url: str, dest_path: Path) -> bool:
    """Download a file from URL to destination path using curl."""
    try:
        print(f"  Downloading {url}...")
        dest_path.parent.mkdir(parents=True, exist_ok=True)
        result = subprocess.run(
            ["curl", "-fsSL", "-o", str(dest_path), url],
            capture_output=True,
            text=True,
            timeout=60
        )
        if result.returncode != 0:
            print(f"  Warning: Failed to download {url}: {result.stderr}")
            return False
        return True
    except Exception as e:
        print(f"  Warning: Failed to download {url}: {e}")
        return False


def download_sqlean_extension(name: str, output_dir: Path) -> Optional[List[Path]]:
    """Download a sqlean extension."""
    if name not in SQLEAN_EXTENSIONS:
        print(f"Error: Unknown sqlean extension '{name}'")
        return None

    ext_info = SQLEAN_EXTENSIONS[name]
    downloaded = []

    src_dir = output_dir / "src"
    src_dir.mkdir(parents=True, exist_ok=True)

    # Download source files (preserve directory structure)
    for file_path in ext_info.get("files", []):
        url = f"{SQLEAN_BASE_URL}/{file_path}"
        # Preserve subdirectory structure
        dest = src_dir / file_path
        dest.parent.mkdir(parents=True, exist_ok=True)
        if download_file(url, dest):
            downloaded.append(dest)

    # Download header files (preserve directory structure)
    for header_path in ext_info.get("headers", []):
        url = f"{SQLEAN_BASE_URL}/{header_path}"
        dest = src_dir / header_path
        dest.parent.mkdir(parents=True, exist_ok=True)
        if download_file(url, dest):
            downloaded.append(dest)

    # Download the common sqlean.h header if needed
    sqlean_header = src_dir / "sqlean.h"
    if not sqlean_header.exists():
        download_file(f"{SQLEAN_BASE_URL}/sqlean.h", sqlean_header)

    return downloaded if downloaded else None


def download_sqlite_extension(name: str, output_dir: Path) -> Optional[List[Path]]:
    """Download an official SQLite extension."""
    if name not in SQLITE_OFFICIAL_EXTENSIONS:
        print(f"Error: Unknown SQLite extension '{name}'")
        return None

    ext_info = SQLITE_OFFICIAL_EXTENSIONS[name]

    if "note" in ext_info and "url" not in ext_info:
        print(f"Note: {name} - {ext_info['note']}")
        return None

    if "url" not in ext_info:
        print(f"Error: No URL for extension '{name}'")
        return None

    src_dir = output_dir / "src"
    src_dir.mkdir(parents=True, exist_ok=True)

    url = ext_info["url"]
    dest = src_dir / f"{name}.c"

    if download_file(url, dest):
        return [dest]
    return None


def download_github_extension(repo: str, output_dir: Path, files: Optional[List[str]] = None) -> Optional[List[Path]]:
    """
    Download extension from GitHub repo.

    Args:
        repo: GitHub repo in format "owner/repo" or full URL
        output_dir: Directory to save files
        files: Specific files to download (optional)
    """
    if "github.com" in repo:
        # Extract owner/repo from URL
        match = re.search(r'github\.com/([^/]+/[^/]+)', repo)
        if match:
            repo = match.group(1)

    # Default to downloading all .c and .h files from src/ or root
    if not files:
        # This would require GitHub API to list files
        print("Note: For GitHub repos, please specify files with --files")
        return None

    downloaded = []
    src_dir = output_dir / "src"
    src_dir.mkdir(parents=True, exist_ok=True)

    for file_path in files:
        url = f"https://raw.githubusercontent.com/{repo}/main/{file_path}"
        dest = src_dir / Path(file_path).name
        if download_file(url, dest):
            downloaded.append(dest)

    return downloaded if downloaded else None


# ============================================================================
# Code Generation
# ============================================================================

def generate_js_callbacks(ext_info: ExtensionInfo, output_dir: Path) -> None:
    """Generate JavaScript callback implementations for the extension."""

    callback_file = output_dir / "extension-callbacks.js"

    # Group functions by type
    scalar_funcs = [f for f in ext_info.functions if f.func_type == "scalar"]
    aggregate_funcs = [f for f in ext_info.functions if f.func_type == "aggregate"]

    code = f'''/**
 * {ext_info.name} Extension Callbacks
 * {ext_info.description}
 *
 * Generated by build-extension.py
 * Source: {ext_info.source}
 */

// Function ID constants
'''

    # Generate function IDs
    for i, func in enumerate(ext_info.functions, start=1):
        const_name = f"FUNC_{func.name.upper()}"
        code += f"export const {const_name} = {i}n;\n"

    code += '''
// Aggregate context storage
const aggregateContexts = new Map();

// Helper functions
function makeNull() {
    return { valueType: 'null' };
}

function makeInteger(val) {
    return { valueType: 'integer', intValue: BigInt(val) };
}

function makeFloat(val) {
    return { valueType: 'float', floatValue: val };
}

function makeText(val) {
    return { valueType: 'text', textValue: String(val) };
}

function makeBlob(val) {
    return { valueType: 'blob', blobValue: val };
}

function getValue(sqlValue) {
    switch (sqlValue.valueType) {
        case 'integer': return Number(sqlValue.intValue);
        case 'float': return sqlValue.floatValue;
        case 'text': return sqlValue.textValue;
        case 'blob': return sqlValue.blobValue;
        case 'null':
        default: return null;
    }
}

/**
 * Scalar function dispatcher
 */
export function onScalarFunction(functionId, args) {
    switch (functionId) {
'''

    # Generate scalar function cases
    for func in scalar_funcs:
        const_name = f"FUNC_{func.name.upper()}"
        code += f'''        case {const_name}: {{
            // TODO: Implement {func.name}({func.num_args} args)
            // Return type hint: {func.return_type}
'''

        # Generate basic implementation based on function name
        impl = generate_function_implementation(func)
        code += impl
        code += "        }\n\n"

    code += '''        default:
            throw new Error(`Unknown function id: ${functionId}`);
    }
}

/**
 * Aggregate step function dispatcher
 */
export function onAggregateStep(functionId, contextId, args) {
    switch (functionId) {
'''

    # Generate aggregate step cases
    for func in aggregate_funcs:
        const_name = f"FUNC_{func.name.upper()}"
        code += f'''        case {const_name}: {{
            let ctx = aggregateContexts.get(contextId);
            if (!ctx) {{
                ctx = {{ values: [] }};
                aggregateContexts.set(contextId, ctx);
            }}
            // TODO: Implement {func.name} step
            if (args.length > 0) {{
                const val = getValue(args[0]);
                if (val !== null) {{
                    ctx.values.push(val);
                }}
            }}
            break;
        }}

'''

    code += '''        default:
            throw new Error(`Unknown aggregate function id: ${functionId}`);
    }
}

/**
 * Aggregate finalize function dispatcher
 */
export function onAggregateFinalize(functionId, contextId) {
    switch (functionId) {
'''

    # Generate aggregate finalize cases
    for func in aggregate_funcs:
        const_name = f"FUNC_{func.name.upper()}"
        impl = generate_aggregate_finalize(func)
        code += f'''        case {const_name}: {{
            const ctx = aggregateContexts.get(contextId);
            aggregateContexts.delete(contextId);
            if (!ctx || ctx.values.length === 0) {{
                return makeNull();
            }}
            {impl}
        }}

'''

    code += '''        default:
            throw new Error(`Unknown aggregate function id: ${functionId}`);
    }
}

// Stub implementations for other callbacks
export function onCollationCompare(collationId, a, b) {
    return a.localeCompare(b);
}

export function onUpdate(hookId, op, database, table, rowid) {}
export function onCommit(hookId) { return false; }
export function onRollback(hookId) {}
export function onAuthorize(authId, action, arg1, arg2, database, trigger) {
    return 'ok';
}
'''

    callback_file.write_text(code)
    print(f"  Generated: {callback_file}")


def generate_function_implementation(func: FunctionInfo) -> str:
    """Generate a basic implementation for a function based on its name."""
    name = func.name.lower()

    # String functions
    if name == "reverse":
        return '''            if (args.length < 1) throw new Error('reverse requires 1 argument');
            const text = getValue(args[0]);
            if (text === null) return makeNull();
            return makeText(String(text).split('').reverse().join(''));
'''

    if name in ("upper", "nupper"):
        return '''            if (args.length < 1) throw new Error('upper requires 1 argument');
            const text = getValue(args[0]);
            if (text === null) return makeNull();
            return makeText(String(text).toUpperCase());
'''

    if name in ("lower", "nlower"):
        return '''            if (args.length < 1) throw new Error('lower requires 1 argument');
            const text = getValue(args[0]);
            if (text === null) return makeNull();
            return makeText(String(text).toLowerCase());
'''

    if name == "repeat":
        return '''            if (args.length < 2) throw new Error('repeat requires 2 arguments');
            const text = getValue(args[0]);
            const count = getValue(args[1]);
            if (text === null || count === null) return makeNull();
            return makeText(String(text).repeat(Math.max(0, count)));
'''

    if name in ("lpad", "rpad"):
        pad_start = "padStart" if name == "lpad" else "padEnd"
        return f'''            if (args.length < 2) throw new Error('{name} requires 2-3 arguments');
            const text = getValue(args[0]);
            const length = getValue(args[1]);
            const fill = args.length > 2 ? getValue(args[2]) : ' ';
            if (text === null || length === null) return makeNull();
            return makeText(String(text).{pad_start}(length, fill));
'''

    if name == "concat_ws":
        return '''            if (args.length < 1) throw new Error('concat_ws requires separator and values');
            const sep = getValue(args[0]) || '';
            const parts = [];
            for (let i = 1; i < args.length; i++) {
                const v = getValue(args[i]);
                if (v !== null) parts.push(String(v));
            }
            return makeText(parts.join(sep));
'''

    if name == "split_part":
        return '''            if (args.length < 3) throw new Error('split_part requires 3 arguments');
            const text = getValue(args[0]);
            const delim = getValue(args[1]);
            const field = getValue(args[2]);
            if (text === null) return makeNull();
            const parts = String(text).split(delim);
            const idx = field - 1; // 1-based
            return idx >= 0 && idx < parts.length ? makeText(parts[idx]) : makeText('');
'''

    # Math functions
    if name == "sqrt":
        return '''            if (args.length < 1) throw new Error('sqrt requires 1 argument');
            const x = getValue(args[0]);
            if (x === null || x < 0) return makeNull();
            return makeFloat(Math.sqrt(x));
'''

    if name == "pow":
        return '''            if (args.length < 2) throw new Error('pow requires 2 arguments');
            const base = getValue(args[0]);
            const exp = getValue(args[1]);
            if (base === null || exp === null) return makeNull();
            return makeFloat(Math.pow(base, exp));
'''

    if name in ("ceil", "ceiling"):
        return '''            if (args.length < 1) throw new Error('ceil requires 1 argument');
            const x = getValue(args[0]);
            if (x === null) return makeNull();
            return makeInteger(Math.ceil(x));
'''

    if name == "floor":
        return '''            if (args.length < 1) throw new Error('floor requires 1 argument');
            const x = getValue(args[0]);
            if (x === null) return makeNull();
            return makeInteger(Math.floor(x));
'''

    if name in ("trunc", "truncate"):
        return '''            if (args.length < 1) throw new Error('trunc requires 1 argument');
            const x = getValue(args[0]);
            if (x === null) return makeNull();
            return makeInteger(Math.trunc(x));
'''

    if name in ("ln", "log"):
        return '''            if (args.length < 1) throw new Error('ln requires 1 argument');
            const x = getValue(args[0]);
            if (x === null || x <= 0) return makeNull();
            return makeFloat(Math.log(x));
'''

    if name == "log10":
        return '''            if (args.length < 1) throw new Error('log10 requires 1 argument');
            const x = getValue(args[0]);
            if (x === null || x <= 0) return makeNull();
            return makeFloat(Math.log10(x));
'''

    if name == "log2":
        return '''            if (args.length < 1) throw new Error('log2 requires 1 argument');
            const x = getValue(args[0]);
            if (x === null || x <= 0) return makeNull();
            return makeFloat(Math.log2(x));
'''

    if name == "exp":
        return '''            if (args.length < 1) throw new Error('exp requires 1 argument');
            const x = getValue(args[0]);
            if (x === null) return makeNull();
            return makeFloat(Math.exp(x));
'''

    # Crypto/hash functions
    if name in ("md5", "sha1", "sha256", "sha384", "sha512"):
        algo = name.upper().replace("SHA", "SHA-")
        return f'''            // TODO: Implement {name} using crypto.subtle or external library
            if (args.length < 1) throw new Error('{name} requires 1 argument');
            const data = getValue(args[0]);
            if (data === null) return makeNull();
            // Placeholder - use node:crypto or web crypto API
            throw new Error('{name} not yet implemented - requires crypto API');
'''

    # UUID functions
    if name in ("uuid", "uuid4"):
        return '''            // Generate UUID v4
            const bytes = new Uint8Array(16);
            crypto.getRandomValues(bytes);
            bytes[6] = (bytes[6] & 0x0f) | 0x40; // Version 4
            bytes[8] = (bytes[8] & 0x3f) | 0x80; // Variant
            const hex = [...bytes].map(b => b.toString(16).padStart(2, '0')).join('');
            return makeText(`${hex.slice(0,8)}-${hex.slice(8,12)}-${hex.slice(12,16)}-${hex.slice(16,20)}-${hex.slice(20)}`);
'''

    # Regexp function
    if name in ("regexp", "regexp_like"):
        return '''            if (args.length < 2) throw new Error('regexp requires 2 arguments');
            const pattern = getValue(args[0]);
            const text = getValue(args[1]);
            if (pattern === null || text === null) return makeNull();
            try {
                const regex = new RegExp(pattern);
                return makeInteger(regex.test(text) ? 1 : 0);
            } catch (e) {
                throw new Error(`Invalid regex: ${e.message}`);
            }
'''

    if name == "regexp_replace":
        return '''            if (args.length < 3) throw new Error('regexp_replace requires 3 arguments');
            const text = getValue(args[0]);
            const pattern = getValue(args[1]);
            const replacement = getValue(args[2]);
            const flags = args.length > 3 ? getValue(args[3]) : 'g';
            if (text === null || pattern === null) return makeNull();
            try {
                const regex = new RegExp(pattern, flags);
                return makeText(String(text).replace(regex, replacement || ''));
            } catch (e) {
                throw new Error(`Invalid regex: ${e.message}`);
            }
'''

    if name == "regexp_substr":
        return '''            if (args.length < 2) throw new Error('regexp_substr requires 2 arguments');
            const text = getValue(args[0]);
            const pattern = getValue(args[1]);
            if (text === null || pattern === null) return makeNull();
            try {
                const regex = new RegExp(pattern);
                const match = String(text).match(regex);
                return match ? makeText(match[0]) : makeNull();
            } catch (e) {
                throw new Error(`Invalid regex: ${e.message}`);
            }
'''

    # Fuzzy string matching
    if name in ("soundex",):
        return '''            if (args.length < 1) throw new Error('soundex requires 1 argument');
            const text = getValue(args[0]);
            if (text === null) return makeNull();
            // Basic Soundex implementation
            const s = String(text).toUpperCase().replace(/[^A-Z]/g, '');
            if (s.length === 0) return makeText('');
            const codes = { B:1,F:1,P:1,V:1, C:2,G:2,J:2,K:2,Q:2,S:2,X:2,Z:2,
                           D:3,T:3, L:4, M:5,N:5, R:6 };
            let result = s[0];
            let prev = codes[s[0]] || 0;
            for (let i = 1; i < s.length && result.length < 4; i++) {
                const code = codes[s[i]] || 0;
                if (code && code !== prev) {
                    result += code;
                }
                prev = code;
            }
            return makeText(result.padEnd(4, '0'));
'''

    if name in ("edit_distance", "levenshtein"):
        return '''            if (args.length < 2) throw new Error('edit_distance requires 2 arguments');
            const s1 = getValue(args[0]);
            const s2 = getValue(args[1]);
            if (s1 === null || s2 === null) return makeNull();
            const a = String(s1), b = String(s2);
            const m = a.length, n = b.length;
            const dp = Array(m + 1).fill(null).map((_, i) =>
                Array(n + 1).fill(null).map((_, j) => i === 0 ? j : j === 0 ? i : 0));
            for (let i = 1; i <= m; i++) {
                for (let j = 1; j <= n; j++) {
                    dp[i][j] = a[i-1] === b[j-1] ? dp[i-1][j-1] :
                        1 + Math.min(dp[i-1][j], dp[i][j-1], dp[i-1][j-1]);
                }
            }
            return makeInteger(dp[m][n]);
'''

    # Default: stub implementation
    args_str = f"{func.num_args} args" if func.num_args >= 0 else "variadic"
    return f'''            // TODO: Implement {func.name}({args_str})
            throw new Error('{func.name} not yet implemented');
'''


def generate_aggregate_finalize(func: FunctionInfo) -> str:
    """Generate finalize implementation for aggregate functions."""
    name = func.name.lower()

    if name in ("sum", "total"):
        return '''const sum = ctx.values.reduce((a, b) => a + b, 0);
            return makeFloat(sum);'''

    if name in ("avg", "mean"):
        return '''const sum = ctx.values.reduce((a, b) => a + b, 0);
            return makeFloat(sum / ctx.values.length);'''

    if name in ("count",):
        return '''return makeInteger(ctx.values.length);'''

    if name in ("min",):
        return '''return makeFloat(Math.min(...ctx.values));'''

    if name in ("max",):
        return '''return makeFloat(Math.max(...ctx.values));'''

    if name == "median":
        return '''const sorted = [...ctx.values].sort((a, b) => a - b);
            const mid = Math.floor(sorted.length / 2);
            const median = sorted.length % 2 ? sorted[mid] : (sorted[mid-1] + sorted[mid]) / 2;
            return makeFloat(median);'''

    if name in ("stddev", "stddev_pop"):
        return '''const mean = ctx.values.reduce((a, b) => a + b, 0) / ctx.values.length;
            const variance = ctx.values.reduce((sum, val) => sum + Math.pow(val - mean, 2), 0) / ctx.values.length;
            return makeFloat(Math.sqrt(variance));'''

    if name in ("variance", "variance_pop"):
        return '''const mean = ctx.values.reduce((a, b) => a + b, 0) / ctx.values.length;
            const variance = ctx.values.reduce((sum, val) => sum + Math.pow(val - mean, 2), 0) / ctx.values.length;
            return makeFloat(variance);'''

    if name == "group_concat":
        return '''return makeText(ctx.values.join(','));'''

    # Default
    return '''// TODO: Implement finalize for ''' + func.name + '''
            return makeNull();'''


def generate_test_file(ext_info: ExtensionInfo, output_dir: Path) -> None:
    """Generate a test file for the extension."""

    test_file = output_dir / "test-extension.mjs"

    code = f'''/**
 * Tests for {ext_info.name} extension
 * Generated by build-extension.py
 */

import {{ fileURLToPath }} from 'url';
import {{ dirname, join }} from 'path';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);

// Import extension callbacks
import * as callbacks from './extension-callbacks.js';

// Import SQLite extensible component
const sqlite = await import('../../../build/js-ext/sqlite-extensible.js');
const {{ lowLevel, extension }} = sqlite;

let passed = 0;
let failed = 0;

function assert(condition, message) {{
    if (condition) {{
        passed++;
        console.log(`  ✓ ${{message}}`);
    }} else {{
        failed++;
        console.log(`  ✗ ${{message}}`);
    }}
}}

function assertEqual(actual, expected, message) {{
    if (actual === expected) {{
        passed++;
        console.log(`  ✓ ${{message}}`);
    }} else {{
        failed++;
        console.log(`  ✗ ${{message}}`);
        console.log(`    Expected: ${{expected}}`);
        console.log(`    Actual: ${{actual}}`);
    }}
}}

async function runTests() {{
    console.log('{ext_info.name} Extension Tests\\n');

    // Open database
    const db = lowLevel.open(':memory:', {{ readwrite: true, create: true, memory: true }});
    assert(db !== 0n, 'Database opened');

'''

    # Generate test cases for each function
    for func in ext_info.functions:
        const_name = f"FUNC_{func.name.upper()}"
        code += f'''
    // Test {func.name}
    console.log('\\n  Testing {func.name}...');
    try {{
        extension.registerScalarFunction(
            db, '{func.name}', {func.num_args},
            {{ deterministic: {str(func.deterministic).lower()} }},
            callbacks.{const_name}
        );
        assert(true, '{func.name}() registered');

        // TODO: Add specific test cases for {func.name}
        // const stmt = lowLevel.prepare(db, "SELECT {func.name}(...)");
        // lowLevel.step(stmt);
        // const result = lowLevel.column...(stmt, 0);
        // assertEqual(result, expected, '{func.name}() returns correct value');
        // lowLevel.finalize(stmt);

    }} catch (e) {{
        failed++;
        console.log(`  ✗ {func.name}() failed: ${{e.message}}`);
    }}
'''

    code += '''
    // Close database
    lowLevel.close(db);

    // Summary
    console.log('\\n' + '='.repeat(50));
    console.log(`Tests: ${passed + failed} total, ${passed} passed, ${failed} failed`);
    console.log('='.repeat(50) + '\\n');

    if (failed > 0) {
        process.exit(1);
    }
}

runTests().catch(err => {
    console.error('Test error:', err);
    process.exit(1);
});
'''

    test_file.write_text(code)
    print(f"  Generated: {test_file}")


def generate_registration_code(ext_info: ExtensionInfo, output_dir: Path) -> None:
    """Generate code to register all extension functions."""

    reg_file = output_dir / "register.mjs"

    code = f'''/**
 * Register {ext_info.name} extension functions
 * Generated by build-extension.py
 */

import * as callbacks from './extension-callbacks.js';

/**
 * Register all {ext_info.name} functions with a database handle
 * @param {{bigint}} db - Database handle from lowLevel.open()
 * @param {{object}} extension - Extension API from sqlite-extensible
 */
export function register{ext_info.name.title().replace("_", "")}(db, extension) {{
    const handles = [];

'''

    for func in ext_info.functions:
        const_name = f"FUNC_{func.name.upper()}"
        func_type = "registerScalarFunction" if func.func_type == "scalar" else "registerAggregateFunction"
        code += f'''    // Register {func.name}
    handles.push(extension.{func_type}(
        db, '{func.name}', {func.num_args},
        {{ deterministic: {str(func.deterministic).lower()} }},
        callbacks.{const_name}
    ));

'''

    code += '''    return handles;
}

export const FUNCTION_IDS = {
'''

    for func in ext_info.functions:
        const_name = f"FUNC_{func.name.upper()}"
        code += f'    {func.name}: callbacks.{const_name},\n'

    code += '};\n'

    reg_file.write_text(code)
    print(f"  Generated: {reg_file}")


# ============================================================================
# Main Build Process
# ============================================================================

def build_extension(
    name: str,
    source: str = "sqlean",
    output_dir: Optional[Path] = None,
    github_repo: Optional[str] = None,
    github_files: Optional[List[str]] = None,
) -> bool:
    """
    Download and build an extension.

    Args:
        name: Extension name
        source: Source type ('sqlean', 'sqlite', 'github')
        output_dir: Output directory (default: extensions/<name>)
        github_repo: GitHub repo for 'github' source
        github_files: Files to download for 'github' source

    Returns:
        True if successful
    """
    if output_dir is None:
        output_dir = Path("extensions") / name
    else:
        output_dir = Path(output_dir)

    output_dir.mkdir(parents=True, exist_ok=True)

    print(f"\n{'='*60}")
    print(f"Building extension: {name}")
    print(f"Source: {source}")
    print(f"Output: {output_dir}")
    print(f"{'='*60}\n")

    # Download source files
    print("Downloading source files...")
    if source == "sqlean":
        c_files = download_sqlean_extension(name, output_dir)
    elif source == "sqlite":
        c_files = download_sqlite_extension(name, output_dir)
    elif source == "github":
        if not github_repo:
            print("Error: --github-repo required for github source")
            return False
        c_files = download_github_extension(github_repo, output_dir, github_files)
    else:
        print(f"Error: Unknown source '{source}'")
        return False

    if not c_files:
        print("Error: Failed to download source files")
        return False

    print(f"\nDownloaded {len(c_files)} file(s)")

    # Parse C code
    print("\nParsing source code...")
    all_functions = []

    for c_file in c_files:
        print(f"  Parsing {c_file.name}...")
        c_code = c_file.read_text(errors='ignore')
        functions = parse_sqlite3_create_function(c_code)
        parse_function_implementations(c_code, functions)
        all_functions.extend(functions)

    print(f"\nFound {len(all_functions)} functions:")
    for func in all_functions:
        args_str = f"{func.num_args} args" if func.num_args >= 0 else "variadic"
        det_str = "deterministic" if func.deterministic else ""
        print(f"  - {func.name}({args_str}) [{func.func_type}] {det_str}")

    # Get extension info
    if source == "sqlean" and name in SQLEAN_EXTENSIONS:
        description = SQLEAN_EXTENSIONS[name]["description"]
    elif source == "sqlite" and name in SQLITE_OFFICIAL_EXTENSIONS:
        description = SQLITE_OFFICIAL_EXTENSIONS[name]["description"]
    else:
        description = f"{name} extension"

    ext_info = ExtensionInfo(
        name=name,
        source=source,
        description=description,
        functions=all_functions,
        c_files=[str(f) for f in c_files],
    )

    # Generate JavaScript callbacks
    print("\nGenerating JavaScript callbacks...")
    generate_js_callbacks(ext_info, output_dir)

    # Generate registration code
    print("\nGenerating registration code...")
    generate_registration_code(ext_info, output_dir)

    # Generate test file
    print("\nGenerating test file...")
    generate_test_file(ext_info, output_dir)

    # Generate README
    readme = output_dir / "README.md"
    readme.write_text(f'''# {name} Extension

{description}

## Source
- Type: {source}
- Files: {", ".join(f.name for f in c_files)}

## Functions

| Function | Args | Type | Deterministic |
|----------|------|------|---------------|
''' + "\n".join(
    f"| {f.name} | {f.num_args} | {f.func_type} | {'✓' if f.deterministic else ''} |"
    for f in all_functions
) + '''

## Usage

```javascript
import { lowLevel, extension } from '../build/js-ext/sqlite-extensible.js';
import { register''' + name.title().replace('_', '') + ''' } from './register.mjs';

const db = lowLevel.open(':memory:', { readwrite: true, create: true });
register''' + name.title().replace('_', '') + '''(db, extension);

// Now you can use the extension functions in SQL
const stmt = lowLevel.prepare(db, "SELECT ...");
```

## Testing

```bash
node test-extension.mjs
```
''')
    print(f"  Generated: {readme}")

    print(f"\n{'='*60}")
    print(f"Extension '{name}' built successfully!")
    print(f"Output directory: {output_dir}")
    print(f"{'='*60}\n")

    return True


def list_extensions(source: Optional[str] = None) -> None:
    """List available extensions."""

    if source is None or source == "sqlean":
        print("\nSqlean Extensions (github.com/nalgeon/sqlean):")
        print("-" * 60)
        for name, info in SQLEAN_EXTENSIONS.items():
            funcs = ", ".join(info.get("functions", [])[:5])
            if len(info.get("functions", [])) > 5:
                funcs += "..."
            print(f"  {name:12} - {info['description']}")
            print(f"               Functions: {funcs}")
        print()

    if source is None or source == "sqlite":
        print("\nSQLite Official Extensions (sqlite.org):")
        print("-" * 60)
        for name, info in SQLITE_OFFICIAL_EXTENSIONS.items():
            note = info.get("note", "")
            if note:
                print(f"  {name:12} - {info['description']} ({note})")
            else:
                funcs = ", ".join(info.get("functions", [])[:5])
                print(f"  {name:12} - {info['description']}")
                print(f"               Functions: {funcs}")
        print()


# ============================================================================
# CLI
# ============================================================================

def main():
    parser = argparse.ArgumentParser(
        description="SQLite WASM Extension Builder",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  %(prog)s text --source sqlean          # Build sqlean text extension
  %(prog)s uuid --source sqlite          # Build SQLite uuid extension
  %(prog)s --list                         # List all available extensions
  %(prog)s --list-sqlean                  # List sqlean extensions
  %(prog)s crypto --output ./my-ext       # Build to custom directory
""",
    )

    parser.add_argument("name", nargs="?", help="Extension name to build")
    parser.add_argument("--source", choices=["sqlean", "sqlite", "github"],
                        default="sqlean", help="Extension source (default: sqlean)")
    parser.add_argument("--output", "-o", help="Output directory")
    parser.add_argument("--github-repo", help="GitHub repo for github source (owner/repo)")
    parser.add_argument("--github-files", nargs="+", help="Files to download from GitHub repo")
    parser.add_argument("--list", action="store_true", help="List available extensions")
    parser.add_argument("--list-sqlean", action="store_true", help="List sqlean extensions")
    parser.add_argument("--list-sqlite", action="store_true", help="List SQLite extensions")

    args = parser.parse_args()

    if args.list:
        list_extensions()
        return

    if args.list_sqlean:
        list_extensions("sqlean")
        return

    if args.list_sqlite:
        list_extensions("sqlite")
        return

    if not args.name:
        parser.print_help()
        return

    success = build_extension(
        name=args.name,
        source=args.source,
        output_dir=Path(args.output) if args.output else None,
        github_repo=args.github_repo,
        github_files=args.github_files,
    )

    sys.exit(0 if success else 1)


if __name__ == "__main__":
    main()
