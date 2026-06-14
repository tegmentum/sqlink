(module
  (memory (export "mem0") 1)
  (memory $m1 (export "mem1") 1)

  ;; Write to both memories, read back, return sum.
  ;; If the engine doesn't support multi-memory or the
  ;; component model rejects this shape, this won't load.
  (func (export "write_both_read_sum") (result i32)
    ;; Write 7 at offset 0 in mem0
    (i32.store (i32.const 0) (i32.const 7))
    ;; Write 35 at offset 0 in mem1 (use $m1 reference)
    (i32.store $m1 (i32.const 0) (i32.const 35))
    ;; Sum the two i32 reads from each memory
    (i32.add
      (i32.load (i32.const 0))
      (i32.load $m1 (i32.const 0)))
  )
)
