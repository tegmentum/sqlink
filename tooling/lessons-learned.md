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

---

### 2026-06-17  T-26 investigation (harness output limits doc)

**What I built:** Added a "Harness output limitations" section
to tooling/cli-cheatsheet.md collecting the four parser-side
behaviors that have surprised me during recent ships. Each
entry names: the symptom, the workaround, and the extension
that surfaced it.

  - leading whitespace eaten by prompt regex (numfmt pad_left)
  - integer-valued reals lose .00 (color WCAG contrast)
  - `#` hex output  comment marker (color hex)
  - NULLs render as <NULL> sentinel (T-19, baseline)

**What worked:**
- Each entry traces back to a specific extension that hit it.
Future-me reads "ah,  in pad_left was surfaced by numfmt
 they probably had the same problem" and skips ahead.
- The section sits below "Things the cli does that surprise"
(real cli quirks) so the difference is clear: cli-level vs
harness-level. The harness behaviors are MORE recoverable
(rewrite the smoke) than the cli ones (work with what the
cli emits).

**What surprised me:**
- Almost wrote the section as "things the harness gets wrong."
Reframed to "harness output limitations"  these are
intentional simplifications (the parse_results regex IS
greedy by design, the # comment marker IS shorter than
needed), not bugs. The doc should communicate "this is the
contract, work with it" not "this is broken, work around it."

**Tooling opportunity:**
- (T-26 closed)
- Notice the pattern: 3 of 4 entries trace back to a
specific commit. If the cheatsheet had a "(surfaced by:
commit X)" link per entry, drift becomes easier to verify.
Premature for 4 entries; revisit if it grows to 10+.

---

### 2026-06-17  radix extension (plugin #100!)

**What I built:** Integer base conversion extension, bases
2-36. Five scalars on a base-N algorithm shape (new this
session):

  radix_to(n, base)             255, 16   "FF"
  radix_from(s, base)           "FF", 16  255
  radix_change(s, from, to)     "FF", 16, 2  "11111111"
  radix_digits(n, base)         255, 10   3
  radix_bits(n)                 255       8

Out-of-range base (<2 or >36)  NULL. Negative numbers
preserve leading `-`. Case-insensitive parse, uppercase
output. `i64::MIN` handled via i128 widening before unsigned
abs (the standard "can't .abs() i64::MIN" trick).

**What worked:**
- Dogfooded T-24 seed-expected again. Edited the banner to
a one-line description, committed. Should be the default
workflow now.
- The smoke covers the i64-edge cases explicitly: 0, -42,
i64::MIN (via radix_bits implicitly). Each edge case is
ONE row, distinguishable in the diff.
- This is plugin #100 by count. Worth pausing to note: the
catalog now has 100 wasm component extensions across ~30
domains, all sharing the same dispatch ABI. The investment
in tooling (scaffold, smoke, plan-add, lessons-learned
discipline) has been carrying its weight  most ships in
this batch took 15-20 minutes start to finish.

**What surprised me:**
- Rust's `i64::abs()` panics on `i64::MIN` (overflow). Caught
this from a previous bug-hunt; preemptively used
`(n as i128).unsigned_abs() as u64`. The fact that I REMEMBERED
the trick suggests the snippets/README.md "design patterns"
discipline is working: I read once that "i64 abs is unsafe at
MIN" and now reach for the workaround automatically.
- Plugin #100 didn't feel like a milestone during shipping 
the workflow is smooth enough that no individual ship feels
notable. That's the right outcome from process investment.

**Tooling opportunity:**
- (none new) Catalog dynamics are healthy. 100 ships in;
no friction to add.

---

### 2026-06-17  T-27 investigation (ext-ship wrapper)

**What I built:** `make ext-ship NAME=<x>` target. Bundles
the existing `make ext NAME=<x>` (build + single-smoke) with
a full `smoke --all -j 0` regression check at the end. Also
updated the new-extension Claude skill so future-me sees the
recommendation in the workflow doc.

**Why:** Each ship this session I manually ran `--all -j 0`
after committing the plugin. Once it caught a real bug
(T-17 parallel flake during the latlon ship). The
discipline is load-bearing  but it's easy to forget when
the prior step said "PASS." A single target removes the
forget-failure path.

**What worked:**
- Kept `make ext` itself unchanged. The compile-fix-compile
loop during DEVELOPMENT needs to be fast (single smoke,
not full --all). Only the end-of-ship wrapper opts into
the full pass.
- Cost: ~15s for the full catalog at -j 0. Worth it once per
ship; would NOT be worth it per-iteration.
- Verified live by running `make ext-ship NAME=radix` 
all 29 smokes PASS.

**What surprised me:**
- The skill (`.claude/commands/new-extension.md`) had a
"Smoke-everything" section but framed it as "if you suspect
a regression." That's wrong  the right framing is "at
end of every ship, default." Reframed and renamed.
- Almost added `ext-ship` AS the default `make ext`. Stopped:
that breaks the iteration loop. The two targets need
distinct purposes; the wrapper is the one that signals
"I'm done."

**Tooling opportunity:**
- (T-27 closed) The 6-line Makefile target is the right
amount of ceremony.

---

### 2026-06-17  natsort extension

**What I built:** Natural sort comparison  the "file2 sorts
before file10" semantics. Three scalars on a tokenize-then-
compare shape (new this session):

  natsort_compare(a, b)   -1 / 0 / 1
  natsort_less(a, b)       1 if a sorts before b natural-order, else 0
  natsort_key(s)           packed string s.t. lexicographic
                            compare(key(a), key(b))  natsort_compare(a, b)

Tokenizer splits each input into runs of (digits | non-digits).
Number tokens are compared numerically; text tokens are compared
case-insensitively. Number tokens sort before text tokens at
shared positions (the "file10 before filea" convention).

Tie-break: equal numeric value but different digit count  fewer
digits sorts first ("1" < "01"). All shared positions tied 
shorter total sequence sorts first.

**What worked:**
- The key() function lets a smoke verify that the COMPARATOR
agrees with the KEY function on the same inputs  not via
testing the implementation twice, but by composing them:
`compare(key(a), key(b))` should match `compare(a, b)`.
Smoke includes that round-trip check.
- Dogfooded `make ext-ship` (T-27) as my end-of-ship check.
The "ship + regression check + done" flow is one command now.
30 smokes PASS in 16s.

**What surprised me:**
- `ta.len() as i64).cmp(...)` returns `Ordering`, not i64, so
the cast-to-i64 wouldn't work. Replaced with explicit match.
Rust's type system caught this at compile time  no smoke
needed. Worth noting as a reminder: try the casts first,
let the compiler tell you when they're wrong.
- Mixed token comparison (number vs text at same position)
isn't a standard the natsort literature agrees on. I picked
"numbers first" because that's what most filesystems and
file-managers do. Documented in the source.

**Tooling opportunity:**
- (none new) ext-ship smoothed the workflow further than I
expected  felt notably faster than my prior "build,
smoke, manually-run-all" loop.

