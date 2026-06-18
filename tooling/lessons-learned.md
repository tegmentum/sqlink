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

---

### 2026-06-17  T-9 investigation (cli misleading OOM)

**What I built:** root-cause + 1-line fix in core/src/db.rs +
6-line fix in cli/src/lib.rs for the "Error: out of memory"
spam in multi-statement smoke runs.

**What I found:** the OOM message was misleading. Real cause:

1. SQL like `SELECT 1; -- comment\n` reaches the cli.
2. After the `;`, the cli's eval loop sees `tail = " -- comment\n"`
   as the remainder.
3. `prepare_with_tail(" -- comment\n")` calls
   `sqlite3_prepare_v2`, which returns SQLITE_OK with
   stmt=NULL (per SQLite docs: comment-only input produces no
   stmt).
4. The Rust wrapper happily constructs a `Statement{raw: NULL}`
   and the eval loop calls `stmt.collect_rows()` on it.
5. `collect_rows  step  sqlite3_step(NULL)` returns
   SQLITE_MISUSE.
6. Error path calls `sqlite3_db_handle(NULL)  sqlite3_errmsg(NULL)`.
7. **Per SQLite documentation**: `sqlite3_errmsg(NULL)` returns
   the static string "out of memory".

So the OOM was actually "you called step() on a null stmt
pointer" wearing a misleading mask.

Repro shrinker: started from a 7-statement smoke that hit OOM
3x. Bisected via heredoc-vs-pipe difference (turned out the
trigger was inline `-- xxx` comments after `SELECT ...;`, NOT
block comments). Confirmed by stripping inline comments 
clean output.

**The fix:**

  - `core/src/db.rs`: add `Statement::is_empty() -> bool` that
    returns true iff `self.raw.is_null()`. Annotated with the
    full rationale.
  - `cli/src/lib.rs::eval_sql_inner`: after `prepare_with_tail`,
    check `stmt.is_empty()`. If true, skip to the tail without
    stepping. Restores correct comment-only handling.

After fix: smoke.py --all on all 11 extensions PASS, no OOM
in any output.

**Tooling opportunity:**
- (T-9 closed) The cli bug is fixed in this commit. Removed
the smoke.py defense from being misclassified  the OOM was
never an extension issue.
- (T-10 new) The smoke.py heuristic looks for the literal
string "Error: out of memory" as a failure marker. Now that
we know that string was a misleading SQLite-internal message,
remove it from the failure list  the real failure signatures
are "Error loading", "no such function", "panicked",
"instantiate loaded ext".

---

### 2026-06-17  bic (ISO 9362 SWIFT codes)

**What I built:** 7-scalar BIC validator + decomposer. Hand-
rolled, ~80 LOC. Validate / bank / country / location / branch
/ is_primary / is_test.

**What worked:**
- Structural validation only (no check digit on BIC) means
the code is just: filter whitespace, uppercase, count chars,
verify each region's char class. Tiny.
- T-1's `.load` first / block comment after was clean. The
T-9 fix means smoke.sql with multi-line block comments
runs without ghost OOMs.
- ISO 9362's quirks (8-char vs 11-char form, "XXX" branch =
primary office, '0' second-char-of-location = test BIC) are
exactly the kind of domain trivia that belongs in scalars
rather than every consumer reimplementing.

**What surprised me:**
- I made my OWN test-BIC fake (TEST0FRPP plus XXX = 12 chars,
1 over) on the first pass. Validator correctly returned 0;
result was a confusing "0 returned for the is_test case".
Lesson: count the chars before typing a test input. Same
mistake-class as the vin one (made up a check digit), but
now caught by smoke output inspection rather than a
debugger.
- ISO 9362 test-BIC convention is position-7 (second char of
location code) = '0'. Not widely known; worth documenting
in code comments  did so.
- The validator-then-extract pattern is repeated across all 7
scalars: `if valid { extract } else { NULL }`. Same shape as
vin, isin, mac. Could be factored, but each extension's
extraction logic differs enough (slice positions, formats)
that a generic factor would be lossy.

**Tooling opportunity:**
- (T-11 new) Smoke harness could detect "all-empty output"
from a scalar that should produce text  catches my
test-BIC-12-char-typo class of error WITHOUT needing
output assertions (T-7). Heuristic: count NULL/empty rows
following a SELECT; if all rows in a 5+ SELECT batch are
empty after a known-good validator returns 1, flag it.
Lightweight version of T-7's full expected-output
assertion.

---

### 2026-06-17  ean (EAN-13 / UPC-A / EAN-8)

**What I built:** 4-scalar barcode validator + decomposer +
upca_to_ean13 cross-converter. Uses the `weighted_mod10`
snippet from `tooling/snippets/luhn.rs`  first actual user
of the snippets directory.

    ean_validate(text)     INTEGER  1 if 8/12/13-digit + check ok
    ean_check_digit(text)  INTEGER  computes the 13th digit from
                                      a 12-digit prefix
    ean_gs1_prefix(text)   INTEGER  first 3 digits (country/region)
    upca_to_ean13(text)    TEXT     '0' + 12-digit UPC = 13-digit EAN

**What worked:**
- Inlining the snippet via copy-paste was as simple as the
README promised  ~10 LOC for `weighted_mod10`, marked with
"// --- snippet: tooling/snippets/luhn.rs ---" delimiters so
future-me knows where to look for updates.
- EAN-13/UPC-A/EAN-8 all use the same weighted-mod-10 shape
with different weights tables  the snippet generalizes
cleanly.
- T-9 fix payoff is huge: smoke output is now CLEAN. 9 SELECTs,
9 clear answers, zero ghost OOM lines.

**What surprised me:**
- EAN-13 weight table is `1,3,1,3,...` from position 0;
UPC-A is `3,1,3,1,...` from position 0. Subtle but matters.
Documented inline in the const arrays.
- The check_digit formula is `(10 - (sum%10)) % 10`  the
outer mod-10 catches the case where sum%10 = 0 (check
digit = 0, not 10). Easy to drop the outer mod and get bug.

