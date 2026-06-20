.load extensions/wasm-introspect/target/wasm32-wasip2/release/wasm_introspect_extension.component.wasm

/* wasm-introspect  scalar introspection of .wasm blobs.
 *
 * Per spec: wasm_is_valid, wasm_imports, wasm_exports,
 * wasm_custom_sections, wasm_function_count, wasm_memory_pages,
 * wasm_version_byte, wasm_introspect_version. NULL on parse failure;
 * blob argument is BLOB or TEXT.
 *
 * Synthetic blob fixtures, hand-rolled and verified with wasm-tools:
 *
 *   minimal_module:   8 bytes  magic + version 1. No sections.
 *                     0061736d01000000
 *
 *   minimal_component: 8 bytes  magic + version 0x0d (component).
 *                     0061736d0d000100
 *
 *   module_export_f:  module with one local function exported as "f"
 *                     (no params, no results). Type + Function +
 *                     Export + Code sections.
 *                     0061736d010000000104016000000302010007050101660000
 *                     0a040102000b
 *
 *   module_import_mem_custom: module with an imported function
 *                     env.f (no-op type), a memory of 2 initial
 *                     pages, plus two custom sections ("hello" and
 *                     "name", both empty).
 *                     0061736d0100000001040160000002090103656e76016600
 *                     00050301000200060568656c6c6f0005046e616d65
 */

-- ---- Header sniffing: version_byte distinguishes module / component ----
-- Module (MVP) has 0x01 at offset 4.
SELECT wasm_version_byte(x'0061736d01000000');
-- Component has 0x0d (13).
SELECT wasm_version_byte(x'0061736d0d000100');
-- Non-wasm bytes return NULL.
SELECT wasm_version_byte(x'deadbeef');
-- Too short (only magic, no version) returns NULL.
SELECT wasm_version_byte(x'0061736d');

-- ---- Validity ----
-- Minimal valid module.
SELECT wasm_is_valid(x'0061736d01000000');
-- Minimal valid component.
SELECT wasm_is_valid(x'0061736d0d000100');
-- Garbage.
SELECT wasm_is_valid(x'deadbeef');
-- Empty.
SELECT wasm_is_valid(x'');

-- ---- Imports ----
-- Empty module has no imports -> [].
SELECT wasm_imports(x'0061736d01000000');
-- Module with one imported function env.f.
SELECT wasm_imports(x'0061736d0100000001040160000002090103656e7601660000050301000200060568656c6c6f0005046e616d65');
-- json_extract should reach into the array.
SELECT json_extract(wasm_imports(x'0061736d0100000001040160000002090103656e7601660000050301000200060568656c6c6f0005046e616d65'), '$[0].module');
SELECT json_extract(wasm_imports(x'0061736d0100000001040160000002090103656e7601660000050301000200060568656c6c6f0005046e616d65'), '$[0].name');
SELECT json_extract(wasm_imports(x'0061736d0100000001040160000002090103656e7601660000050301000200060568656c6c6f0005046e616d65'), '$[0].kind');

-- ---- Exports ----
-- Module with one local function exported as "f".
SELECT wasm_exports(x'0061736d0100000001040160000003020100070501016600000a040102000b');
SELECT json_extract(wasm_exports(x'0061736d0100000001040160000003020100070501016600000a040102000b'), '$[0].name');
SELECT json_extract(wasm_exports(x'0061736d0100000001040160000003020100070501016600000a040102000b'), '$[0].kind');

-- ---- Custom sections ----
-- Empty module: no customs.
SELECT wasm_custom_sections(x'0061736d01000000');
-- Two custom sections ("hello" and "name").
SELECT wasm_custom_sections(x'0061736d0100000001040160000002090103656e7601660000050301000200060568656c6c6f0005046e616d65');

-- ---- Function count ----
-- No funcs.
SELECT wasm_function_count(x'0061736d01000000');
-- One declared function.
SELECT wasm_function_count(x'0061736d0100000001040160000003020100070501016600000a040102000b');
-- One imported function (counts in the function index space).
SELECT wasm_function_count(x'0061736d0100000001040160000002090103656e7601660000050301000200060568656c6c6f0005046e616d65');

-- ---- Memory pages ----
-- No memory section -> 0.
SELECT wasm_memory_pages(x'0061736d01000000');
-- Memory with initial 2 pages.
SELECT wasm_memory_pages(x'0061736d0100000001040160000002090103656e7601660000050301000200060568656c6c6f0005046e616d65');

-- ---- NULL passthrough ----
SELECT wasm_is_valid(NULL);
SELECT wasm_imports(NULL) IS NULL;
SELECT wasm_exports(NULL) IS NULL;
SELECT wasm_custom_sections(NULL) IS NULL;
SELECT wasm_function_count(NULL) IS NULL;
SELECT wasm_memory_pages(NULL) IS NULL;
SELECT wasm_version_byte(NULL) IS NULL;

-- ---- Garbage -> NULL on parse-only scalars (is_valid returns 0) ----
SELECT wasm_imports(x'deadbeefcafebabe') IS NULL;
SELECT wasm_exports(x'deadbeefcafebabe') IS NULL;

-- ---- Version is non-empty TEXT ----
SELECT length(wasm_introspect_version()) > 0;