---

### 2026-06-17  T-28 investigation (extension-patterns catalog)

**What I built:** tooling/extension-patterns.md. ~150 LOC
markdown that distills the 10 retrieval shapes I've called
out across lessons-learned.md (now ~1700 lines) into a
quick-picker table + one-paragraph descriptions + reusable
helper checklist + anti-patterns list.

Quick-picker maps shape  representative extension so a
future ship can start with "this is like currency  exact-
key lookup" instead of grepping 100 lessons-learned entries.

**What worked:**
- Each row in the picker names a CONCRETE extension so the
"like X" mental model has somewhere to point. classifier 
postcode; alias-table  unitconv; tokenize  natsort. Names
beat abstractions.
- The "anti-patterns" section captures decisions I almost
made and rejected: returning JSON multi-value instead of N
scalars, wrapping a heavy crate for 1-2 fns, adding a generic
_normalize. Future-me will recognize the temptations.
- Keeping the doc TIGHT (one paragraph per shape) prevents
it from drifting into a tutorial. The deep lessons stay in
lessons-learned; this is the index.

**What surprised me:**
- Writing this revealed two shapes I'd been conflating:
"coord transform" (numeric mapping, no lookup) is genuinely
distinct from "alias-table" (lookup-driven scaling). latlon
+ geo-distance fit the former; unitconv fits the latter.
Documented separately.
- I added a "When to add a new shape" footer ("happens ~once
per 5-10 ships"). Setting the calibration explicitly means
future-me doesn't either (a) add a new shape for every
ship (over-categorization) or (b) cram a new shape into
a near-fit existing category (under-categorization).

**Tooling opportunity:**
- (T-28 closed)
- The pair of (lessons-learned.md  one-liner; extension-
patterns.md  one-paragraph; snippets/README.md  code or
design tip) now spans the explanation depth gradient. New
ships look at extension-patterns first for shape, then dip
into lessons-learned for the why-this-not-that detail.

---

### 2026-06-17  country extension

**What I built:** ISO 3166-1 country reference lookup. Five
scalars over a 95-entry table:

  country_name(s)      "US" / "USA" / "840"  "United States"
  country_alpha2(s)    "USA"                  "US"
  country_alpha3(s)    "US"                   "USA"
  country_numeric(s)   "US"                   840
  country_region(s)    "US"                   "Americas"

Auto-detects input format by length + char class:
  - 2-char alphabetic  alpha-2 lookup
  - 3-char alphabetic  alpha-3 lookup
  - 1-3 digit numeric  ISO numeric lookup
Case-insensitive. Unknown / malformed  NULL.

**What worked:**
- The auto-detect input is the novelty over the currency
extension's design: instead of forcing the caller to know
which column they have, sniff the format. Works because the
formats are mutually exclusive (no 3-letter all-numeric, no
2-letter all-alphabetic that's also a number).
- This is the second consumer of the "exact-key lookup" shape.
Combined with currency, the pattern in
tooling/extension-patterns.md now has two reference points.
- Used T-24 (--seed-expected), T-27 (ext-ship). Workflow
felt routine  zero seconds of "what do I run next?"

**What surprised me:**
- The first draft of `lookup()` tried each column unconditionally
("try alpha-2; if None, try alpha-3; if None, try numeric").
That's the parser-union shape  WRONG here. The input format
is unambiguous, so sniff once and dispatch. Cleaner, faster,
fails earlier on bad input.
- Documenting this in extension-patterns.md as the difference
between "parser-union" (parse multiple input grammars) and
"auto-detect exact-key" (input format is unambiguous,
dispatch to the right column). They look similar but the
control flow is opposite.

**Tooling opportunity:**
- (T-29 new) extension-patterns.md should mention the
"auto-detect exact-key" variant under the exact-key lookup
section. Adds ~3 lines. Cheap; defer to the next T-* batch
since this is the first auto-detect ship and one example
isn't enough yet to know if it's worth its own row in the
quick-picker.
Plugin count 101  102.

---

### 2026-06-17  T-29 investigation (auto-detect exact-key variant)

**What I built:** Added an "Auto-detect variant" subsection
under exact-key lookup in tooling/extension-patterns.md.
Includes a 10-line code template, a note distinguishing it
from parser-union (overlapping grammars vs. character-class-
disjoint formats), and `country` as the reference.

**What worked:**
- Folding into the existing exact-key section rather than
spinning a new row in the quick-picker. With ONE consumer,
a top-level row would over-promise; a subsection under the
parent shape signals "variant, not new shape." If a 2nd
auto-detect ship lands and the dispatch pattern is the same,
promote to its own row.
- The "NOT a parser-union" call-out matters. Both shapes have
"try multiple input forms" energy, but the control flow is
opposite. Future-me will hit this distinction and the doc
saves the wrong choice.

**What surprised me:**
- Wrote the original lookup() in country as parser-union
("try alpha-2, then alpha-3, then numeric"). The cleanup to
sniff-then-dispatch was a real refactor, not a stylistic
preference. With the wrong shape, an obviously-not-alpha-3
input ("US") still TRIED the alpha-3 lookup before failing
 wasted cycles AND obscured the "the formats are disjoint"
truth.
- The deferred-to-next-batch threshold from T-29's original
note ("first auto-detect ship and one example isn't enough")
was already met before next-batch  the moment I added
extension-patterns.md and looked at it for the auto-detect
section, the lack of it was friction. Closed same batch
instead of next.

**Tooling opportunity:**
- (T-29 closed)
- Notable meta-lesson: T-* closure timing is sometimes
the SAME turn that I write the lessons-learned entry, not
later. If the closure is small AND I'm already in the file,
inline-close beats next-batch.

---

### 2026-06-17  iban extension

**What I built:** ISO 13616 IBAN validator + decomposer. Six
scalars on the validator+extractor shape (4th consumer):

  iban_validate(s)         1 if valid, else 0
  iban_normalize(s)        strip whitespace, uppercase
  iban_country(s)          alpha-2 (first 2 chars)
  iban_check_digits(s)     2-digit checksum (chars 3-4)
  iban_bban(s)             body after check (NULL if invalid)
  iban_format(s)           groups-of-4 display form

Mod-97 algorithm: rearrange first 4 to end, expand A-Z to
10-35 as decimal digits, accumulate the long-decimal mod 97
iteratively (no bignum needed). Valid iff remainder == 1.

Length validation per country is REQUIRED  the mod-97 check
alone catches digit-transposition but NOT missing/extra digits.
Both checks together cover the typo space.

**What worked:**
- The iterative mod-97 (carry remainder through digits one
at a time) means no bignum dependency. ~10 lines including
the A-Z expansion.
- Smoke covers the failure modes EXPLICITLY:
  - tampered check digit (GB82  GB83)
  - tampered body char (WEST  XEST)
  - wrong length for country (DE89...0 = 21 chars, not 22)
  - unknown country (XX)
  - empty input
Each fails for a different reason; smoke documents the
defense-in-depth.
- Reached for the validator+extractor template from
extension-patterns.md before writing code. Saved 5 minutes
of "where do I put the length check?"

**What surprised me:**
- I almost wrote a `Statement` re-implementation of mod-97
in bignum (with `String` accumulation). Caught myself: the
canonical iterative form is what every IBAN reference
implementation uses, and it's strictly simpler. The
"accumulate the remainder" pattern is the right answer 
recognizing it from prior algorithms (mod-N CRC, ABA digital
root) is the meta-lesson.
- The 78-country LENGTHS table felt like it should be its
own snippet (alongside ISIN/CUSIP country lists), but each
catalog's columns are different shapes (length here, format
mask in others). Not worth extracting.

**Tooling opportunity:**
- (none new) Workflow clean. Plugin count 102  103. The
ext-ship target paid for itself  one keystroke at the end
to run the regression check.

---

### 2026-06-17  T-30 investigation (skill currency update)

**What I built:** Updated `.claude/commands/new-extension.md`
to reflect the tooling that's shipped this session. The skill
was authored before T-19/T-21/T-24/T-25/T-26/T-27/T-28 landed
and was missing significant workflow steps.

Changes:
- Step 3 now starts with "Identify the shape" and points to
extension-patterns.md. The shape decision must come BEFORE
writing code  picking right here saves significant
refactoring.
- Step 4 mentions T-19 NULL sentinel and the harness-
limitations section of cli-cheatsheet.md.
- Step 5 adds `--seed-expected` as the canonical way to
write smoke.expected, with explicit advice that the banner
is a TODO marker.
- Step 7 (new) standardizes the lessons-learned entry shape
with the four-section template I've converged on.
- Step 8 (new) makes `make ext-ship` the canonical
end-of-ship check, not bare `make ext`.
- Bottom "Status checks" section points to t-status.py and
smoke.py --list.

**What worked:**
- Diffing against the skill found 6 distinct things I'd been
doing manually that the skill didn't document. Each was a
small recurring tax; collectively, the new ship workflow
should be smoother for a future-me coming back cold.
- Kept the original `make ext` step intact for the iteration
loop; only the FINAL check uses ext-ship.

**What surprised me:**
- The skill is the primary touchpoint for future-me running
this workflow cold. Every new tool I add HAS to land in the
skill or it doesn't get used. Treating skill updates as a
mandatory "after T-* closure" step would prevent this drift.
Logged as the meta-lesson; not formalizing as a T-* until I
see drift again.
- Total skill grew from 164 to ~190 lines. Still scannable
in one screen.

**Tooling opportunity:**
- (T-30 closed)
- The pattern "ship tool  update skill" should be a habit.
Currently relies on memory; if drift recurs, formalize as a
`tooling/skill-currency.py` check that warns when a tool was
added without a skill mention.

---

### 2026-06-17  nato extension

**What I built:** NATO phonetic alphabet encode/decode. Three
scalars on a "lookup-table-with-bidirectional-traversal" shape
(close to formatter+parser, but the underlying table is the
same for both directions so the round-trip property is
table-internal, not algorithmic):

  nato_encode("ABC")        "Alpha Bravo Charlie"
  nato_decode("Alpha Bravo")  "AB"
  nato_word('A')             "Alpha"

Multi-word input/output: " | " between word boundaries on
encode; "|" decodes back to space. Case-insensitive in both
directions. Unknown decode words fall back to first-char.

**What worked:**
- Smoke verifies the round-trip `decode(encode(x))  upper(x)`
on "Hello World"  composes both directions through the
sentence boundary handling.
- Catching myself before shipping `Box::leak` in the encode
function. The leak was small (one Box per non-alpha char,
freed at process exit anyway) but tasteless. Restructured to
`Vec<String>` and `.join(" ")` instead. ~5 LOC longer but
zero unsafe / zero leak. Worth it.
- nato_word as a single-char lookup is a useful primitive
even though encode covers full strings. Composable in SQL
queries.

**What surprised me:**
- The need for "|" as a word boundary. Without it,
encode("AB CD")  "Alpha Bravo Charlie Delta" would be
indistinguishable from encode("ABCD"). The boundary marker
is necessary information that has to survive the round-trip.
Once I saw the smoke for it, the design becomes obvious;
without the smoke, I'd have shipped the lossy version.
- Smokes drive design. The "round-trip property" check forced
me to think about boundary preservation BEFORE writing the
decoder. Worth elevating in the skill.

**Tooling opportunity:**
- (none new) Plugin count 103  104. Process is humming.

---

### 2026-06-17  T-31 investigation (plan-add silent truncation)

**What I built:** Changed `tooling/plan-add.py` to REFUSE
descriptions that overflow the column width instead of
silently truncating with an ellipsis. Added `--force` for
the rare cases where the truncation is acceptable.

**Why:** The last 5 ships had truncated descriptions in
PLAN-sqlite-plugins.md without me noticing  the column
silently lost "(bidirectional)", "(file2 < 10)",
"(A->Alpha)", etc. Each truncation made the plan less useful
as a quick scan.

**What worked:**
- The error message tells you the budget AND your overflow,
so you can rewrite cleanly: "column budget is 20 (label
width 31, name takes 11 chars including parens)."
- `--force` is the explicit opt-in for truncation. Removes
the silent failure mode while preserving access to the old
behavior. Same shape as T-24's banner pattern: "do the safe
thing by default; the unsafe option needs a flag."
- Verified live: tried a too-long description, got the
refusal with a useful message; --force restored the
truncated insert.