**Tooling opportunity:**
- (T-12 new) The snippet pasting pattern is working but the
"// --- snippet: ... ---" delimiter is just a convention.
A `tooling/check-snippets.py` could grep for those delimiters,
diff the inlined copy against the source snippet, flag drift.
Wouldn't enforce  just surfaces "this extension's pasted
copy is now stale." Low effort, high value if a snippet ever
gets a bugfix.

---

### 2026-06-17  postcode (multi-country postal code)

**What I built:** 4-scalar postal-code validator covering 9
countries (US, UK/GB, CA, DE, FR, JP, NL, AU, BR) via per-country
anchored regexes. Validate / detect_country / validate_country /
normalize.

**What worked:**
- The `regex` crate was already in the compat-registry as
clean (used by `regexp` extension). Scaffold consumed it
cleanly.
- `std::sync::OnceLock` for the per-country compiled regex is
cheaper than lazy_static! and avoids the macro dep. One
declaration per country, ~12 LOC.
- The "try all countries in order, return first match" pattern
in `detect()` works because UK / CA / JP / NL / BR all have
distinctive prefixes/structure that don't overlap with the
digit-only ones (US, DE, FR, AU). Documented the ordering
constraint in a comment.

**What surprised me:**
- The Canadian postal code regex is hairier than expected.
Real CA codes use specific subsets of letters in each slot
(D, F, I, O, Q, U are excluded; W and Z are restricted in
the first position). The pattern landed at
`[A-CEGHJ-NPRSTVXY][0-9][A-CEGHJ-NPRSTV-Z] ?[0-9][A-CEGHJ-NPRSTV-Z][0-9]`
 dense but mechanical.
- UK GIR 0AA (the Girobank exception) is a real historical
quirk that real-world validators still need to accept.
Added it as an alternation.
- Order in `detect()` matters: US's `\d{5}` would accept
"12345" and shadow Australia's `[0-9]{4}` if we tried US
first. Listed digit-only countries LAST.

**Tooling opportunity:**
- (T-13 new) The "try all countries in order" pattern for
detect_*-style scalars could be a helper-snippet. Right
now my `detect()` has the ordering hardcoded in the
extension. If we ship more detect-classifier extensions
(MAC vendor lookup by OUI, phone region by prefix, etc.),
the same shape repeats: `for cc in candidates { if
matches { return Some(cc) } }`. A 5-line helper isn't
worth a snippet alone but would compound if 3+
extensions copy it.

---

### 2026-06-17  escape (URL / HTML / SQL / shell)

**What I built:** 6-scalar text-escaping extension. Hand-rolled,
~150 LOC. url_encode / url_decode (RFC 3986 percent-encoding +
form-urlencoded `+`), html_escape / html_unescape (5 named +
numeric &#NN; / &#xHH; entities), sql_quote (SQLite-style
double-quote escaping), shell_quote (POSIX single-quote safe).

**What worked:**
- Each scalar is independent and small (~20-30 LOC each); the
file is mostly readable as 6 separate algorithms.
- HTML unescape's numeric-entity path (`&#65;`  'A', `&#x43;`
 'C') was a fun bit  uses `char::from_u32` to convert.
- shell_quote uses the canonical `'...'` wrap with `'\''`
substitution for embedded apostrophes  the only POSIX-safe
quoting for arbitrary text.

**What surprised me:**
- HTML unescape needs to handle 5 named entities (amp, lt, gt,
quot, apos/&#39;) PLUS numeric entities PLUS pass-through
for unknown sequences. The full set is much bigger (~250
named entities like &copy; &deg; etc.) but the 5 + numeric
covers ~95% of real-world content. Documented the
limitation in the source.
- `core::fmt::write(&mut out, format_args!("%{:02X}", b))` is
the no-std way to do hex formatting into a String. Cleaner
than `out.push_str(&format!(...))` (no temp allocation) but
syntax is heavier.

**Tooling opportunity:**
- (T-14 new) The HTML entity table (the named entities I
chose plus the long tail) could be its own snippet  the
tradeoff is "include the long table (~250 entries, ~5KB
source) for completeness" vs "ship the common-5 and tell
callers to chain through markdown for full rendering."
Defer until a caller asks for the long tail.

---

### 2026-06-17  uuid (parse + version + timestamp augmentation)

**What I built:** Augmented the existing 3-scalar uuid extension
with 5 more parse/inspect scalars: validate, version, nil,
timestamp_ms, variant. uuid is now 8 scalars.

**What worked:**
- Augmentation pattern is by now well-established (see text-nlp,
ids, web-parsers). Just add FIDs + match arms + manifest
entries. No new deps.
- The `uuid` crate exposes parse_str() + get_version_num() +
get_timestamp() + get_variant() cleanly  exact API I needed.
- T-2's flags closure became important here: nil() and the
inspect functions are DETERMINISTIC; the generators are
nondet. Pre-bound `det` + `nd` made the manifest a single
visual scan.

**What surprised me:**
- The Makefile `ext` target broke on uuid because uuid is a
top-level workspace member (built into target/wasm32-wasip2/
release/), whereas scaffolded extensions declare `[workspace]`
in their Cargo.toml and land artifacts under extensions/<name>/
target/. Pure mechanical mismatch  not a code bug, just a
build-output-location bug.
- The fix is "look in both places, but always WRITE the
.component.wasm at the per-extension path" so smoke.sql's
`.load extensions/<name>/target/...` line stays stable for
both extension classes.

**Tooling opportunity:**
- (T-15 closed inline) Makefile `ext` target now handles both
workspace-member and scaffolded extensions. Fixed in this
commit. Side-effect benefit: smoke.sql `.load` paths are now
guaranteed-uniform across extension classes.

---

### 2026-06-17  T-16 investigation (scaffold + ext speedup)

**What I built:** Shared `CARGO_TARGET_DIR` for scaffolded
extensions  build artifacts (wit-bindgen, serde, etc.) shared
across the catalog instead of recompiled per extension.

**What I found:** every scaffolded extension declares
`[workspace]` in its Cargo.toml (so cargo treats it as its
own root). That meant every scaffold's `cargo check` step AND
every `make ext` rebuilt the full dep tree from scratch.

