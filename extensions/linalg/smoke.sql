-- Smoke test for `linalg`. Each SELECT maps to a PLAN-more-extensions-5
-- #5 acceptance point or to a documented NULL-propagation path.
.load extensions/linalg/target/wasm32-wasip2/release/linalg_extension.component.wasm

/* ---- Constructors. */
SELECT la_zeros(2, 3);                            -- [[0,0,0],[0,0,0]]
SELECT la_eye(2);                                 -- [[1,0],[0,1]]
SELECT la_eye(3);                                 -- [[1,0,0],[0,1,0],[0,0,1]]

/* ---- Shape + transpose. */
SELECT la_shape('[[1,2,3],[4,5,6]]');             -- [2,3]
SELECT la_transpose('[[1,2,3],[4,5,6]]');         -- [[1,4],[2,5],[3,6]]

/* ---- Element-wise add / sub. */
SELECT la_add('[[1,2],[3,4]]', '[[10,20],[30,40]]');   -- [[11,22],[33,44]]
SELECT la_sub('[[10,20],[30,40]]', '[[1,2],[3,4]]');   -- [[9,18],[27,36]]

/* ---- Matrix multiply (NOT elementwise).
       PLAN: la_mul('[[1,2],[3,4]]', '[[5,6],[7,8]]') == '[[19,22],[43,50]]'. */
SELECT la_mul('[[1,2],[3,4]]', '[[5,6],[7,8]]');

/* Non-square multiply: (2×3) × (3×2) = (2×2). */
SELECT la_mul('[[1,2,3],[4,5,6]]', '[[7,8],[9,10],[11,12]]');  -- [[58,64],[139,154]]

/* ---- Scale. */
SELECT la_scale('[[1,2],[3,4]]', 2);              -- [[2,4],[6,8]]
SELECT la_scale('[[1,2],[3,4]]', 0.5);            -- [[0.5,1.0],[1.5,2.0]]

/* ---- Determinant.  PLAN: la_det('[[1,2],[3,4]]') == -2. */
SELECT la_det('[[1,2],[3,4]]');                   -- -2.0
SELECT la_det('[[1,0,0],[0,1,0],[0,0,1]]');       -- 1.0
SELECT la_det('[[2,0,0],[0,3,0],[0,0,4]]');       -- 24.0

/* ---- Inverse.  PLAN: la_inverse('[[1,0],[0,1]]') == '[[1,0],[0,1]]'. */
SELECT la_inverse('[[1,0],[0,1]]');               -- [[1,0],[0,1]]
SELECT la_inverse('[[2,0],[0,4]]');               -- [[0.5,0],[0,0.25]]
SELECT la_inverse('[[1,2],[2,4]]');               -- singular -> NULL

/* ---- Solve Ax = b.  PLAN: identity, [1,2] -> [1,2]. */
SELECT la_solve('[[1,0],[0,1]]', '[1,2]');        -- [1,2] (1D mirrors 1D)
SELECT la_solve('[[2,0],[0,2]]', '[2,4]');        -- [1,2]
/* 2D b form returns 2D x. */
SELECT la_solve('[[1,0],[0,1]]', '[[1],[2]]');    -- [[1],[2]]

/* ---- Rank. */
SELECT la_rank('[[1,0],[0,1]]');                  -- 2
SELECT la_rank('[[1,2],[2,4]]');                  -- 1 (rows linearly dependent)
SELECT la_rank('[[0,0],[0,0]]');                  -- 0

/* ---- Trace.  PLAN: la_trace('[[1,2],[3,4]]') == 5. */
SELECT la_trace('[[1,2],[3,4]]');                 -- 5.0
SELECT la_trace('[[1,0,0],[0,2,0],[0,0,3]]');     -- 6.0

/* ---- Frobenius norm.  PLAN: la_norm('[[3,4]]', 'fro') == 5. */
SELECT la_norm('[[3,4]]', 'fro');                 -- 5.0
SELECT la_norm('[[3,4]]');                        -- 5.0 (default 'fro')
SELECT la_norm('[[1,-2],[3,-4]]', 'l1');          -- 6.0 (col 2 sums to 6)
SELECT la_norm('[[1,-2],[3,-4]]', 'linf');        -- 7.0 (row 2 sums to 7)

/* ---- Eigenvalues. Diagonal matrix -> diagonal entries (order = natural). */
SELECT la_eigvals('[[2,0],[0,3]]');               -- [{re:3,im:0},{re:2,im:0}] (Schur order)

/* ---- NULL / malformed propagation. */
SELECT la_zeros(0, 3);                            -- non-positive dim
SELECT la_eye(-1);                                -- non-positive dim
SELECT la_add('[[1,2]]', '[[1,2,3]]');            -- shape mismatch
SELECT la_mul('[[1,2]]', '[[1,2]]');              -- inner-dim mismatch
SELECT la_det('[[1,2,3],[4,5,6]]');               -- non-square
SELECT la_inverse('[[1,2,3],[4,5,6]]');           -- non-square
SELECT la_solve('[[1,2,3],[4,5,6]]', '[1,2]');    -- non-square A
SELECT la_solve('[[1,0],[0,1]]', '[1,2,3]');      -- shape mismatch
SELECT la_inverse('[[1,2],[2,4]]');               -- singular
SELECT la_trace('[[1,2,3],[4,5,6]]');             -- non-square
SELECT la_norm('[[1,2]]', 'l2');                  -- unknown kind
SELECT la_transpose('not json');                  -- malformed
SELECT la_transpose('[[1,2],[3]]');               -- ragged rows
SELECT la_transpose(NULL);                        -- NULL input

/* ---- Version string non-empty. */
SELECT length(linalg_version()) > 0;
