//! Heap-backed named priority queues, exposed as scalar SQL functions.
//!
//! Queue state lives in a thread-local `HashMap<String, PriorityQueue<...>>`,
//! mirroring the pattern used by other stateful scalar extensions in
//! this repo. Each queue is independent and identified by a TEXT name.
//!
//! Higher numeric priority pops first. Equal priorities pop in FIFO
//! order (insertion order) thanks to a monotonic per-queue seq we mix
//! into the priority tuple so the underlying max-heap orders newer
//! arrivals after older ones for the same user-visible priority.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use std::collections::HashMap;

    use priority_queue::PriorityQueue;

    mod bindings {
        wit_bindgen::generate!({
            path: "../../sqlite-loader-wit/wit",
            world: "minimal",
            generate_all,
        });
    }

    use bindings::exports::sqlite::extension::metadata::{
        Guest as MetadataGuest, Manifest, ScalarFunctionSpec,
    };
    use bindings::exports::sqlite::extension::scalar_function::Guest as ScalarFunctionGuest;
    use bindings::sqlite::extension::types::{FunctionFlags, SqlValue};

    // ---- Function IDs ----
    const FID_PUSH: u64 = 1;
    const FID_POP: u64 = 2;
    const FID_PEEK: u64 = 3;
    const FID_SIZE: u64 = 4;
    const FID_CLEAR: u64 = 5;
    const FID_DRAIN: u64 = 6;
    const FID_VERSION: u64 = 7;

    /// One queue entry. We pair the user value with a per-queue
    /// monotonic seq so that:
    ///   (a) duplicate values can coexist (the PriorityQueue crate
    ///       deduplicates on item hash, so the seq makes them unique)
    ///   (b) equal user priorities tie-break FIFO (older seq = pops
    ///       first when user priority matches).
    type Item = (String, u64);

    /// Heap ordering key. The crate is a max-heap. We want:
    ///   - higher `user_priority` first
    ///   - among equal user_priority, smaller `seq` first (FIFO)
    /// So we encode the second component as `i64::MAX - seq` so a
    /// smaller seq produces a larger value, which the max-heap pops
    /// first. user_priority lives in `0`, the tie-breaker in `1`.
    type Prio = (i64, i64);

    struct Queue {
        pq: PriorityQueue<Item, Prio>,
        next_seq: u64,
    }

    impl Queue {
        fn new() -> Self {
            Self { pq: PriorityQueue::new(), next_seq: 0 }
        }
    }

    thread_local! {
        static QUEUES: RefCell<HashMap<String, Queue>> =
            RefCell::new(HashMap::new());
    }

    struct Ext;

    // ---- Arg helpers ----
    fn arg_text(args: &[SqlValue], i: usize, fname: &str) -> Result<String, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(s.clone()),
            _ => Err(format!("{fname}: TEXT arg at {i}")),
        }
    }

    fn arg_int(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            Some(SqlValue::Real(r)) => Ok(*r as i64),
            _ => Err(format!("{fname}: INTEGER arg at {i}")),
        }
    }

    /// Render a string value as a JSON-encoded string literal (used by
    /// pq_drain). Handles the SQL-relevant escapes; no Unicode escapes
    /// beyond what JSON requires, since our values arrived through the
    /// SQL TEXT channel which is already UTF-8.
    fn json_escape(s: &str, out: &mut String) {
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\x08' => out.push_str("\\b"),
                '\x0c' => out.push_str("\\f"),
                c if (c as u32) < 0x20 => {
                    out.push_str(&format!("\\u{:04x}", c as u32));
                }
                c => out.push(c),
            }
        }
        out.push('"');
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            // Stateful  these are NOT deterministic from SQLite's
            // point of view: same args, different outputs as the queue
            // mutates. Mark them all non-deterministic so the planner
            // doesn't fold or cache calls.
            let nd = FunctionFlags::empty();
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "priority_queue".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_PUSH, "pq_push", 3, nd),
                    s(FID_POP, "pq_pop", 1, nd),
                    s(FID_PEEK, "pq_peek", 1, nd),
                    s(FID_SIZE, "pq_size", 1, nd),
                    s(FID_CLEAR, "pq_clear", 1, nd),
                    s(FID_DRAIN, "pq_drain", 1, nd),
                    s(FID_VERSION, "pq_version", 0, det),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                has_wal_hook: false,
                wal_hook_id: 0,
                dot_commands: alloc::vec![],
                declared_capabilities: alloc::vec![],
                optional_capabilities: alloc::vec![],
                preferred_prefix: Some("priority_queue".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.priority_queue".into()),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),

                FID_PUSH => {
                    let name = arg_text(&args, 0, "pq_push")?;
                    let priority = arg_int(&args, 1, "pq_push")?;
                    let value = arg_text(&args, 2, "pq_push")?;
                    QUEUES.with(|m| {
                        let mut map = m.borrow_mut();
                        let q = map.entry(name).or_insert_with(Queue::new);
                        let seq = q.next_seq;
                        q.next_seq = q.next_seq.wrapping_add(1);
                        // i64::MAX - seq so older items beat newer at
                        // equal user-priority under max-heap ordering.
                        let tie = i64::MAX.wrapping_sub(seq as i64);
                        q.pq.push((value, seq), (priority, tie));
                        Ok(SqlValue::Integer(q.pq.len() as i64))
                    })
                }

                FID_POP => {
                    let name = arg_text(&args, 0, "pq_pop")?;
                    QUEUES.with(|m| {
                        let mut map = m.borrow_mut();
                        let Some(q) = map.get_mut(&name) else {
                            return Ok(SqlValue::Null);
                        };
                        match q.pq.pop() {
                            Some(((value, _seq), _prio)) => Ok(SqlValue::Text(value)),
                            None => Ok(SqlValue::Null),
                        }
                    })
                }

                FID_PEEK => {
                    let name = arg_text(&args, 0, "pq_peek")?;
                    QUEUES.with(|m| {
                        let map = m.borrow();
                        let Some(q) = map.get(&name) else {
                            return Ok(SqlValue::Null);
                        };
                        match q.pq.peek() {
                            Some(((value, _seq), _prio)) => Ok(SqlValue::Text(value.clone())),
                            None => Ok(SqlValue::Null),
                        }
                    })
                }

                FID_SIZE => {
                    let name = arg_text(&args, 0, "pq_size")?;
                    QUEUES.with(|m| {
                        let map = m.borrow();
                        let n = map.get(&name).map(|q| q.pq.len()).unwrap_or(0);
                        Ok(SqlValue::Integer(n as i64))
                    })
                }

                FID_CLEAR => {
                    let name = arg_text(&args, 0, "pq_clear")?;
                    QUEUES.with(|m| {
                        let mut map = m.borrow_mut();
                        let n = match map.get_mut(&name) {
                            Some(q) => {
                                let n = q.pq.len();
                                q.pq.clear();
                                q.next_seq = 0;
                                n
                            }
                            None => 0,
                        };
                        Ok(SqlValue::Integer(n as i64))
                    })
                }

                FID_DRAIN => {
                    let name = arg_text(&args, 0, "pq_drain")?;
                    QUEUES.with(|m| {
                        let mut map = m.borrow_mut();
                        let Some(q) = map.get_mut(&name) else {
                            return Ok(SqlValue::Text("[]".to_string()));
                        };
                        let mut out = String::from("[");
                        let mut first = true;
                        while let Some(((value, _seq), _prio)) = q.pq.pop() {
                            if !first {
                                out.push(',');
                            }
                            first = false;
                            json_escape(&value, &mut out);
                        }
                        out.push(']');
                        q.next_seq = 0;
                        Ok(SqlValue::Text(out))
                    })
                }

                other => Err(format!("priority_queue: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
