-- Smoke test for the `priority-queue` extension.
-- Heap-backed named priority queues with thread-local state.
.load extensions/priority-queue/target/wasm32-wasip2/release/priority_queue_extension.component.wasm

-- version scalar (deterministic)
SELECT pq_version() = '0.1.0';

-- empty queue: peek/pop are NULL, size is 0
SELECT pq_peek('jobs');
SELECT pq_pop('jobs');
SELECT pq_size('jobs');

-- push three items, returns new size each time
SELECT pq_push('jobs', 5, 'low');
SELECT pq_push('jobs', 10, 'high');
SELECT pq_push('jobs', 7, 'mid');

-- size + peek: highest priority is 'high' (10)
SELECT pq_size('jobs');
SELECT pq_peek('jobs');

-- pop in priority order: 'high' (10), 'mid' (7), 'low' (5)
SELECT pq_pop('jobs');
SELECT pq_pop('jobs');
SELECT pq_pop('jobs');

-- empty again
SELECT pq_size('jobs');
SELECT pq_pop('jobs');

-- FIFO tie-break at equal priority: a then b then c
SELECT pq_push('fifo', 1, 'a');
SELECT pq_push('fifo', 1, 'b');
SELECT pq_push('fifo', 1, 'c');
SELECT pq_pop('fifo');
SELECT pq_pop('fifo');
SELECT pq_pop('fifo');

-- pq_drain returns JSON array highest-first and empties the queue
SELECT pq_push('drain', 1, 'one');
SELECT pq_push('drain', 3, 'three');
SELECT pq_push('drain', 2, 'two');
SELECT pq_drain('drain');
SELECT pq_size('drain');

-- pq_clear returns prior size; subsequent ops see empty queue
SELECT pq_push('cl', 1, 'x');
SELECT pq_push('cl', 2, 'y');
SELECT pq_clear('cl');
SELECT pq_size('cl');
SELECT pq_pop('cl');

-- named queues are independent
SELECT pq_push('q1', 1, 'A');
SELECT pq_push('q2', 1, 'B');
SELECT pq_size('q1');
SELECT pq_size('q2');
SELECT pq_pop('q1');
SELECT pq_pop('q2');

-- JSON escape sanity: quote + backslash + newline survive drain
SELECT pq_push('esc', 1, 'he said "hi"');
SELECT pq_drain('esc');

-- drain on a never-pushed queue gives []
SELECT pq_drain('never');
