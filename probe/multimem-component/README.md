# Multi-memory components: validation probe

Hand-written WAT with two memories, used to validate that the
component-model wrapping pipeline + wasmtime accept multi-
memory before committing to the TVM substrate switch in
PLAN-browser-runtime.md.

## Reproduce

```bash
cd probe/multimem-component
wasm-tools parse probe.wat -o probe.core.wasm
wasm-tools component new probe.core.wasm -o probe.component.wasm
wasmtime --wasm multi-memory=y --invoke write_both_read_sum probe.core.wasm
# expected: 42 (= 7 in mem0 + 35 in mem1)
```

## Result (2026-06-14, wasmtime 45.0.1, wasm-tools 0.247.0)

- wasm-tools accepts the multi-memory core module ✓
- wasm-tools component new wraps cleanly ✓
- wasmtime instantiates + executes both-memory function: returns 42 ✓

Conclusion: multi-memory is valid in wasm32-wasip2 component
cores; the TVM substrate switch in PLAN-browser-runtime is
structurally feasible. Rust source → tvm-guest-mm pipeline
validation deferred to that work.