Timings measured:

  scaffold (no shared target):
    cold cache:  49.8s
  scaffold (shared CARGO_TARGET_DIR=extensions/_shared-target):
    cold cache:  49.8s   (first scaffold still pays)
    second:      29.6s
    steady-state: 3.3s   (94% reduction)

  make ext (no shared target):  ~33s per build
  make ext (shared target):     17.5s fresh / 14.3s rebuild
                                (50% reduction)

The 50% reduction on `make ext` is because cargo build (vs
check) still does some optimization work even when the source
is unchanged, but most of the wit-bindgen + serde compile is
shared.

**The fix:**

  tooling/scaffold.py  set env[CARGO_TARGET_DIR] to
                        extensions/_shared-target before
                        spawning cargo check.

  Makefile             EXT_SHARED_TARGET var; `make ext` sets
                        CARGO_TARGET_DIR when the extension is
                        scaffolded ([workspace] declared) AND
                        looks in the shared dir first when
                        finding the build artifact.

  provenance/scan.py    skip any extensions/_* dir  the
                        leading underscore is the convention
                        for tooling-managed dirs that look
                        like extensions but aren't.

  .gitignore            extensions/_shared-target/

**What surprised me:**
- Scaffolded extensions can NOT just drop their `[workspace]`
declaration  if they did, cargo would try to join the
parent sqlite-wasm workspace, which has different deps and
build profiles. The `[workspace] {}` empty table is load-
bearing.
- The .cargo/config.toml-based approach (per-extension config
setting target dir) would also work but requires writing one
extra file per scaffold. The env var path is one-line.
- I almost forgot the provenance/scan.py filter  the shared-
target dir was being categorized as a non-extension on each
scan run. Annoying noise.

**Tooling opportunity:**
- (T-16 closed inline) The speedup is the fix.
- (T-17 new) `make ext-smoke-all` would benefit from a
parallel mode  smoke tests are independent. Currently it's
serial; ~85 extensions × 3-5s each = 4-7 minutes. A
`--parallel` flag (`concurrent.futures.ProcessPoolExecutor`)
would cut this to under a minute.

---

### 2026-06-17  T-7 investigation (smoke output assertions)

**What I built:** Optional `extensions/<name>/smoke.expected`
files; smoke.py diffs the parsed cli output against them when
present. Backward-compatible (no file = panic-only as before).

**What I found:** the existing panic-only smoke check only
catches "did anything crash." A Luhn helper that silently
returns wrong values (or a typoed BIN range in creditcard, or
the AS-Australia regex matching US 5-digit ZIPs) would PASS
because validators still return 0/1; just wrong values.

**The design:**

  smoke.expected format  one line per SELECT result, in
  smoke.sql order:
    plain value     exact match
    ~~             skip (nondet  rng / time-of-call /
                    fresh-each-call generators)
    ?               any non-empty value (output exists but
                    value varies, e.g. fake_name())
    leading `#`     comment, ignored

  smoke.py changes:
    parse_results(stdout)        strip cli prompts + Loaded line
    parse_expected(file)         strip comments + blank lines
    compare(actual, expected)    diff with ~~ / ? semantics

  --show-parsed <NAME> flag      print parsed rows for seeding
                                  smoke.expected files
  --list now shows "[asserted]"  for extensions with
                                  smoke.expected.

**Seeded smoke.expected for 7 validator extensions:** aba, bic,
creditcard, ean, isin, ssn, vin. These are deterministic
algorithm-with-check-digit shapes where a silent regression
would be the biggest concern. Verified:

  - `tooling/smoke.py --all` PASSes on all 18 extensions
    (7 asserted, 11 panic-only)
  - Bug-injection test: tampered isin smoke.expected to
    expect '9' where actual is '5'. smoke.py correctly FAILed
    with line-level diff: `row 6: expected '9', got '5'`.

**What surprised me:**
- Strip-the-cli-prompts parsing is fragile. Block comments in
smoke.sql get buffered into prompt continuations; the regex
`^(sqlite>\s*|\s*\.\.\.>\s*)+` handles the chained case but
required iteration to nail down.
- The "Loaded extension:" line is the smoke prelude that ISN'T
a SELECT result. Easy to forget; parser explicitly skips it.
- NULL outputs from a SELECT render as empty lines in the
cli's output, which my parser drops. That's the right
default for "empty smoke output is not a row" but means
smoke.expected needs to NOT have empty lines representing
NULL  use a sentinel? For now I just don't seed NULL-row
extensions; they have a "?" wildcard if needed.

**Tooling opportunity:**
- (T-7 closed) Inline.
- (T-18 new) smoke.expected files need to be rebuilt when
the corresponding smoke.sql changes (added/removed
statements, reordered, etc.). A `tooling/check-smoke.py`
that detects "smoke.sql changed but smoke.expected has the
old N rows" would warn before a CI surprise. Cheap (~30 LOC).

---

### 2026-06-17  T-18 investigation (smoke.expected staleness)

**What I built:** `count_smoke_selects(path)` + `staleness(name)`
helpers in smoke.py. `--list` annotates `[asserted, STALE]`
when smoke.sql's SELECT count doesn't match smoke.expected's
row count. Live smoke runs prepend a `WARN: ...` line when
stale.