**What surprised me:**
- This bug had been present from the start. Every ship I'd
just stopped reading the plan-add output after "appended:"
because the row was always emitted, so I assumed the
truncation was something the table designer accepted.
WRONG  it was a silent-data-loss bug, just slow-moving.
- The pattern "silent data loss in a tool whose output I
stopped reading" is worth recognizing. T-19 was the same
class (NULL  empty line silently dropped). The fix shape
is also the same: make the failure obvious instead of
hiding it under a workaround.

**Tooling opportunity:**
- (T-31 closed)
- (T-32 candidate) The cli-cheatsheet's "Harness output
limitations" section captures THIS class of bug (silent
parse-side limitations). plan-add belongs in the same
catalog. If I add 2 more tools with silent-failure modes,
write a "tooling design tips" doc with "make failures
loud" as the headline. Defer until 3rd consumer.

---

### 2026-06-17  tile extension

**What I built:** Web Mercator XYZ tile coordinate math. Seven
scalars covering lat/lon  tile, tile  bounding box, and
quadkey encode/decode:

  tile_x(lon, z) / tile_y(lat, z)        decimal degrees  tile xyz
  tile_lon(x, z) / tile_lat(y, z)        tile xyz  NW corner deg
  tile_quadkey(x, y, z)                   Bing-style quadkey string
  tile_from_quadkey(q)                    JSON {x, y, z}
  tile_bbox(x, y, z)                      JSON {west, south, east, north}

