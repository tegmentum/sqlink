# Lessons learned: extension porting

A running retrospective on each extension implementation. Add an
entry at the BOTTOM after every ship.

Pattern per entry:

    ### YYYY-MM-DD  <extension-name>

    **What I built:** one-line summary.

    **What worked:** the parts that felt fast / where the tooling
    paid off.

    **What surprised me:** API gotchas, crate quirks, build flags,
    smoke output anomalies. The compat-registry should grow
    proportionally.

    **Tooling opportunity:** if a friction point felt repeatable,
    name it. Periodically batch-review these to find what to
    automate next.

Read this file end-to-end before starting a new extension if you
haven't shipped one recently  the entries are concrete enough to
trigger "oh right, X" recognition that prevents rediscovery cost.

---

## Retrospectives

### 2026-06-17  tooling commit (scaffold + smoke + skill)

**What I built:** PLAN-extension-tooling.md + tooling/{scaffold,
smoke, plan-add}.py + tooling/{templates, compat-registry.json}
+ Makefile `ext` target + `.claude/commands/new-extension.md`.

**What worked:**
- Seeding compat-registry with ~40 entries from this session's
  prior work captured real institutional knowledge that would
  otherwise live only in commit messages.
- `cargo check` as part of the scaffold catches feature-flag
  mistakes immediately. Caught eui48 within seconds (see mac
  entry below).
- The Makefile `ext` target wraps the 4-step sequence cleanly;
  ~3-4s wall time for small extensions.

**What surprised me:**
- The cli's SQL parser fuses leading `--` comments into the
  next dot-command and chokes on `.load`. smoke.py strips them
  before piping; the smoke.sql template still ships them at the
  top, which is misleading.
- The scaffold's manifest closure hardcodes
  `let det = FunctionFlags::DETERMINISTIC;` then passes it
  unconditionally. Nondeterministic scalars (faker) need to
  swap this  awkward.

**Tooling opportunity:**
- (T-1) Move `-- comment` lines in smoke.sql.tmpl AFTER the
  `.load` line so they don't confuse anyone reading the
  template directly.
- (T-2) Improve the manifest closure to take a flags arg:
  `let s = |id, name, n, flags| ...;` so per-scalar flag
  choice is explicit at the call site. Both `det` and `nd`
  available at top of describe().

---

### 2026-06-17  mailto

**What I built:** 7-scalar RFC 6068 mailto: URI parser via the
`url` crate (validate / to / subject / body / cc / bcc /
recipients).

**What worked:**
- `url::Url::parse(s)?.filter(|u| u.scheme() == "mailto")` is a
  clean two-line scheme gate.
- The url crate's `query_pairs()` decodes percent-encoded values
  for free; no manual urldecode needed.
- compat-registry had `url` flagged clean so the scaffold's
  Cargo.toml was right out of the gate.

**What surprised me:**
- mailto: recipient lives in `u.path()`, NOT `u.host()`. Easy
  to get wrong if you're used to http URIs.
- Multiple primary recipients are comma-separated WITHIN the
  path (e.g. `mailto:a@x,b@y`)  manual split + trim needed.
  Plus `to=` query params can also carry recipients (RFC 6068).
- Needed to add `serde_json` to deps after using it for the
  recipients array. The scaffold didn't pre-add it; that's
  probably correct (not every ext uses JSON), but worth noting.

**Tooling opportunity:**
- (T-3) When a `--crate` arg points at a clean entry in
  compat-registry whose `notes` mention "transitively pulls X",
  the scaffold could surface that so the user knows what's
  coming size-wise.

---

### 2026-06-17  mac

**What I built:** 7-scalar MAC address (EUI-48) toolkit. Validate
/ normalize / OUI / NIC / multicast-bit / local-bit / format.
Hand-rolled  no crate.

**What worked:**
- The scaffold's `cargo check` step caught `eui48`'s broken
  transitive (`rustc-serialize` 0.3.25 E0046) BEFORE I wrote any
  scalar code. Saved ~15 min of dead-end debugging.
- Pivoted to hand-roll in ~30 LOC: filter hex digits, parse
  pairs, format with separator. Faster than fighting any crate.
