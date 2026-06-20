-- Smoke test for `multi-comparison`. Verifies the four correction
-- procedures (Bonferroni, Holm, BH, BY) on textbook arrays.
.load extensions/multi-comparison/target/wasm32-wasip2/release/multi_comparison_extension.component.wasm

/* ---- Bonferroni: alpha/N matches known examples.
       N=5, alpha=0.05  adjusted_alpha = 0.01. The first two p-values
       (0.001, 0.008) fall below 0.01 and are rejected; the rest are not. */
SELECT json_extract(mc_bonferroni('[0.001,0.008,0.039,0.041,0.042]', 0.05), '$.adjusted_alpha');
SELECT json_extract(mc_bonferroni('[0.001,0.008,0.039,0.041,0.042]', 0.05), '$.reject_array');

/* Bonferroni N=10, alpha=0.05  adjusted = 0.005. */
SELECT json_extract(mc_bonferroni('[0.001,0.004,0.01,0.02,0.03,0.04,0.05,0.06,0.07,0.08]', 0.05), '$.adjusted_alpha');
SELECT json_extract(mc_bonferroni('[0.001,0.004,0.01,0.02,0.03,0.04,0.05,0.06,0.07,0.08]', 0.05), '$.reject_array');

/* ---- Holm step-down: same N=5 input. Sorted ascending; reject while
       p_(i) <= alpha / (N - i + 1).
         i=1: alpha/5 = 0.01,  0.001 <= 0.01  reject  index 0
         i=2: alpha/4 = 0.0125, 0.008 <= 0.0125  reject  index 1
         i=3: alpha/3  0.0167, 0.039 > 0.0167  STOP
       Holm rejects exactly 2 (same as Bonferroni here). Sorted indices
       are [0,1,2,3,4] because the input is already sorted ascending. */
SELECT json_extract(mc_holm('[0.001,0.008,0.039,0.041,0.042]', 0.05), '$.sorted_indices');
SELECT json_extract(mc_holm('[0.001,0.008,0.039,0.041,0.042]', 0.05), '$.reject_array');

/* Holm preserves original order: shuffle the same p-values
       (orig indices 2,0,3,1,4) and confirm sorted_indices recovers the
       ascending permutation [1,3,0,2,4]. reject_array still flips the
       0.001 and 0.008 slots  now at original positions 1 and 3. */
SELECT json_extract(mc_holm('[0.039,0.001,0.041,0.008,0.042]', 0.05), '$.sorted_indices');
SELECT json_extract(mc_holm('[0.039,0.001,0.041,0.008,0.042]', 0.05), '$.reject_array');

/* ---- Benjamini-Hochberg FDR: classic BH 1995 paper example. N=15,
       q=0.05. The published answer is 4 rejections (p_(4) = 0.0095,
       p_(5) = 0.0201 > 0.0167). threshold = 0.0095. */
SELECT json_extract(mc_bh_fdr('[0.0001,0.0004,0.0019,0.0095,0.0201,0.0278,0.0298,0.0344,0.0459,0.324,0.4262,0.5719,0.6528,0.759,1.0]', 0.05), '$.threshold');
SELECT json_extract(mc_bh_fdr('[0.0001,0.0004,0.0019,0.0095,0.0201,0.0278,0.0298,0.0344,0.0459,0.324,0.4262,0.5719,0.6528,0.759,1.0]', 0.05), '$.reject_array');

/* BH at q=0.05 with the brief's small example [0.001, 0.008, 0.039,
       0.041, 0.042]. critical[i] = 0.01,0.02,0.03,0.04,0.05.
       p_(5)=0.042 <= 0.05  largest i = 5  reject ALL FIVE.
       (This is the canonical step-up rule  any p_(j) with j  i_max
       is rejected even if p_(j) > q*j/N. The brief's "4" is the BH
       1995 example above; this small array actually rejects all five.) */
SELECT json_extract(mc_bh_fdr('[0.001,0.008,0.039,0.041,0.042]', 0.05), '$.threshold');
SELECT json_extract(mc_bh_fdr('[0.001,0.008,0.039,0.041,0.042]', 0.05), '$.reject_array');

/* BH with nothing significant: all p-values are well above any critical
       value  threshold=0, reject_array all zeros. */
SELECT json_extract(mc_bh_fdr('[0.4,0.5,0.6,0.7,0.8]', 0.05), '$.threshold');
SELECT json_extract(mc_bh_fdr('[0.4,0.5,0.6,0.7,0.8]', 0.05), '$.reject_array');

/* ---- Benjamini-Yekutieli FDR (positive-dependence). Stricter than
       BH: divides critical value by c(N) = sum 1/k.
       N=15  c(N)  3.3182, denom = N*c(N)  49.77.
       critical[i] = 0.05 * i / 49.77. The first three p-values pass
       (largest k = 3); the fourth p=0.0095 > crit[4]  0.004019 fails.
       BY rejects 3 of 15  noticeably stricter than BH's 4. */
SELECT json_extract(mc_by_fdr('[0.0001,0.0004,0.0019,0.0095,0.0201,0.0278,0.0298,0.0344,0.0459,0.324,0.4262,0.5719,0.6528,0.759,1.0]', 0.05), '$.reject_array');

/* ---- multi_comparison_version: non-empty. */
SELECT length(multi_comparison_version()) > 0;

/* ---- NULL / out-of-domain inputs propagate to NULL. */
SELECT mc_bonferroni(NULL, 0.05);
SELECT mc_bonferroni('not json', 0.05);
SELECT mc_bonferroni('[]', 0.05);                  -- empty array
SELECT mc_bonferroni('[0.5, -0.1]', 0.05);         -- out-of-[0,1]
SELECT mc_bonferroni('[0.5, "x"]', 0.05);          -- non-numeric
SELECT mc_holm('[0.1,0.2]', 1.5);                   -- alpha > 1
SELECT mc_bh_fdr('[0.1,0.2]', NULL);