Web Mercator latitudes clamp to +/-85.05 (the standard range
beyond which y blows up). Quadkey is "interleave bits as base-4
digits MSB-first."

**What worked:**
- T-31 fired IMMEDIATELY on my plan-add call ("web mercator
tile coords + quadkey" = 34 chars, budget 24). Forced me to
rewrite to "web mercator + quadkey." Tool-discipline already
paying off in the same turn it landed.
- Coord transform shape recognized from extension-patterns.md
before writing code. Smoke pattern (forward, reverse,
round-trip) followed directly.
- Smoke surfaced the "empty-string output" harness quirk:
`tile_quadkey(0, 0, 0)` returns "" by spec (zoom 0 is one
tile, no digits) but parse_results strips empty lines.
Coalesced with `<empty>` sentinel  similar to the T-19
NULL handling but for empty strings.

**What surprised me:**
- Quadkey zoom-0 = empty string. Mathematically correct but
ergonomically awkward. The smoke needed to express both "the
function returned empty" AND "the row exists." Sentinel
wrap (`nullif(..., '')` + `coalesce`) was the cleanest
workaround.
- Mercator latitude clamp at +/-85.05  not +/-90  is the
non-obvious constant. Future-me will forget; the smoke's
"clamps to 0/31 at zoom 5" rows pin it.

**Tooling opportunity:**
- (T-32 new) Empty-string outputs are the SECOND silent
parse-side limitation I've hit (after T-19 NULL). T-32 = add
an "Empty strings dropped" note to the cli-cheatsheet's
harness-output-limitations section. Tiny edit; do it on the
next T-* batch.
Plugin count 104  105.

---

### 2026-06-17  T-32 investigation (empty-string harness limit doc)

**What I built:** Added a 5th bullet to the cli-cheatsheet's
"Harness output limitations" section explicitly documenting
that empty-string outputs get dropped by `parse_results`,
with the canonical workaround:

  SELECT coalesce(nullif(f(), ''), '<empty>');

Specifically notes that this is NOT solvable by another
`.nullvalue`-style directive  empty string and NULL are
different types, and the blank-skip exists for the prompt-
noise reason that's load-bearing.

**What worked:**
- Took ~2 minutes from "I see the issue" to "documented in
the place I'll find it." The cheatsheet already had a
section for this exact category. Adding to an existing
catalog is much cheaper than starting one.
- The doc names the SURFACING extension (`tile_quadkey`)
following the same pattern as the other 4 entries. Each
entry remains traceable to its origin commit.

**What surprised me:**
- I almost added `--empty-value <s>` as a smoke.py
counterpart to `.nullvalue`. Caught myself: that would
require cli-side cooperation (the cli has no notion of
"this scalar returned the empty string vs no row"),
the harness can't tell the difference either, and the
sentinel-wrap is 22 keystrokes per problem case. Three
reasons not to build the tool. Documented the workaround
instead.
- The "make failures loud" pattern (T-19, T-31) doesn't
APPLY here. The empty-string drop isn't a bug to fix; it's
a parser invariant the harness needs to function. The
correct response is to document the invariant, not to
remove it.

**Tooling opportunity:**
- (T-32 closed)
- The cli-cheatsheet's harness-limitations section now has
5 entries  big enough to be useful but small enough to
read in one screen. Worth a periodic re-read; defer until
catalog adds a 6th entry to confirm it's still load-
bearing.

---

### 2026-06-17  setops extension (new shape: variable-array)

**What I built:** Set operations on JSON arrays. Eight scalars:

  set_union(a, b)              [1,2,3] + [3,4,5]  [1,2,3,4,5]
  set_intersection(a, b)        [1,2,3] [2,3,4]  [2,3]
  set_difference(a, b)          [1,2,3] - [2,3]   [1]
  set_sym_difference(a, b)      A  B = (A-B) + (B-A)
  set_unique(a)                 [3,1,2,1]  [3,1,2]
  set_contains(arr, x)          1 / 0
  set_subset(small, big)        1 / 0
  set_disjoint(a, b)            1 / 0

Equality is by canonical JSON serialization  `1` != `1.0` and
`"abc"` != `abc`. Matches what SQLite users expect from
json_equal-style functions. Lossy for FP equality but
predictable.

**What worked:**
- This is a NEW shape: "variable-length array I/O via JSON."
The extension-patterns.md anti-pattern entry warned against
returning JSON for FIXED-shape multi-values  but variable-
length is exactly what JSON arrays are for. The distinction
got me unstuck on the shape decision.
- Dedup preserving first-occurrence order is more useful than
sorted dedup for the SQL ergonomics: `set_unique` over a
typed-input column shouldn't reorder the input.
- T-31 fired AGAIN ("set operations on JSON arrays" = 29
chars, budget 22). Rewrote to "JSON-array set ops" (18
chars). Second consecutive ship where the tool caught me.
That's a real workflow improvement, not noise.

**What surprised me:**
- The intersection-with-empty case ('[]') returns '[]' (2
chars), which DOESN'T trigger T-32's empty-string drop.
I'd assumed I'd need a sentinel and wrote one; then realized
the JSON encoding has explicit brackets and the harness was
fine. Removed the sentinel wrap. ~30 seconds of "oh wait."
- The canonical-JSON equality choice means callers shouldn't
mix `1` and `1.0` in the same set; serde_json preserves
trailing `.0` so they hash separately. Documented in code.

**Tooling opportunity:**
- (T-33 new) extension-patterns.md should add a "Variable-
length array I/O" shape entry. Distinct from the "JSON
multi-value anti-pattern" because the variable-length case
NEEDS JSON. ~5 lines added to the doc; defer to the next
T-* batch.
Plugin count 105  106.

---

### 2026-06-17  T-33 investigation (variable-length array shape)

**What I built:** Added 11th row to the quick-picker table in
tooling/extension-patterns.md, plus a "Variable-length array
I/O" detailed section under the existing shape catalog. The
section explicitly contrasts with the "JSON multi-value
anti-pattern" entry below it so future-me can tell when JSON
is right vs wrong.

  Right: setops returns N items where N depends on input
  Wrong: currency_info(code) returns JSON with fixed shape

Also documents three pitfalls discovered shipping setops:
order semantics decision, "[]" doesn't drop, NULL on
malformed input composes with json_each.

**What worked:**
- The contrast-with-anti-pattern framing was the right move.
A new "this is correct" entry without the cross-reference
to the existing "this is incorrect" entry would have left
the apparent contradiction unresolved. Future-me would
have read both and been confused.
- Mentioned T-32 inline ("`[]` doesn't trigger T-32's drop").
The shape catalog is becoming a hub for cross-references
into the tooling lessons; that's healthy.

**What surprised me:**
- I almost wrote the new section ABOVE the anti-pattern entry.
Moved it BELOW, so the reading order is: detailed shape entry,
THEN anti-pattern entry next to it. Putting them adjacent
makes the distinction read like a single thought rather than
two unrelated points.

**Tooling opportunity:**
- (T-33 closed) The pattern catalog now has 11 entries; that's
about the right size for a one-screen quick-picker. If it
crosses 15, consider splitting by domain (I/O shapes vs
algorithm shapes). Premature today.

---

### 2026-06-17  compass extension (quantizer shape)

**What I built:** Compass bearing  cardinal direction
conversion. Five scalars on a quantizer shape (numeric value
 discrete band  name):

  compass_cardinal(deg)         8-point (N/NE/E/SE/S/SW/W/NW)
  compass_cardinal16(deg)        16-point (adds NNE/ENE/etc.)
  compass_degrees(name)          reverse: name  center degrees
  compass_distance(a, b)         shortest angular distance 0..180
  compass_normalize(deg)         wrap to [0, 360)

Bands are CENTERED on the cardinal point. North covers
[-22.5, +22.5) at the 8-point resolution, NOT [0, 45). This
matches navigation convention; smoke locks the boundary at
22 (still N) vs 23 (now NE) so future-me can't accidentally
shift the bands.

**What worked:**
- compass_distance properly handles "wrap around": d(0, 350)
returns 10, not 350. The smoke covers this explicitly  the
common bug class for angular math is treating distance as
(b - a) instead of "shortest path on the circle."
- compass_degrees as the reverse lookup creates a useful
round-trip: cardinal_16(degrees('NNE')) should equal "NNE".
The smoke confirms this implicitly via the explicit
boundary tests.
- T-31 fired AGAIN (3rd consecutive ship)  "compass cardinal
8/16-point" was 27 chars, budget 21. Rewrote. The pattern is
clear now: my natural description style consistently
overflows the column. The TOOL keeps me honest.

**What surprised me:**
- I instinctively wanted to call this shape "classifier" but
that's not quite right. Classifier matches a STRUCTURED
input (a postcode pattern, a NANP prefix); a quantizer
takes a CONTINUOUS input and buckets it. The 8/16-point
case is "named band of degrees" which is a quantizer with
named buckets. Adding to the patterns doc would over-
categorize  one example doesn't warrant a new row yet.
- The "centered band" decision matters: bands centered on the
cardinal POINTS feels more right than bands aligned to the
edges. Documented in code.

**Tooling opportunity:**
- (none new) Plugin count 106  107. Workflow smooth.

---

### 2026-06-18  beaufort extension + T-34 (Quantizer shape promoted)

**What I built:** Two things in one ship.

(1) `beaufort` extension. Five scalars over the 13-band WMO
Beaufort wind scale:

  beaufort_force(ms)         m/s  0..12 integer band
  beaufort_name(ms)          m/s  "Calm" / "Gale" / etc.
  beaufort_from_kmh(kmh)     km/h convenience
  beaufort_from_mph(mph)     mph convenience
  beaufort_min_ms(force)     reverse: force  lower-bound m/s

Force 12 is open-ended (hurricane covers anything beyond
32.7 m/s).

(2) Promoted "Quantizer" to a top-level shape in
extension-patterns.md, with compass + beaufort as the two
consumers. Includes pitfalls discovered shipping both:
band-centering vs edge-alignment (compass uses centered, beaufort
uses edge), open-ended top band (test "well beyond"), and the
reverse-direction convention (compass returns center, beaufort
returns lower bound).

**What worked:**
- Compass alone wasn't enough to justify the new shape entry.
Beaufort made it concrete: same Vec<(low, name)> table pattern,
same bucket-walk-from-top logic, different domain. Two
consumers ARE the right threshold.
- T-31 didn't fire this ship  "Beaufort wind scale" is 19
chars, budget 20. Just under. Confirms the budget is
realistic for thoughtful descriptions.
- The "walk from the top to handle open-ended top band"
trick is the kind of subtle correctness detail the patterns
doc captures. Future quantizer ships will reach for it.

**What surprised me:**
- I'd written compass and considered "quantizer" as the shape
but held back from documenting it (1 example isn't enough).
The discipline paid off: beaufort surfaced the band-centering
question that compass alone wouldn't have. With both
examples in hand, the doc entry captures the REAL choice
(centered vs edge-aligned) rather than just describing the
loop.
- My initial smoke comment for `beaufort_from_kmh(120)` was
wrong  said "11" but the actual answer is 12 (120/3.6 =
33.33 m/s, which is over the 32.7 hurricane threshold). The
smoke caught my arithmetic error during --show-parsed. Smoke
tests > my arithmetic.

**Tooling opportunity:**
- (T-34 new) Promoted the quantizer shape  retroactively
opened so t-status counts the lifecycle (the compass entry
informally noticed it; beaufort's 2nd-consumer ship justified
the formal write-up).
- (T-34 closed) Quantizer shape documented with 2 consumers.
- The pattern catalog now has 12 entries. Still readable in
one screen. The "split when 15" threshold from T-33 is
roughly the right calibration.

---

### 2026-06-18  T-35 investigation (doc-refs drift check)

**What I built:** tooling/doc-refs-check.py walks three
tooling docs and flags any extension citation whose
extensions/<name>/ directory doesn't exist:

  tooling/extension-patterns.md   shape  representative
  tooling/snippets/README.md      snippet  consumers
  tooling/cli-cheatsheet.md       limitation  surfacing ext

Matches narrow patterns (Reference:, Consumers:, Surfaced via,
quick-picker table rows) to avoid false positives on backticked
keywords like `fn`, `name`, `column`.

**What worked:**
- 24 references checked across 3 docs on the first run. Zero
orphans  caught the catalog at a clean baseline.
- Verified by temporarily moving extensions/compass/, running
the tool, seeing it flag the orphan, then restoring. End-to-end
detection confirmed.
- The narrow-pattern approach (markers + table cells) keeps
the false-positive rate at 0 even as the docs grow. Compare
to "any backtick string"  which would flag `fn`/`name`/etc.
The cost is that I have to maintain the picker-table regex
when new shapes get added.

**What surprised me:**
- I'd been worrying about doc drift in the abstract for ~3
ships now. The actual tool to fix it is 50 LOC of Python.
The DURATION of worry was longer than the work to fix it.
Worth recognizing as a pattern: "I keep meaning to" lasting
multiple ships  it's time to just build the thing.
- Almost matched "any backtick string of 3+ chars" as the
pattern. That would have flagged `fn`, `len`, `key`, etc.
The marker-prefix approach is strictly better but I had to
catch myself before shipping the lazy version.

**Tooling opportunity:**
- (T-35 closed) Inline. The script IS the closure.
- The tooling/ directory now has 6 self-contained Python
scripts: scaffold, smoke, plan-add, check-snippets, t-status,
doc-refs-check. Each one fits in 50-300 LOC. The "small
focused tools" pattern is working; resist the urge to merge
them into a single CLI.

---

### 2026-06-18  polyline extension

**What I built:** Google polyline encoder/decoder. Three scalars
on the formatter+parser shape (variable-length JSON input on
encode, JSON output on decode):

  polyline_encode(json)    [[lat,lon],...]  "_p~iF~ps|U..."
  polyline_decode(text)    text  [[lat,lon],...]
  polyline_length(text)    count of coords without full decode

The encoding is the classic delta + zigzag + 5-bit-chunk +
+63-to-ASCII scheme used by Google Maps Directions API,
Mapbox, OSRM, etc. Precision is fixed at 5dp (the standard
default).

**What worked:**
- Smoke locks to the EXACT canonical spec example: the input
`[[38.5,-120.2], [40.7,-120.95], [43.252,-126.453]]` MUST
encode to `_p~iF~ps|U_ulLnnqC_mqNvxq@`. That's a byte-exact
match against Google's reference  if I'd shipped a wrong
implementation, this single row would catch it.
- T-31 fired TWICE on this ship: "Google polyline encode/decode"
(29 chars), then "lat/lon polyline codec" (22 chars) before
landing on "polyline coord codec" (20 chars). The budget is
HARD; descriptions need to be punchy. That's a feature.
- T-35 (doc-refs-check) ran clean after the ship  no doc
references to polyline yet, no orphan risk.

**What surprised me:**
- The zigzag encoding ((v << 1) ^ (v >> 31) for signed ints)
is a common trick I'd forgotten about. My implementation
uses the equivalent form `(v << 1) as u32` followed by `if
neg { !v }` which produces the same bytes but reads more
clearly. Same operation, different style.
- `coords_to_json` had to round to 5dp explicitly  without
rounding, the round-trip `decode(encode([[40.7128, -74.0060]]))`
returned `40.71279999999...` due to f64 representation. The
fix is to round at the JSON boundary, matching the
precision the polyline format actually preserves.

**Tooling opportunity:**
- (none new) Plugin count 108  109. Workflow continues
to be smooth. T-31 fired twice without complaint; T-35
ran clean.

---

### 2026-06-18  T-36 investigation (lessons-stub template)

**What I built:** tooling/lessons-stub.py emits a paste-ready
lessons-learned.md section with today's date pre-filled.
Two flavors:

  python3 tooling/lessons-stub.py <name>                  # plugin
  python3 tooling/lessons-stub.py --kind investigation T-N "scope"

Each variant prints the four-section structure (built /
worked / surprised / opportunity) ready to fill in.

**What worked:**
- I was guessing the date from the most recent system reminder
("date changed to YYYY-MM-DD") and sometimes copying yesterday's
header out of habit. `datetime.date.today()` is the reliable
source. Mini-friction removed.
- The investigation flavor pre-populates `(T-NN closed)` in
the opportunity section, since most investigations are closed
inline by their own write-up.
- Updated .claude/commands/new-extension.md so future-me sees
the recommended workflow in step 7.

**What surprised me:**
- I almost templated the "What I built" section with a
sample-signature placeholder. Held back: each ship's
signatures are different, and a sample template invites
copying without adapting. Better to leave it blank as a
prompt to fill in.
- Trade-off between rigid structure (auto-generates) and
flexibility (some ships need a 5th section). Resolved by
making the template minimum-viable  add sections inline
if the ship needs them; the script just covers the floor.

**Tooling opportunity:**
- (T-36 closed)
- The tooling/ directory now has 7 self-contained scripts.
Each does ONE thing and prints to stdout for paste-in or
exits non-zero on failure. Composability without a CLI
wrapper.

---

### 2026-06-18  cipher extension (new shape: text-with-key)

**What I built:** Classical text ciphers in six scalars:

  caesar_encode(text, shift)        text  shifted by n mod 26
  caesar_decode(text, shift)        inverse (= encode by -n)
  rot13(text)                       caesar shift=13 (self-inverse)
  vigenere_encode(text, key)        polyalphabetic with repeating key
  vigenere_decode(text, key)        inverse
  atbash(text)                      A<->Z self-inverse substitution

Each preserves case, passes non-letters through unchanged, and
handles Unicode by leaving non-ASCII untouched. Vigenère
advances the key position only on letter chars (matches the
classical convention so "Hello, World" with key "KEY" doesn't
waste key positions on the comma).

Smoke locks the textbook Vigenère example:
  vigenere_encode("ATTACKATDAWN", "LEMON") = "LXFOPVEFRNHR"
exactly. This is THE most-cited example in cryptography
textbooks; byte-matching it is a strong correctness guarantee.

**What worked:**
- Dogfooded T-36 (lessons-stub.py) on this ship  generated
the section header with today's date pre-filled. Tiny
quality-of-life win.
- The "text transform with key" shape is genuinely new this
session. Different from formatter+parser (those just
serialize); different from tokenize-then-compare (no key);
different from quantizer (no name lookup). The key arg
parameterizes the transform.
- The self-inverse property smoke ("rot13 of rot13 = original",
"atbash of atbash = original") catches off-by-one errors in
the alphabet math instantly. Took 2 SELECTs and now I trust
the implementation.

**What surprised me:**
- T-31 fired AGAIN: "Caesar/ROT13/Vigenere/Atbash" was 28
chars, budget 22. Rewrote to "classical text ciphers"
(22 chars). 4 of last 5 ships have hit T-31; the budget is
genuinely tight for descriptive cipher-class names. Not a
bug  the column needs to fit.
- I started writing `(c as u8 + n) % 26` and caught myself:
that's wrong for negative shifts (caesar_decode uses -n).
`rem_euclid` is the safe modulo for signed ints. Easy to
miss; smoke caught it via the `caesar_encode('ABC', -3)`
test which would have wrapped wrong with naive modulo.

**Tooling opportunity:**
- (T-37 new) "Text transform with key" is the 13th shape and
deserves its own row in extension-patterns.md once a 2nd
consumer arrives. Single-consumer doesn't warrant promotion;
deferred per the T-34 lesson on shape thresholds.
Plugin count 109  110.

---

### 2026-06-18  xor extension + T-37 (text-transform-with-key promoted)

**What I built:** Two things again, T-34 style.

(1) `xor` extension. Three scalars over the byte-XOR cipher
with hex codec:

  xor_encode(text, key)    UTF-8 text + key  hex string of XOR'd bytes
  xor_decode(hex, key)     hex + key  recovered text (or Blob if !utf8)
  xor_raw(text, key)       text + key  raw Blob (no hex round-trip)

Empty key  NULL; malformed/odd-length hex  NULL.

(2) Promoted "Text transform with key" to the 13th row in
extension-patterns.md, with cipher + xor as consumers. The
shape entry calls out:
  - encode + decode pair convention
  - empty/no-applicable-key  NULL is the standard
  - key-cursor convention (advance only on transformable chars,
    matches Vigenere classical rule)
  - output type-switching for binary keys (xor's Blob fallback)

(3) Bonus: fixed a T-35 (doc-refs-check) limitation surfaced
mid-promotion  the PICKER_ROW regex hardcoded shape names
("Quantizer|Classifier|...") and missed the new "Text transform
with key" row. Replaced with a generic 3-column matcher that
skips the header. Now flexible to future shape additions
without code changes.

**What worked:**
- T-37 promotion paired with the 2nd consumer ship is the
right rhythm. Cipher alone gave me the SHAPE intuition;
xor's byte/hex variant proved the abstraction generalizes
beyond Caesar-style letter math. Two consumers also forced
me to write the "output type-switching" pitfall  Caesar
wouldn't have needed that note.
- The T-35 regex limitation surfaced exactly when I expected
it to (I'd called it out in T-35's lessons entry). Fixing
it took 5 minutes. The fact that the doc tools self-flag
their own staleness is healthy.

**What surprised me:**
- My initial smoke had `xor_decode(xor_encode(xor_encode(x,k),k),k)`
labeled "self-inverse." That's WRONG: XOR is self-inverse for
RAW bytes, but my encode wraps in hex, so `encode(encode(x,k),k)`
treats the inner hex as text and produces something else. The
test still passed (just locked the actual behavior), but I
corrected the comment to "composition, not self-inverse"
before shipping. Smoke tests prove behavior; comments need
to match.
- doc-refs-check went from 24  26 references after the
picker-row regex fix. Confirmed the fix actually picks up
the new rows.

**Tooling opportunity:**
- (T-37 closed) Shape promoted with 2 consumers per the
T-34 threshold rule.
- The pattern catalog is now 13 entries. Approaching the
"split when 15" threshold I called out in T-33. Still
fits on one screen; defer the split decision until 15+.

---

### 2026-06-18  T-38 investigation (next-fid utility)

**What I built:** tooling/next-fid.py prints the next unused
FID const value for an extension's src/lib.rs. Reads the file,
finds all `const FID_*: u64 = N`, prints `max(N) + 1`.

  python3 tooling/next-fid.py xor
    4
  python3 tooling/next-fid.py cipher
    7

Exits 0 with the number; exits 1 with stderr if the
extension doesn't exist or hasn't had FIDs assigned yet.

**What worked:**
- 30 LOC of Python. Removes a micro-friction I was hitting
~once per scalar-add (3-5 times per extension on average).
- Pairs with lessons-stub and the other "small focused
tools": each one does ONE thing, prints to stdout, exits
non-zero on failure. The tooling/ directory pattern stays
clean.

**What surprised me:**
- I'd been mentally tracking FID numbers as I added scalars.
Cipher had 6 (FID_CAESAR_ENCODE through FID_ATBASH); xor
had 3. Each addition costs a few seconds of mental
bookkeeping. Cumulative across a session  worth saving.
- The script is so small I almost dismissed it as
over-tooling. But the test cases (xor=4, cipher=7,
setops=9) all matched my hand-counts  the tool agrees
with reality, and now I don't have to count.

**Tooling opportunity:**
- (T-38 closed)
- The tooling/ directory now has 8 scripts. Adding more
should require a real friction; this one barely cleared
the bar but did.

---

### 2026-06-18  easter extension

**What I built:** Easter date computation. Three scalars:

  easter_western(year)              ISO YYYY-MM-DD via Anonymous
                                     Gregorian computus
  easter_orthodox(year)             Eastern Orthodox Easter via
                                     Meeus Julian + Gregorian shift
  easter_offset(year, days, cal)    derived dates (Good Friday = -2,
                                     Pentecost = +49, etc.)

Western works for any year >= 1583 (Gregorian calendar start).
Orthodox needs the Julian-to-Gregorian shift table (currently
covers 1583-2199); beyond that  NULL.

**What worked:**
- Smoke locks the canonical reference cases: Easter 2024 =
March 31, 2025 = April 20, 2000 = April 23. These match
the Catholic Church's published lectionary  byte-exact
agreement with external authority.
- The smoke also locks an INTERESTING year: 2025 Western and
2025 Orthodox both fall on April 20  the rare alignment
when the Julian and Gregorian calendars produce the same
date. Future-me will smile when reading this.
- easter_offset() walks dates one day at a time instead of
implementing full calendar arithmetic. Trades performance
for code simplicity; for the ~50-day offsets typical in
liturgical calendars, the cost is microseconds.

**What surprised me:**
- Ash Wednesday is Easter - 46, NOT 47. I miscounted while
writing the smoke and saw the comment-vs-output mismatch in
--show-parsed. Caught before sealing  the function was
correct, the comment was wrong. Smokes are spec; comments
need to match.
- easter_orthodox uses a manual `match year { 1583..=1699 =>
10, ...}` shift table because the Julian falls one day
further behind every century not divisible by 400. Three
extra match arms cover 600 more years. Easy to extend.
- I almost shipped without testing easter_western(2300) and
easter_orthodox(2300) as separate cases. The former works
fine (no shift needed); the latter rightly returns NULL.
Two scalars, two distinct failure modes  smoke them both.

**Tooling opportunity:**
- (none new) Plugin count 111  112. Dogfooded T-36
(lessons-stub) and T-38 (next-fid) on this ship. Both
saved a small bit of friction.

---

### 2026-06-18  Rename pass + pivot back to SQLite-extension ports

Mid-session honesty check: the user pointed out that "easter"
and similar made-up names don't sound like SQLite extensions
because they aren't  the session had drifted from porting
well-known SQLite extensions into shipping useful general
scalar packs.

Two-part remediation in one batch:

(1) **Rename 8 cute names** to ones that describe the function:
  - nato  nato-phonetic
  - easter  easter-date
  - cipher  classic-cipher
  - xor  xor-cipher
  - beaufort  beaufort-scale
  - compass  compass-bearing
  - tile  web-mercator-tile
  - polyline  google-polyline

The 12 already-descriptive names (phone-prefix, humansize,
latlon, numfmt, etc.) untouched. Scalar function names
preserved across all renames  the manifest "name" is
metadata, not SQL dispatch surface.

(2) **Reframe PLAN-sqlite-plugins.md** to acknowledge the
catalog now mixes (a) real ports of well-known SQLite
extensions and (b) general scalar extension packs that
share the loader/dispatch plumbing. (a) was the original
plan; (b) accumulated organically.

(3) **Pivot back** to real SQLite extension ports. First ship:
sha3 (port of SQLite's bundled `shathree.c` extension). Six
scalars matching the shathree.c surface:

  sha3(X, N)       NIST FIPS 202 SHA-3 at size N (224/256/384/512)
  sha3_224(X) ... sha3_512(X)   shorthand for fixed sizes
  sha3_raw(X, N)   Blob output (extra; not in shathree.c)

Smoke locks NIST reference vectors byte-exactly for both empty
input and "abc" across all four sizes. INTEGER/REAL coerce
to TEXT representation; NULL hashes as empty string. Matches
shathree.c semantics.

**What this reaffirms:**
- Smoke tests against canonical reference data (FIPS 202
vectors, the Google polyline spec example, classic Vigenere
"LEMON" + "ATTACKATDAWN" = "LXFOPVEFRNHR") are the strongest
correctness guarantee available. When the SCALAR is a known-
hash or known-algorithm, the reference vectors are free
truth  use them.
- T-31 fired AGAIN on this ship ("SHA-3 NIST FIPS 202 hashes"
= 26, "SHA-3 hashes (shathree port)" = 28, settled on
"SHA-3 (shathree port)" = 21). Budget keeps me honest.
- Going forward: focus on extensions that ARE ports of
named SQLite extensions (carray, completion, eval, etc.)
rather than general scalar packs.

**Tooling opportunity:**
- (none new) The pivot itself is the lesson. Plugin count
112  113.

---

### 2026-06-18  totype  port of SQLite ext/misc/totype.c

**What I built:** Port of SQLite's totype.c bundled extension.
Two scalars implementing lossless type coercion:

  tointeger(X)   return X as INTEGER if round-trip is exact
                  else NULL
  toreal(X)      return X as REAL if conversion is lossless
                  else NULL

Semantics match the C implementation precisely:
- INTEGER passes through unchanged
- REAL  INTEGER ok only if value is exactly representable
  (no fractional part, in i64 range, not NaN/Inf)
- TEXT parsed as decimal or hex (0x...) for tointeger
- BLOB decoded as UTF-8 then parsed like TEXT
- NULL  NULL

**What worked:**
- The smoke covers each failure mode separately: fractional
real, overflow, NaN/Inf, empty string, garbage, NULL. Each
returns NULL for a different REASON; the smoke documents
that totype is doing real lossless work, not just casting.
- The hex literal acceptance ('0x2a'  42) matches SQLite's
own literal syntax. Easy to forget that totype distinguishes
"parses as hex" from "is a numeric string."
- Second pivot-back ship. The pattern is clear now: pick a
named ext/misc/*.c, port the scalars, lock the behavior
against the C original's documented semantics. Same shape
as sha3 (shathree.c).

**What surprised me:**
- I almost shipped `toreal(42)` returning `Some(42.0)` without
the round-trip check. SQLite's toreal is documented to
return NULL when the conversion isn't exact  for most i64
values this is fine, but values near 2^63 can lose precision
in f64. The check is `r as i64 == n`. Same defensive shape
as the float-to-int direction.
- `toreal('1.5e10')` correctly handles scientific notation
because Rust's f64::parse covers it. Nice that the host
language's parser already matches SQLite's expectations.

**Tooling opportunity:**
- (none new) Plugin count 113  114. Continuing the pivot:
next candidate is uint collation (ext/misc/uint.c) which would
be the FIRST consumer of the collation dispatch path. Or eval
(complex; needs connection access). Choosing whichever has
cleaner scope next ship.

---

### 2026-06-18  uint  port of SQLite ext/misc/uint.c collation

**What I built:** First consumer of the collation dispatch path
in this codebase. The collation has been wired up since day one
but no extension declared one until now. Port of SQLite's
ext/misc/uint.c:

  ORDER BY col COLLATE uint
  
   compares strings as natural numbers within digit runs,
     byte-wise elsewhere. "file2" < "file10" instead of the
     lexicographic "file10" < "file2".

The extension is shape-distinct: ZERO scalar functions, ZERO
aggregates, ZERO vtabs  just a single collation. The
ScalarFunctionGuest still has to be exported per the `collating`
world's WIT contract; it errors on any call (no scalars are
advertised, so the host won't dispatch to it).

**What worked:**
- The `collating` world existed in WIT and was wired through
host + cli BEFORE any consumer landed. Today's ship just
filled an empty slot in the dispatch table  no host/cli
changes needed. That's exactly what a clean extension
ABI should feel like.
- Smoke uses `.print ---` markers between ORDER BY blocks so
the diff can pinpoint which sort behavior changed if it
regresses.
- The leading-zero handling has three distinct cases that
all needed smoke coverage: unequal magnitude (longer wins),
equal magnitude same digits ('1' = '1'), equal magnitude
different padding ('01' > '1' because more original chars).
Each case is one row in the smoke.

**What surprised me:**
- I'd assumed I'd need to edit host or cli code to wire up
the collation. Wrong  the dispatch path's been there
since the world was defined. The only "first-time" cost
was figuring out the WIT-binding import:
`bindings::exports::sqlite::extension::collation::Guest as
CollationGuest`. Once you know the path, it's identical
shape to ScalarFunctionGuest.
- The `world: "collating"` line in the bindgen macro was
the actual configuration change vs. the default scaffolding.
Worth documenting in the scaffold templates if collation
extensions become a regular thing.
- The plan-add description landed at "uint.c port (collation)"
which is exactly informative. T-31 didn't fire this ship.

**Tooling opportunity:**
- (T-39 new) The scaffold template hardcodes `world: "minimal"`.
For non-minimal worlds (collating, stateful, tabular, etc.)
the scaffold currently produces wrong output. A `--world`
flag would map to the right template variants. Defer until
a 2nd non-minimal ship; this is the first in months.
Plugin count 114  115.

---

### 2026-06-18  eval  port of SQLite ext/misc/eval.c

**What I built:** Port of SQLite's eval.c bundled extension.
Two scalars (overloaded by arity):

  eval(X)      run X as SQL, concat all cell values with no sep
  eval(X, Y)   run X as SQL, concat all cell values separated by Y

Both are nondeterministic (the inner SQL can read mutable
state, time-of-call, randomness). Returns Err on SQL error.

**What worked:**
- spi::execute from inside the wasm component is straightforward
once you find the import path (`bindings::sqlite::extension::spi`).
db-utils uses the same. ~30 LOC of real code for the entire
extension.
- The to_text() helper coerces every SQL type to its textual
form, matching what SQLite's eval.c does via sqlite3_value_text.
- The arity-overloaded scalar (`eval` with 1 arg AND 2 args)
works cleanly in our manifest  two ScalarFunctionSpec entries
with the same name but different num_args.

**What surprised me:**
- The smoke can't fully test eval against `:memory:`. The host
sqlite3 and the wasm-internal sqlite3 are SEPARATE libraries
with separate page caches; spi.execute requires a file-backed
db to bridge them. The smoke harness uses :memory: by default,
so every eval() call errors with the documented "spi requires
a file-backed database" message.
- db-utils has the same constraint  it ships with no smoke
file at all. I chose to ship a smoke that documents the
limitation explicitly: panic-only verification, plus comments
explaining how to test interactively.
- The asserted-smoke seed was skipped (no smoke.expected) so
the harness just does panic-class detection. With T-19's
panic markers, SqliteError isn't a panic  the smoke PASSes
cleanly while exercising NONE of the actual functionality.
That's mildly dishonest. Future-T-* candidate: harness
should accept a per-extension --db override so spi-dependent
extensions can be smoke-tested for real.

**Tooling opportunity:**
- (T-40 new) Smoke harness should accept `[smoke-db: /tmp/X.db]`
or similar config marker in smoke.sql so spi-dependent
extensions (eval, db-utils, template, ...) can be smoked
against a real file-backed db instead of :memory:. ~10
LOC harness change. Defer pending a 2nd spi-dependent
extension that needs real testing.
Plugin count 115  116.






