- Reused the same `arg_text` first-arg pattern across all 7
  scalars; only `mac_format` needs the second arg.

**What surprised me:**
- The crate ecosystem for "parse a MAC address" is actually
  pretty weak. `eui48` is the obvious pick but transitively
  unbuildable; `mac_address` is for host-interface enumeration
  (not parsing). Hand-rolling won.
- The IEEE OUI/NIC split (3+3 bytes) is well-defined but the
  multicast and local-administered bits aren't well-known
  outside networking circles  worth a comment in the code.

**Tooling opportunity:**
- (T-4) Add a `status: "hand-roll-preferred"` to the
  compat-registry schema, so the next person hitting "I want
  X functionality" sees the recommendation up front instead
  of rediscovering the crate dead-end.

---

### 2026-06-17  faker

**What I built:** 14-scalar test-data generator using the `fake`
crate. name / first_name / last_name / email / safe_email /
username / password / ipv4 / phone / company / street / city /
country / zip.

**What worked:**
- The `fake` crate's API is consistent: every faker type is a
  unit struct, call `.fake()` to materialize. Once you have one
  scalar wired, the rest are paste-and-rename.
- En-locale paths are uniform: `fake::faker::name::en::Name`,
  `fake::faker::internet::en::Email`, etc.
- 14 scalars in maybe 5 minutes of editing  the tooling really
  shines when each scalar is a one-liner.

**What surprised me:**
- The scaffold's manifest closure defaults to
  `FunctionFlags::DETERMINISTIC`. faker is NOT deterministic 
  every call produces fresh output. I had to redefine the
  closure to use `FunctionFlags::empty()` (renamed `det`  `nd`
  conceptually). Easy to forget for caller-perspective extensions
  (faker, lorem, ids).
- The scaffold's `arg_text` helper is unused here  faker
  scalars are 0-arg. Compiler is fine with `#[allow(dead_code)]`
  on the helpers, but they still occupy ~15 LOC visually.

**Tooling opportunity:**
- (T-2 reinforced) The flags-arg manifest closure improvement
  would have made faker trivial: just pass `nd` instead of
  swapping the entire closure body.
- (T-5) If the user passes `--nondeterministic` at scaffold
  time, the template could ship with the closure already set
  up for `nd` and an example FID using it.

---

## Batch review  tooling actions to take next

After the four entries above (tooling commit + 3 plugins):

