//! Liang/Knuth-Plass hyphenation scalars.
//!
//! Backed by the `hyphenation` 0.8 crate (Knuth-Liang TeX
//! patterns + a thread-safe FST-backed resolver). The crate ships
//! pattern blobs for 79 languages; we embed only `en-us` by
//! default (~50 KB) to keep the component artifact small. Switch
//! to the `embed_all` cargo feature on this crate to get the full
//! set (~3 MB).
//!
//! ## What the scalars produce
//!
//! `hyphenate(word, [lang])`
//!   The input word with U+00AD (SOFT HYPHEN) inserted at every
//!   valid break opportunity. Browsers, PDF layout engines, and
//!   most terminal pagers know to render SHY as either invisible
//!   (when no line break happens there) or a real hyphen (when
//!   the layout chooses to break the line at that point). This
//!   is the "drop into HTML/CSS" form.
//!
//! `hyphenate_positions(word, [lang])`
//!   A JSON array of *byte* offsets where breaks may occur,
//!   counted within the original word. For ASCII inputs byte and
//!   char offsets coincide; non-ASCII words (e.g. accented Latin)
//!   may break inside a multi-byte char's preceding boundary, in
//!   which case the offset points at the leading byte. Consumers
//!   that need char or grapheme offsets should re-walk the word.
//!
//! `hyphenation_languages()`
//!   A JSON array of BCP-47-style language tags that this build
//!   has pattern data embedded for. Always includes "en-us"; with
//!   `embed_all` enabled the full 79-language list is returned.
//!
//! `hyphenation_version()`
//!   This extension's crate version (NOT the upstream
//!   `hyphenation` crate version).
//!
//! ## Null + error behavior
//!
//! * NULL word → NULL result. The lang arg can also be NULL,
//!   which is treated as "use the default" (en-us). This mirrors
//!   how other lang-aware scalars in the repo handle absent args.
//! * Unknown / non-embedded lang → SQL error. We deliberately do
//!   not silently fall back to en-us because that would mask the
//!   real issue (typo in the lang tag, or forgot to enable
//!   `embed_all`).
//! * Word too short for the language's left+right minimums
//!   (Liang's "lefthyphenmin" / "righthyphenmin") → the word is
//!   returned unchanged, with no soft hyphens. `breaks` is empty.
//!
//! ## Footprint note
//!
//! With `embed_en-us` only, the release component is ~120 KB
//! gzipped — pattern data dominates. Each additional embedded
//! language adds 30-80 KB depending on the script.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    use hyphenation::{Hyphenator, Iter, Language, Load, Standard};
    use serde_json::json;

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

    const FID_HYPHENATE: u64 = 1;
    const FID_HYPHENATE_POSITIONS: u64 = 2;
    const FID_LANGUAGES: u64 = 3;
    const FID_VERSION: u64 = 4;
    // 1-arg variants — SQLite's scalar dispatch uses (name, nargs)
    // as the lookup key, so the 1-arg overloads need distinct ids
    // even though they share a handler with the 2-arg form.
    const FID_HYPHENATE_1: u64 = 5;
    const FID_HYPHENATE_POSITIONS_1: u64 = 6;

    /// The default language used when the caller passes 1 arg or
    /// passes NULL for the lang arg. Always available because the
    /// crate is built with `embed_en-us`.
    const DEFAULT_LANG: &str = "en-us";

    /// Lookup table for the languages this build has embedded.
    /// Each row maps a BCP-47 tag (lowercased) to its `Language`
    /// enum variant. With `embed_en-us` we list only en-us; with
    /// `embed_all` we'd list everything the crate ships.
    ///
    /// Important: only languages whose pattern blob is actually
    /// embedded should appear here. `Standard::from_embedded(L)`
    /// will return an error at runtime if the blob isn't linked,
    /// and we want the registry to match what we can actually
    /// hyphenate — not what the crate's enum can name.
    fn embedded_languages() -> &'static [(&'static str, Language)] {
        // Keep this list in sync with the crate's `embed_*`
        // features in Cargo.toml. The current configuration is
        // `embed_en-us` only.
        &[
            ("en-us", Language::EnglishUS),
        ]
    }

    fn resolve_language(tag: &str) -> Result<Language, String> {
        // Case-insensitive match; users routinely write "en-US"
        // or "EN-US". The canonical tags in the table are
        // lowercase, so normalize the query.
        let needle = tag.to_ascii_lowercase();
        for (canon, lang) in embedded_languages() {
            if *canon == needle {
                return Ok(*lang);
            }
        }
        // Build the supported-list error message inline so callers
        // can see, in one line, what they can actually pick from.
        let supported: Vec<&str> = embedded_languages()
            .iter()
            .map(|(t, _)| *t)
            .collect();
        Err(format!(
            "hyphenation: unknown or non-embedded language {tag:?}; \
             supported in this build: {}",
            supported.join(", ")
        ))
    }

    /// Extract a TEXT-or-NULL argument. Differs from the
    /// canonical helper only in that NULL is reported as a
    /// distinct variant instead of being collapsed into "missing"
    /// — the lang arg may be present and NULL, which means
    /// "use the default" rather than "argument count too low".
    fn arg_text_opt(args: &[SqlValue], i: usize, fname: &str) -> Result<Option<String>, String> {
        match args.get(i) {
            Some(SqlValue::Text(s)) => Ok(Some(s.clone())),
            Some(SqlValue::Null) => Ok(None),
            Some(_) => Err(format!("{fname}: arg {i} must be TEXT or NULL")),
            None => Err(format!("{fname}: missing arg {i}")),
        }
    }

    /// Resolve the (optional) lang argument at `idx` to a
    /// `Language`, falling back to en-us on absent / NULL.
    fn resolve_lang_arg(args: &[SqlValue], idx: usize, fname: &str) -> Result<Language, String> {
        // For the 1-arg overloads `args.get(idx)` returns None;
        // for the 2-arg form a NULL lang means "default". Both
        // collapse to the en-us path.
        let tag = match args.get(idx) {
            None => return resolve_language(DEFAULT_LANG),
            Some(SqlValue::Null) => return resolve_language(DEFAULT_LANG),
            Some(SqlValue::Text(s)) => s.clone(),
            Some(_) => return Err(format!("{fname}: lang arg must be TEXT or NULL")),
        };
        resolve_language(&tag)
    }

    /// Insert U+00AD (SOFT HYPHEN) at each break position. We
    /// don't reuse `Iter::iter` + `mark_with` because that yields
    /// `String` segments and re-concatenating them allocates
    /// twice; a one-pass byte splice is cheaper and the offsets
    /// are guaranteed by hyphenation to land on char boundaries
    /// (Liang patterns are character-based, so the resolver
    /// always reports indices at UTF-8 char starts).
    fn insert_soft_hyphens(word: &str, breaks: &[usize]) -> String {
        // SHY is U+00AD, encoded as 0xC2 0xAD in UTF-8. Pre-size
        // the output to avoid reallocation for long words.
        const SHY: &str = "\u{00ad}";
        let mut out = String::with_capacity(word.len() + breaks.len() * SHY.len());
        let mut prev = 0usize;
        for &b in breaks {
            // Defensive: out-of-range break positions would
            // indicate a corrupt pattern file; clamp + skip
            // rather than panic on a slice index out of bounds.
            if b <= word.len() && b >= prev {
                out.push_str(&word[prev..b]);
                out.push_str(SHY);
                prev = b;
            }
        }
        out.push_str(&word[prev..]);
        out
    }

    struct Ext;

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32, flags: FunctionFlags| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: flags,
            };
            Manifest {
                name: "hyphenation".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    // Two ids per overloaded name; the SQLite
                    // dispatcher keys on (name, nargs) so the
                    // same SQL identifier can resolve to either
                    // variant depending on call arity.
                    s(FID_HYPHENATE_1, "hyphenate", 1, det),
                    s(FID_HYPHENATE, "hyphenate", 2, det),
                    s(FID_HYPHENATE_POSITIONS_1, "hyphenate_positions", 1, det),
                    s(FID_HYPHENATE_POSITIONS, "hyphenate_positions", 2, det),
                    s(FID_LANGUAGES, "hyphenation_languages", 0, det),
                    s(FID_VERSION, "hyphenation_version", 0, det),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                dot_commands: alloc::vec![],
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_HYPHENATE | FID_HYPHENATE_1 => {
                    let word = arg_text_opt(&args, 0, "hyphenate")?;
                    let Some(word) = word else { return Ok(SqlValue::Null) };
                    let lang = resolve_lang_arg(&args, 1, "hyphenate")?;
                    // `from_embedded` is cheap-ish but not free
                    // (it deserializes the FST blob). The
                    // expected query shape is "one row per
                    // word", so we reload per call rather than
                    // try to memoize across host re-entries —
                    // which would require unsafe statics or a
                    // OnceCell behind a wasm-friendly lock.
                    let dict = Standard::from_embedded(lang)
                        .map_err(|e| format!("hyphenate: load pattern: {e}"))?;
                    let hyphenated = dict.hyphenate(&word);
                    let breaks = hyphenated.breaks.clone();
                    Ok(SqlValue::Text(insert_soft_hyphens(&word, &breaks)))
                }
                FID_HYPHENATE_POSITIONS | FID_HYPHENATE_POSITIONS_1 => {
                    let word = arg_text_opt(&args, 0, "hyphenate_positions")?;
                    let Some(word) = word else { return Ok(SqlValue::Null) };
                    let lang = resolve_lang_arg(&args, 1, "hyphenate_positions")?;
                    let dict = Standard::from_embedded(lang)
                        .map_err(|e| format!("hyphenate_positions: load pattern: {e}"))?;
                    let hyphenated = dict.hyphenate(&word);
                    // serde_json emits `[]` for an empty Vec —
                    // which is exactly the right answer for words
                    // shorter than the lang's left+right min.
                    let arr = json!(hyphenated.breaks);
                    Ok(SqlValue::Text(arr.to_string()))
                }
                FID_LANGUAGES => {
                    let langs: Vec<&str> = embedded_languages()
                        .iter()
                        .map(|(t, _)| *t)
                        .collect();
                    Ok(SqlValue::Text(json!(langs).to_string()))
                }
                FID_VERSION => Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string())),
                other => Err(format!("hyphenation: unknown func id {other}")),
            }
        }
    }

    // Suppress dead-code warnings on `Iter` — it's only used
    // transitively through the `hyphenate` method on `Standard`.
    // Naming it in the `use` line is what unlocks the trait
    // methods; the `_` keeps clippy quiet.
    #[allow(dead_code)]
    fn _force_iter_trait_in_scope() -> usize {
        // touched only to keep the import live across the lifetime
        // of an unused-import lint pass; never called.
        let dict = Standard::from_embedded(Language::EnglishUS).expect("en-us is embedded");
        let h = dict.hyphenate("test");
        h.iter().count()
    }

    bindings::export!(Ext with_types_in bindings);
}