**What I found:** Two things, one as expected and one not.

  1. The expected: dropping the staleness check on existing
     smoke.expected files surfaced a real false negative I
     would have shipped silently. creditcard's smoke.expected
     had 11 rows but smoke.sql has 12 SELECTs. Counts off-by-1
     because one statement returns NULL.

  2. The NOT-expected: parsing NULL outputs from the cli is
     fundamentally ambiguous. The cli prints back-to-back
     `sqlite>` prompts in several distinct cases:
       - Between two statements where the first returned NULL
       - As buffering before reading a multi-line statement
         (block comments cause this)
       - Right after `.load` when the next statement is being
         read in
     I tried a sentinel-based parser ("recover NULLs as
     `<NULL>`") but couldn't distinguish case 1 from case 2/3
     without cli-side cooperation.

**The fix:**

  - Kept the simple line-based parser (NULL rows get dropped).
  - smoke.expected for creditcard updated: the
    `SELECT cc_type('not a card')` statement is now
    `SELECT coalesce(cc_type('not a card'), '<unknown>')`
    so the row is non-NULL and shows up in the parsed output.
    smoke.expected row added accordingly.
  - count_smoke_selects() needed a `.command`-line strip,
    which I'd missed initially  the `.load` line has no `;`,
    so a naive `text.split(";")` glued it onto the first SELECT
    and undercounted by 1. Now drop dot-command lines before
    splitting.

**What surprised me:**
- T-18 + T-7 found a real off-by-one I'd shipped without
  noticing. The PASS at runtime was coincidence (parser dropped
  NULL → row count happened to match expected). With T-18 the
  invisible drift became visible.
- The cli's prompt behavior is harder to parse than I thought.
  A future cli improvement would be a `--smoke-mode` flag that
  emits one result per line, NULL as `\0` or similar  but that's
  a per-cli change, not something the smoke harness can fix.
- The `<unknown>` sentinel in SQL is a cleaner workaround than
  fighting the parser. Documents intent: "this SELECT may return
  NULL; coalesce to a known-good token for smoke."

**Tooling opportunity:**
- (T-18 closed) Inline.
- (T-19 new) The cli-side `--smoke-mode` flag (one result per
  line, sentinel for NULL, no prompts) would let the smoke
  harness be precise instead of heuristic. Cheap on the cli
  side (~10 LOC). Worth doing if the parser approach trips up
  another extension.

---

### 2026-06-17  T-12 investigation (snippet drift checker)

**What I built:** `tooling/check-snippets.py` + `make
ext-check-snippets` target. Walks every
`extensions/*/src/lib.rs`, finds blocks delimited by

    // --- snippet: <path> (<fn_name>) ---
    ...
    // --- end snippet ---

extracts the named fn from the source file, normalizes (strip
doc comments + attributes + per-line leading whitespace), and
compares against the inlined copy. Reports drift with side-by-
side line-level diff.

**What worked:**
- Convention from snippets/README.md is clear enough for
`re.search` to find blocks reliably.
- Normalization rules ("strip ///, //!, #[...], per-line
leading whitespace") match what every consumer naturally
does to a pasted-in snippet.
- Tested BOTH drift directions:
  - Tamper source (`sum % 10 == 0`  `sum %% 10 == 999`)
   ean drift caught with line + expected/actual values.
  - Tamper inlined (`Some(sum % 10 == 0)`  `Some(false)`)
   same drift report.
- Currently 1 inlined-snippet consumer (ean.weighted_mod10);
the tool reports `OK ean  1 snippet(s) match source` on a
clean run.

**What surprised me:**
- Brace-counting to find the matching `}` for a fn body is
slightly fragile  comments containing `{` or `}` would
throw it off. Not a problem TODAY (Rust comments aren't
nested in our inlined snippets), but worth noting.
- The whole-snippet mode (no fn name in parens) is
implementable and left in the code, but I haven't used it
yet. Defer fixing edge cases until a real consumer hits
them.

**Tooling opportunity:**
- (T-12 closed) Inline.
- (T-20 new) The brace-counting parser would benefit from
ignoring `{` / `}` inside `"..."` strings + `//` comments
+ `/* ... */` blocks. ~10 LOC of state-machine code if
ever needed. Defer until a real snippet trips it.

---

### 2026-06-17  T-13 investigation (classifier pattern)

**What I built:** Two artifacts.

1. tooling/snippets/README.md grew a "Design patterns (not
snippets)" section. Documents the ordered-classifier shape
(used in postcode, creditcard, phone-prefix) and the
validator+extractor pair (used in every check-digit
identifier). Explicitly says why we DON'T extract these as
code  capturing closures don't compose, the boilerplate is
5-7 LOC, the explicit form reads like a spec.

2. New `phone-prefix` extension. E.164 prefix  ISO country +
region. ~150 prefixes hand-curated. Uses the classifier
pattern, and the smoke test proves it works correctly: `+1242`
(Bahamas) resolves to BS, not US, because the 4-digit prefix
is listed before the 1-digit `+1` US entry.

**What worked:**
- The documented design tip is more durable than an extracted
helper would have been. Future-me will read the section before
reaching for a worse classifier impl. Snippets/README.md is
already the "where do I look first?" file.
- The phone-prefix smoke catches a real classification edge
case (NANP prefix shadowing) that proves the design tip's
"order matters" advice is load-bearing.
- The classifier pattern now has 3 documented consumers
(postcode, creditcard, phone-prefix). Per the original T-13
note, that's the threshold for revisiting code extraction.

**What surprised me:**
- I tried sketching a `classify<K, F>(input, table) -> Option<K>`
generic helper and a `first_match!` macro before committing to
the doc-only approach. Both compile but read worse than the
loop they replace. The macro hides an early `return` which
is a footgun.
- Documenting WHY-NOT-extract is more valuable than
documenting WHEN-TO-extract. Anyone tempted to write a
"smarter" classifier will see the reasoning first.

**Tooling opportunity:**
- (T-13 closed) Documented as design tip; extraction deferred
until predicates with the same shape ALSO share signatures
(unlikely without crate-level coordination).

---

### 2026-06-17  T-19 investigation (NULL handling)

**What I built:** Two-line change to tooling/smoke.py. The
harness now prepends `.nullvalue <NULL>` to every smoke.sql
before piping it to the cli, so NULL results render as a
literal `<NULL>` line instead of a blank that parse_results
drops. Test files just write `<NULL>` in smoke.expected.

**What worked:**
- The cli already exposes `.nullvalue <s>`  no Rust changes
needed. I'd spent 20 minutes earlier this week tip-toeing
around the missing NULL row by `coalesce()`-ing in SQL.
That's exactly the kind of workaround that signals there's
a real tooling gap underneath.
- Audited every smoke.expected for `?`-wildcard NULL
placeholders before changing the parser; found zero (the
wildcard was documented but never used  callers ALSO worked
around the gap by avoiding NULLs entirely). So the change has
no back-compat risk.
- phone-prefix smoke.sql goes from
  `SELECT coalesce(f('+999'), '<unknown>');`
to plain
  `SELECT f('+999');`
with `<NULL>` in smoke.expected. Reads like a spec again.

**What surprised me:**
- The 20-line comment in parse_results from a prior attempt
("A more elaborate parser tried to recover NULLs as a `<NULL>`
sentinel by splitting on `sqlite>`...") was right about the
problem but wrong about the solution. The fix wasn't a
smarter parser  it was an existing cli feature I hadn't
noticed.
- This is the second time `.nullvalue` came up. First was a
half-remembered dot-command grep weeks ago. Worth a habit:
when a tooling workaround feels gross, grep cli/src/dot.rs
for the noun first.

**Tooling opportunity:**
- (T-21 new) The cli has ~50 dot-commands (.headers, .mode,
.nullvalue, .timer, ...). I keep half-remembering which ones
exist. A tooling/cli-cheatsheet.md listing each + one-line
purpose + when it's useful in smoke tests would prevent
future workaround-then-discover episodes. Cheap to write,
hugely valuable as a habit anchor.
- (T-19 closed)

---

### 2026-06-17  color extension

**What I built:** SQLite extension for color parsing + WCAG
accessibility math.

Parsers (all on a single `parse() -> Option<Rgb>` entry point):
  - `#rgb` and `#rrggbb` hex (with or without leading `#`)
  - `rgb(r, g, b)` and `rgba(r, g, b, a)` (alpha ignored)
  - CSS basic-16 named colors (`red`, `aqua`, `silver`, ...)

Scalars:
  - `color_to_hex(s)`  canonical `#rrggbb` lowercase
  - `color_to_rgb(s)`  `rgb(r, g, b)` string
  - `color_red/green/blue(s)`  channel as INTEGER 0-255
  - `color_luminance(s)`  WCAG relative luminance 0..1
  - `color_contrast_ratio(a, b)`  WCAG ratio 1..21

Unknown input  NULL (T-19 sentinel renders cleanly).

**What worked:**
- WCAG luminance is the kind of formula where copy-pasting from
the spec at the URL embedded in a `///` doccomment makes the
code self-document. No external dep.
- Mixed-input parse via union-of-strippers (`#rgb` OR
`rgb(...)` OR named) is a different shape from the recent
classifier extensions. The `parse() -> Option<Rgb>` entry
point keeps it composable.
- T-19 paid off immediately: the unknown-color row reads as
`<NULL>` in smoke.expected with no SQL workaround. Felt
like the right move 30 minutes ago, feels even more right
in actual use.

**What surprised me:**
- smoke.expected `#`-line-as-comment collided with hex output
on row 1. Fixed in tooling/smoke.py: only `#` followed by
whitespace counts as a comment. Bare `#abc...` is data.
This is the kind of harness bug that doesn't surface until
an extension's output format includes the comment marker.
- WCAG contrast(#000, #fff) rounds to 21 not 21.00 in the cli
output  sqlite drops trailing zeros on `round()` results that
land on integers. Mention so future-me doesn't pad
smoke.expected with phantom decimal precision.

**Tooling opportunity:**
- (T-22 new) Smoke output format quirks  the "integer-valued
real shows as `21` not `21.00`" surprise above. A 10-line note
in tooling/smoke.py or in extensions/README.md listing
"things the cli does that may surprise smoke authors" would
prevent the same correction twice.

---

### 2026-06-17  T-21 investigation (cli cheatsheet)

**What I built:** tooling/cli-cheatsheet.md  one-row-per-
dot-command reference with a "smoke?" column. Lists all 34
dispatch entries from cli/src/dot.rs in a single table.
Includes a "smoke-test idioms" section (when `.print` helps,
which commands DON'T work on stdin) and a "things that
surprise smoke authors" section (folds in the T-22 note
about `round()` precision and the T-9 dash-comment quirk).

**What worked:**
- Inventoried via `grep '^\s*"\.' cli/src/dot.rs` against the
dispatch match. Compact enough to fit on one screen.
- Found two bugs while writing  my draft confidently said
"prepend `.bail on` for first-failure-wins" and "use `.echo on`
to debug." Both wrong! Reading cli/src/lib.rs:678 showed
echo + bail are ONLY honored inside `.read FILE`. Caught and
corrected before committing. Writing-as-debugging works.

**What surprised me:**
- I'd been carrying around a vague memory that `.bail` worked
on stdin too. Standard sqlite3 CLI honors it more broadly,
but ours does not  it's a `.read` flag. Future me would
absolutely have wasted 15 minutes on this.
- Folding the T-22 note ("integer-valued real prints unpadded")
into the same doc rather than spinning a separate file is the
right call. The cheatsheet becomes the "things that bite you
in smoke" centerpiece, not three half-pages.

**Tooling opportunity:**
- (T-22 closed in same doc.)
- (T-21 closed.)
- The cheatsheet's "Authoritative source: cli/src/dot.rs
dispatch (~line 41). If this drifts, re-run T-21's audit."
note is a verbal staleness check  if a future T-* automates
that (grep dispatch arms vs. cheatsheet rows + warn on diff),
note it then. Premature today.

---

### 2026-06-17  unitconv extension

**What I built:** SQLite extension for unit conversion across
five categories. Five 3-arg scalars sharing one shape:

  conv_length(value, from, to)   m, km, cm, mm, in, ft, yd, mi, nmi, ...
  conv_mass(value, from, to)     g, kg, mg, t, oz, lb, st, ...
  conv_time(value, from, to)     s, ms, min, h, d, wk, yr, ...
  conv_data(value, from, to)     B, bit, KB/MB/GB (1000), KiB/MiB/GiB (1024)
  conv_temperature(value, from, to)  C, F, K, R (affine path)

Length/mass/time/data share one helper: `convert(v, from, to,
TABLE) = v * factor(from) / factor(to)`. Tables are sorted-by-
category alias lists. Temperature has its own path because
C/F/K are affine (offset + scale), not pure scaling.

**What worked:**
- The "TABLE-of-aliases  scaling factor" pattern is the
opposite of the classifier pattern  here the input ORDER
doesn't matter because we look up by exact (case-insensitive)
match, not prefix. Different shape, intentional.
- Smoke uses round() to dodge FP-precision sensitivity. The
affine fixed-point check (-40C = -40F) is a smoke I always
include for temperature conversions  catches the off-by-one
where the offset slips into the scale step.
- T-19 sentinel ('<NULL>' on unknown 'parsec') showcased again.
The smoke reads as a spec from top to bottom.

**What surprised me:**
- `1024.0_f64.powi(2)` is NOT const-callable. Build error
"cannot call non-const method `std::f64::<impl f64>::powi`
in constants." Replaced with literal `1048576.0`. Save 30
seconds in future extensions by reaching for the literal
directly when defining static lookup tables. Note for the
const-tables idiom: stick to additive/multiplicative literals.

**Tooling opportunity:**
- (T-23 new) The const-fn limitation surprise is the kind of
"thing the language does that bites you when building static
tables" that belongs in tooling/cli-cheatsheet.md alongside
the smoke quirks  but it's a Rust-language quirk not a
cli quirk, so a different doc. tooling/extension-recipes.md
could collect "patterns that compile cleanly in scaffold
extensions" (const-table syntax, alias-list shape, etc.).
Defer until a 3rd such surprise.

---

### 2026-06-17  T-11 investigation (all-NULL warn)

**What I built:** 4-line addition to smoke_one() in
tooling/smoke.py. When no smoke.expected exists yet AND the
parsed output has 5+ rows AND every row is `<NULL>`, the
harness emits a one-line WARN ("is your scalar wired up?").
Suppresses automatically once smoke.expected gets seeded
(the real diff catches concrete mismatches).

**What worked:**
- T-19 made this possible. Before the `<NULL>` sentinel,
"all rows empty" was indistinguishable from "no rows
returned" or "buffering artifact." Now an all-NULL run
is a clean signature with no false positives.
- Verified threshold logic via inline python -c: 5 NULL
rows  WARN; 3 mixed  silent; 4 NULL  silent (below
threshold; intentional to keep cost of false positive low
for tiny smokes).
- Catches the "I wired up FID dispatch to the wrong table"
typo class without needing smoke.expected.

**What surprised me:**
- Originally I thought T-11 would be subsumed by T-7
(smoke.expected assertions). It mostly is  but during
scaffold (before seeding) it's still a real signal. The
4-line cost is fine for the asymmetric value (catches a
loud fail-class at the start of every plugin ship).
- Composes nicely with T-18 (smoke.expected staleness check).
T-11 fires when there's no smoke.expected, T-18 fires when
there is one but counts disagree. Different lifecycle
stages, no overlap.

**Tooling opportunity:**
- (T-11 closed)
- The trinity of smoke.py warnings (staleness, all-NULL,
output diff) now covers most fat-typo classes between
scaffold and asserted-stable. Nothing immediate to add.

---

### 2026-06-17  currency extension

**What I built:** ISO 4217 reference-lookup extension. Four
scalars over a single 70-entry table:

  currency_name(code)      "USD"  "United States dollar"
  currency_symbol(code)    "USD"  "$"
  currency_decimals(code)  "USD"  2 (JPY=0, KWD=3, ...)
  currency_numeric(code)   "USD"  840

Table columns: (alpha-3, ISO-numeric, decimals, symbol, name).
Case-insensitive input; non-3-letter input rejected before
lookup. Unknown code  NULL.

**What worked:**
- Exact-key-lookup shape  3rd distinct retrieval pattern this
session: (1) classifier (longest-prefix-wins, phone-prefix),
(2) alias-table (unitconv: one canonical, many names), (3)
exact-key (here: code IS canonical). Each table shape is its
own idiom; trying to extract a generic "lookup" helper would
flatten useful distinctions.
- Pre-filter (len==3 && all alphabetic) lets the table loop
exit fast on garbage input. Same hygiene as phone-prefix's
trim_start_matches('+').
- The decimal-places-per-currency field is the
not-obvious-from-the-code value here. JPY having 0
decimals is the kind of fact a future query needs to format
amounts correctly. Documenting in the table beats
documenting in code comments.

**What surprised me:**
- The full ISO 4217 catalog is ~180 currencies. Shipped ~70
(top-30 by GDP + regional reserve currencies + the unusual
decimal-counts as illustrative examples). The header note
says "curated ~70 most-used; full list is 180+" so future-me
knows it's intentional. Adding the rest is a 1:1 table
append if a real consumer needs them.
- Symbols are a multi-byte Unicode minefield: euro (), Indian
rupee (), Turkish lira () etc. The table holds them as
&str literals; the cli prints them verbatim through wasi
stdout, which is what smoke.expected compares against. No
encoding fiddling needed  worth noting for future symbol-
heavy tables (Norse runes? IPA?).

**Tooling opportunity:**
- (none new) Currency shipped without any tooling surprises.
The T-11 + T-19 pair worked end-to-end: the smoke I drafted
had unknown codes that NULL-render cleanly, and the 4-line
WARN didn't fire because the smoke had non-NULL rows. Both
features earning their keep.

---

### 2026-06-17  T-17 investigation (parallel smoke)

**What I built:** `-j N` / `--jobs N` flag on tooling/smoke.py.
`-j 0` = cpu_count workers (default for `make ext-smoke-all`).
Fans out via concurrent.futures.ThreadPoolExecutor; subprocess
calls are I/O-bound so the GIL release during `subprocess.run`
is enough  no ProcessPool overhead needed.

Measured: 25 asserted smokes go 69s  12.7s (~5.4 speedup
on an 8-core mac). Mostly amortizes wasi runtime startup
across cores; per-extension wall time is unchanged.

**What worked:**
- Threads vs processes: threads won here because each smoke
spawns a subprocess. The Python thread waits on subprocess
I/O the entire time  GIL is held only during the tiny
parse_results + compare. ProcessPool would pay ~50ms fork
cost per worker for no gain.
- Output ordering preserved in serial mode (default for
single-extension invocations) so logs stay readable.
Parallel mode uses as_completed  prints arrive interleaved
but with the extension name prefix, so it's still grep-able.
- Make integration: `ext-smoke-all` defaults to `-j 0`.
Single-extension `make ext NAME=...` still hits the
serial path because it only smokes one.

**What surprised me:**
- I instinctively reached for ProcessPoolExecutor. Stopped to
think: what's actually slow? subprocess + wasi cold-start +
component instantiation, all OUTSIDE the Python process.
Python thread is parked on a syscall. Threads = correct
abstraction. ~30 sec saved by NOT writing the wrong code.
- The 5.4 speedup on an 8-core box leaves headroom. Disk +
wasi runtime overhead floors per-smoke at ~2s. Adding more
workers past 8 brings diminishing returns; -j 0 is the
right default.

**Tooling opportunity:**
- (T-17 closed)
- (T-10 silently closed) Verified: smoke.py's panic_markers
no longer includes "out of memory"  the T-9 fix removed it
when shipping (commit 2a84ec0). Lessons-learned was the only
artifact still listing it as open.

---

### 2026-06-17  humansize extension

**What I built:** Bidirectional humanizer for bytes and
durations. Five scalars across two parser+formatter pairs:

  humansize_bytes(n)            1500       "1.5 KB"   (decimal)
  humansize_ibytes(n)           1536       "1.5 KiB"  (binary)
  humansize_parse_bytes(s)      "1.5 KB"  1500
  humansize_duration(secs)      3700       "1h 1m"
  humansize_parse_duration(s)   "1h 30m"  5400

Formatter picks the largest unit where value 1.0. Duration
caps at 2 most-significant units ("1d 5h" not "1d 5h 23m 7s").
Parser is case-insensitive and tolerates whitespace.

**What worked:**
- Formatter+parser pair is a new shape this session (color was
parse-only, currency was lookup-only). The "parse handles the
formatter's own output" round-trip property is a useful
informal invariant; verified in the smoke (1500 "1.5 KB"
1500). Future me knows the pair is self-consistent.
- The "trim trailing .0" trick gives "1 KiB" instead of
"1.0 KiB" for round numbers. Small but matters when the
output goes into a UI.
- Duration parser handles mixed units in any order ("1h 30m"
or "30m 1h" both work). Total-summing is order-independent
by design.

**What surprised me:**
- The "2-units cap" decision for `format_duration`. I started
with "all non-zero units" ("1d 5h 23m 7s"), which is precise
but reads as cluttered. Capping at 2 produces "1d 5h" which
matches how humans actually talk about durations. The lost
precision (~hours of slack) is OK for the humanize use case;
if a caller needs exact, they pass through humansize_duration
+ humansize_parse_duration round-trip and get back to seconds.
- Stopped before writing a "fuzz-test the round-trip" smoke.
That's overkill for ~50 line code with no hot path. The
6-row round-trip in smoke.sql is enough demonstration.

**Tooling opportunity:**
- (none new) The harness handled this smoothly. T-17 parallel
runner made the "make ext + smoke + smoke --all" loop fast
enough that I felt no friction iterating.

---

### 2026-06-17  T-24 investigation (seed-expected automation)

**What I built:** `tooling/smoke.py --seed-expected NAME` flag.
Writes smoke.expected from the current parsed output, with a
4-line review-and-trim banner pre-pended so it can't ship
accidentally without human review. Refuses to overwrite an
existing file (delete-first is explicit).

**What worked:**
- Pure removal of friction I'd done 5+ times this session:
`python3 tooling/smoke.py --show-parsed X | tee
extensions/X/smoke.expected` followed by manual `# header`
edit. Now one command writes a properly-banner'd file.
- The "delete-first to reseed" gate is the right default 
existing smoke.expected files carry hand-written comments
and intentional `~~` / `?` wildcards that auto-seed would
clobber. Make it explicit.
- Verified: seed produces byte-identical output (minus the
banner) to a hand-written file from the same run.

**What surprised me:**
- Almost shipped it without the banner. The whole value of
smoke.expected is "human reviewed this once and asserted it."
An auto-seeded file that LOOKS reviewed is a regression
trap. The banner is the marker that says "review me before
this is your assertion."
- Resisted the urge to add `--no-banner` for "I really mean
it." That switch IS the regression trap. If a future me
wants no banner, they can manually edit  the 4 lines are
4 keystrokes to remove. Don't paper over the safety with
config.

**Tooling opportunity:**
- (T-24 closed)
- The trinity of smoke.py warnings + the seed workflow now
covers: scaffold (T-11 all-NULL warn)  seed
(T-24 --seed-expected)  iterate (T-7 diffs)  stale-detect
(T-18 count mismatch)  fast-CI (T-17 parallel). Full
lifecycle covered. Nothing immediate to add.

---

### 2026-06-17  T-17 follow-up (cache contention)

**What I found:** The T-17 parallel runner from commit f43134d
was flaky  reproducible "actual=0 rows" in random subsets
per run, often 2-9 of the 27 smokes. Root cause: the host's
cas.sqlite component cache file is opened unconditionally,
even with --no-component-cache. The flag only skips USING
the cache; the SQLite file is still opened, and 8+ concurrent
opens of the same DB race during initialization.

**What I fixed:** Per-worker `--cache-dir` via
`tempfile.mkdtemp()`. Each parallel subprocess gets its own
cas.sqlite. Cleaned up in finally{} so timeouts/exceptions
don't leak. ~1 MB tempdir per worker, short-lived.

Verified: 5 consecutive `-j 0` runs, 27 smokes each, zero
failures. The flake repro'd in 2-3 of every 5 runs before.

**What I learned:**
- "--no-component-cache" sounds like "don't touch the cache,"
but it means "don't use the cache for caching." The file
machinery still runs. Two-flag idioms are misleading on
their own  the host should probably honor a "skip cache
entirely" mode if this comes up again.
- Diagnosed via repeated runs ("3 in a row, see if it flakes").
Cheaper than reading code first. If a build looks flaky,
believe the flake before debugging the test.
- Almost shipped the wrong fix (--no-component-cache alone)
because the FIRST run with that change still failed and I
attributed it to leftover state. Three more runs proved the
flake was still present  per-worker dirs are the real fix.

**Tooling opportunity:**
- (none new) The diagnostic loop (3-5x repeated `--all`)
worked. Could be a `--repeat N` smoke flag for future
flake-hunting, but that's over-tooling for a problem this
size. Defer until a 2nd flake hunt.

---

### 2026-06-17  latlon extension

**What I built:** Coordinate format conversion extension. Five
scalars on a numeric/string transform shape (different from
all prior shapes this session  not a lookup, not a parser-
union, not a formatter-only):

  latlon_to_dms(decimal, axis)      40.7128, 'lat'  "40° 42' 46.08\" N"
  latlon_to_ddm(decimal, axis)      40.7128, 'lat'  "40° 42.768' N"
  latlon_from_dms(text)             "40° 42' 46\" N"  40.7128
  latlon_normalize_lon(x)           wrap to [-180, 180)
  latlon_normalize_lat(x)           clamp to [-90, 90]

`axis` arg picks the hemispheric letter (N/S vs E/W). Parser
is permissive: handles "40° 42' 46\" N", "40 42 46 N", or
raw signed "-40.7128". Bad axis  NULL.

**What worked:**
- Dogfooded T-24 (`--seed-expected`) on this ship. Wrote
smoke.sql, made the extension, ran --seed-expected, swapped
the banner for a real description, committed. Saved at
least 2-3 minutes of manual harvest+paste.
- Longitude wrap (Euclidean remainder) catches the boundary
case `180.0  -180.0` so smoke can pin the open-half-
interval convention.
- The smoke caught a real bug during development: I'd written
`'' ` (two apostrophes) in the format string by mistake.
The DMS output read "40° 42'' 46" instead of "40° 42' 46".
Fixed before commit. Smoke = spec.

**What surprised me:**
- T-17 parallel runner regression caught immediately by the
end-of-ship `--all -j 0` check. Diagnosed and fixed in 15
minutes. If --all had been silent ("all 27 passed") and I'd
moved on, the flake would have hit a future committer 
much harder to debug cold than fresh. The discipline of
running --all at the end of every ship paid off here.
- Latitudes clamp (no wrap) but longitudes wrap (no clamp).
That asymmetry confuses people who've never thought about
coordinate systems. Documented in code; might be the kind
of thing worth a one-line note in a "geo extensions"
overview doc if more land.

**Tooling opportunity:**
- (none new) Workflow felt smooth. Plugin count 97  98.

---

### 2026-06-17  T-25 investigation (T-* status reader)

**What I built:** `tooling/t-status.py`  scans this file for
`(T-N new)` / `(T-N closed)` markers and prints the open
vs closed lists. Usage:

  python3 tooling/t-status.py          all
  python3 tooling/t-status.py open     just open
  python3 tooling/t-status.py closed   just closed

The section title each marker sits in becomes the displayed
label. Anything with a `new` marker but no `closed` is open.

**What worked:**
- The 18 T-numbered items so far (most over the last 24 hours)
have been getting hard to keep mental track of. Each ship I
manually grepped lessons-learned for "what's still open" 
~30 seconds per check, multiplied by 3-4 checks per ship.
- Tested live: produced exactly 3 open (T-14, T-20, T-23 
all explicitly deferred) and 15 closed, matching my mental
model. No surprises in the categorization.

**What surprised me:**
- The regex needed to be lenient about "closed" sub-clauses:
"(T-13 closed)", "(T-10 silently closed)", "(T-15 closed
inline)", "(T-22 closed in same doc.)" all appear in the
real corpus. A strict `(T-N closed)` regex would miss those.
The pattern `(T-N[^)]*closed[^)]*)` covers all observed
variants without overmatching.
- The display picks the markdown `###` section title above
each marker. Made the output more useful  it shows the
CONTEXT each T-* lived in, not just the bare number. Same
T-* in section "container" vs "T-21 investigation" reads
very differently.

**Tooling opportunity:**
- (T-25 closed) The 30-line script is the right amount of
ceremony for this. Anything more ambitious (interactive
filters, age-of-open, frequency-of-mention) would be
over-tooling for ~20 T-* items.

---

### 2026-06-17  numfmt extension

**What I built:** Pure-formatter extension. Seven scalars on
the "in: number  out: string" shape (no parser-back, no
lookup):

  numfmt_commas(n, places)       1234567.89, 2  "1,234,567.89"
  numfmt_fixed(n, places)        3.14159, 2     "3.14"
  numfmt_ordinal(n)              21              "21st"  (11th!)
  numfmt_scientific(n, sig)      1234.5, 3      "1.23e3"
  numfmt_percent(n, places)      0.135, 1       "13.5%"
  numfmt_pad_left(s, w, fill)    "42", 5, "0"   "00042"
  numfmt_group(n, sep)           1234567.89, '.' "1.234.567.89"

The ordinal-suffix rule (11/12/13 override last-digit) is
the kind of thing the smoke catches by enumeration: 1st, 2nd,
3rd, 11th (NOT 11st), 21st, 112th.

**What worked:**
- T-24 seed-expected paid off again: 22 rows seeded in one
command, just swapped the banner for a description.
- Different shape from everything prior this session:
formatter-only (no parser back, no lookup, no conversion).
The closest peer is humansize but that has a parser too.
This is the "pure output transform" shape.

**What surprised me:**
- pad_left('hi', 4, ' ') produced "  hi" but the smoke
harness's prompt-regex (`^(sqlite>\s*|\s*\.\.\.>\s*)+`) eats
leading whitespace as part of prompt stripping. The padded
output was indistinguishable from non-padded in the parsed
diff. Worked around by using '.' fill char in the smoke
so the padding is visible.
- This is the second harness quirk (after T-22's "integer-
valued real prints as 21 not 21.00") that affected expected-
file authoring. Worth a one-line note in
tooling/cli-cheatsheet.md.

**Tooling opportunity:**
- (T-26 new) Add a "harness output limitations" subsection
to tooling/cli-cheatsheet.md listing:
  - integer-valued real loses ".00" trailing zeros
  - leading whitespace is stripped by prompt regex
  - block comments produce extra `sqlite>` prompts in stdout
Cheap update; defer the file edit to the next batch.
Plugin count 98  99.






