| ID | Friction | Fix |
|---|---|---|
| T-1 | smoke.sql.tmpl ships top-of-file `--` comments that confuse readers (smoke.py strips them, but the template's misleading) | Move comments after the `.load` line, or switch to `/* block */` syntax |
| T-2 | Manifest closure hardcodes `det`; nondet scalars need redefinition | Pass flags as a closure arg; pre-bind both `det` and `nd` at top of describe() |
| T-3 | Some clean crates pull large transitives (e.g. `url`  `idna`  `icu_*`). User can't see the size impact at scaffold time | Add a `pulls` field to compat-registry; surface in scaffold output |
| T-4 | Crates with no good alternative get rediscovered every time | New compat-registry status: `hand-roll-preferred` with a one-line "why" |
| T-5 | Nondet extensions are awkward to spell with the current template | `--nondeterministic` flag on scaffold.py that prebinds `nd` |

T-1, T-2, T-4 are 15-min fixes. T-3 and T-5 are 30-60 min each.
Will apply T-1 + T-2 + T-4 inline before continuing.

T-1 + T-2 + T-4 landed in the next commit alongside vin.

---

### 2026-06-17  vin

**What I built:** 7-scalar VIN (ISO 3779) toolkit. validate /
check_digit / wmi / vds / vis / model_year / region. Hand-rolled
(no crate)  ~120 LOC including the weights table and
transliteration map.

**What worked:**
- The improved template (`det` + `nd` available, closure takes
  flags) felt cleaner. Pass `det` per-scalar instead of having
  it implicit. Visually less surprising.
- The `/* */` block comment in smoke.sql.tmpl meant my real
  smoke.sql could keep prose at the top without the cli parser
  choking on it.
- Hand-rolling beat looking for a VIN crate. There's no
  dominant Rust VIN crate; the algorithm is well-defined in
  ~50 LOC. Same pattern as MAC.

**What surprised me:**
- The cli emitted "Error: out of memory" on a single
  `vin_check_digit` call during the first smoke run, then the
  exact same statement succeeded on a re-run. Transient; not
  reproducible. Suspected per-statement state accumulation in
  the cli wasm  worth flagging if it recurs.
- My first smoke.sql used a "real-ish" VIN with a hand-typed
  check digit that didn't actually compute  the validate
  scalar correctly returned 0. Lesson: use Wikipedia's
  algorithm example (1M8GDM9AXKP042788, check digit X) as the
  canonical happy-path sample for any algorithm-with-check-digit
  validator I write going forward.
- The post-2010 model-year code cycle includes BOTH letters
  AND digits (letters 2010-2030, digits 1-9 for 2031-2039).
  My code handles this correctly but it's subtle  caller
  inspecting a "model year" that returns 2033 from a placeholder
  digit might be confused.

**Tooling opportunity:**
- (T-6) Curated "known-good test inputs" for validation
  algorithms. Several extensions already use canonical
  Wikipedia / RFC samples (ISBN's "9780198526636", base32's
  "JBSWY3DP", VIN's "1M8GDM9AXKP042788"). A
  `tooling/canonical-samples.md` would let the next person
  porting an algorithm pick up these references without re-
  Googling. Would also serve as smoke.sql seeds.
- (T-7) Smoke harness could optionally assert outputs by
  matching against a `smoke.expected` file. Catches regression
  where the algorithm subtly drifts (e.g. weight tables get
  swapped). Currently smoke.py only catches "did anything
  panic" failures; this would catch behavior drift.

---

### 2026-06-17  creditcard

**What I built:** 6-scalar credit card BIN-range type detection
+ Luhn-validate + mask + last4 + BIN + normalize. Hand-rolled.
Covers visa / mastercard / amex / discover / jcb / diners /
unionpay / maestro.

**What worked:**
- Reused the canonical ISO 8583 test cards (4111111111111111,
  378282246310005, etc.) from memory  these are well-known
  enough that I didn't have to look up "what's a Luhn-passing
  Visa." That's exactly the T-6 canonical-samples doc I
  flagged in the vin entry. Worth shipping.
- The hand-roll pattern was again fast: BIN ranges +
  digit-count tests fit in one big `brand()` fn. ~100 LOC
  total for the parser including the Luhn helper.
- The new manifest closure pattern (`s(..., det)`) reads
  cleaner than the old implicit-deterministic shape. The
  6 scalars line up nicely.

**What surprised me:**
- I almost duplicated `parsers.luhn_check` — the parsers
  extension already has Luhn. Inlining a one-fn copy here is
  fine (no cross-extension dispatch), but worth noting: the
  Luhn helper is now in TWO places. If a bug were found in one,
  the other might lag.
- BIN-range tables are surprisingly hairy. Mastercard alone
  has two disjoint ranges (51-55 AND 2221-2720); Discover has
  3 (6011, 65, 644-649); JCB is 3528-3589. Easy to typo or
  swap. The `smoke.expected` (T-7) testing pattern would
  catch a swap immediately.

**Tooling opportunity:**
- (T-6 reinforced) The canonical-samples doc is now wanted by
  both vin and creditcard. Worth ~30 min to write up.
- (T-8) `tooling/shared-helpers.rs` or similar  a way to
  share small algorithms (Luhn, hex decode, etc.) across
  extensions WITHOUT pulling each into a separate crate. Could
  be a `tooling/snippets/` directory with files the scaffold
  optionally appends. Or a single `extensions-common` crate
  the workspace pulls in. The latter is cleaner long-term but
  adds workspace complexity; the former is faster.

---

### 2026-06-17  tooling commit (canonical samples doc)

**What I built:** tooling/canonical-samples.md  reference doc
of known-good test inputs for the algorithm-validators we've
shipped (ISBN, VIN, base32/base58, ISIN, BIC, etc.) plus a few
cross-extension ones (canonical SHA-256, BPE token sequence,
common timestamps).

**What worked:**
- Walking my own ship history was the right way to seed this 
each canonical sample is verifiably anchored (Wikipedia,
RFC, ISO standard, manufacturer test card).
- Doc fits in one screen; not too long to skim before
choosing a sample for new smoke.sql.

**What surprised me:** nothing  this was clean scope.

**Tooling opportunity:** none new. T-6 closed.

---

### 2026-06-17  isin

**What I built:** 4-scalar ISIN (ISO 6166) validator. validate /
check_digit / country / nsin. Hand-rolled  ~60 LOC including
the letter-expansion table and modulo-10 check.

**What worked:**
- Used the brand-new canonical-samples.md to pick test cases:
Apple (US0378331005), Tesla (US88160R1014), BMW
(DE0005190003). All verified-correct check digits  exactly
the use case T-6 was supposed to solve.
- The algorithm shape is "expand alphanumeric to digits, then
Luhn." Reused the same Luhn helper shape as creditcard 
which is now in THREE places (parsers, creditcard, isin).
- 4 scalars (validate + 3 decomposers) fits the pattern of
the other identifier extensions (isbn, vin) cleanly. Same
ergonomics  caller knows what to expect.

**What surprised me:**
- The transient "Error: out of memory" recurred  same shape
as the vin smoke test, single scalar SELECT after several
earlier ones, then the correct result on a retry. This is a
real cli quirk (probably per-statement memory accumulation in
the wasm component cache), not extension-specific.
- The Luhn variant for ISIN is slightly different from the
classic credit-card Luhn: the first "multiply by 2" position
is the RIGHTMOST digit (after expansion), not the second-from-
right. My code handles this correctly via `let mut alt = true`
initially, but it's worth a comment for future-me. Real source
of bugs in other Luhn impls.

**Tooling opportunity:**
- (T-8 reinforced) Luhn helper now duplicated in THREE places.
This is the line where copy-paste should give way to either
a `tooling/snippets/luhn.rs` or an `extensions-common` crate.
- (T-9) The transient OOM appearing in 2/2 smoke tests with
the same shape (single SELECT scalar after several
multi-arg-ish statements) might be a real cli bug. Worth a
bisect: does it happen on the host-side `dispatch_scalar`
path, or only after the cli wasm has been running for several
statements? Note it; revisit when it gets in the way.

---

### 2026-06-17  aba (US bank routing)

**What I built:** 3-scalar ABA RTN validator + FRB district +
fed region. Hand-rolled (~70 LOC) including the district-range
folding (01-12 normal, 21-32 thrift -20, 61-72 electronic -60,
80 traveler's cheques).

**What worked:**
- Picked a real-world canonical sample (021000021 = JPMorgan
Chase NY) for smoke; verified it validates. Added to
canonical-samples.md  reinforces T-6.
- The weighted-check (3,7,1,3,7,1,3,7,1) is a one-liner
zip-and-multiply  smaller than Luhn. No need for an
ABA-specific snippet; the `weighted_mod10` helper in
tooling/snippets/luhn.rs generalizes this.
- FRB district mapping is a small lookup table  fits in 20
LOC. Bonus value over just validate is real (callers
reporting on "where is this routing number's bank").

**What surprised me:**
- The OOM is escalating. ABA smoke had 3 occurrences (out of
7 statements), all on the SELECT-after-multi-statement
pattern. Outputs were correct on each retry. The cli
recovers gracefully but the noise in smoke logs is annoying.
Real T-9 work needed: bisect the cli's wasm for per-statement
memory accumulation.

**Tooling opportunity:**
- (T-8 closed) Snippets directory landed this commit:
`tooling/snippets/luhn.rs` covers the three Luhn variants
(classic, ISIN-style, weighted). README explains the
paste-and-own model + tradeoff vs. a shared crate.
- (T-9 reinforced) The OOM pattern hits every smoke run with
the right shape. Priority is escalating from "annoying" to
"noisy." Bisect when it gets in the way of a real port.


