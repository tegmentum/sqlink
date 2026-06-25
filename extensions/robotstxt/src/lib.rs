//! robots.txt parsing + matching for SQLite.
//!
//! Wraps Folyd's `robotstxt` 0.3 crate -- the canonical Rust port of
//! Google's reference C++ robots.txt parser (the one Googlebot ships).
//! Aligns with RFC 9309 plus the de facto Google extensions
//! (`Sitemap:`, `Crawl-delay:`) the I-D explicitly allows under
//! "other records" (section 2.2.4).
//!
//! Function surface:
//!
//!   robots_is_allowed(robots_txt, user_agent, url)  -> integer (0 / 1)
//!   robots_crawl_delay(robots_txt, user_agent)      -> real (seconds; NULL if absent)
//!   robots_sitemaps(robots_txt)                     -> text (JSON array of sitemap URLs)
//!   robotstxt_version()                             -> text
//!
//! Argument coercion:
//!   * `robots_txt`, `user_agent`, `url`: TEXT (utf-8). BLOB also
//!     accepted -- robots.txt is ascii-ish and we let the parser
//!     digest whatever bytes we hand it. NULL on any arg -> NULL out.
//!
//! `robots_is_allowed` defers to `DefaultMatcher::one_agent_allowed_by_robots`
//! (longest-match-wins, Allow on tie, as Googlebot does).
//!
//! `robots_crawl_delay` walks the robots body via our own
//! `RobotsParseHandler` impl since the matcher doesn't expose
//! crawl-delay (it's a Google extension, not strict REP). UA matching
//! mirrors the matcher: case-insensitive identity per group;
//! specific-UA group wins, `*` group is fallback. Returns NULL if no
//! crawl-delay record applies. Numeric parsing tolerates floats
//! ("0.5"), integers ("10"), and -- per Bing's documented
//! convention -- silently skips non-numeric values.
//!
//! `robots_sitemaps` walks the body for top-level `Sitemap:` records
//! (RFC 9309 says they're global, not UA-scoped) and emits a JSON
//! array. Empty robots body -> "[]". URLs are emitted verbatim
//! (no normalization) and JSON-escaped for the SQL surface.
//!
//! NULL semantics: any NULL arg -> NULL output. Wrong-type args
//! (INTEGER / REAL for a string slot) -> NULL, not error, so the
//! functions compose in CASE / WHERE without surfacing errors.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

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

    use robotstxt::{parse_robotstxt, DefaultMatcher, RobotsParseHandler};

    const FID_IS_ALLOWED: u64 = 1;
    const FID_CRAWL_DELAY: u64 = 2;
    const FID_SITEMAPS: u64 = 3;
    const FID_VERSION: u64 = 4;

    struct Ext;

    /// Coerce an SqlValue to a `String`. TEXT -> as-is, BLOB ->
    /// utf-8 lossy (robots.txt + URLs are ASCII in practice; we
    /// don't want to reject a Windows-newline body that happens to
    /// have stray bytes). NULL -> Ok(None). Numeric -> None to keep
    /// the SQL surface forgiving.
    fn opt_str(v: &SqlValue) -> Option<String> {
        match v {
            SqlValue::Text(s) => Some(s.clone()),
            SqlValue::Blob(b) => Some(String::from_utf8_lossy(b).into_owned()),
            _ => None,
        }
    }

    /// Strict-NULL helper: returns None if the slot is missing or
    /// NULL, else returns the coerced string. Used by the
    /// "any-NULL-in -> NULL-out" gate at the top of each function.
    fn req_str(args: &[SqlValue], i: usize) -> Result<Option<String>, String> {
        match args.get(i) {
            None => Err(format!("missing arg at {i}")),
            Some(SqlValue::Null) => Ok(None),
            Some(v) => Ok(opt_str(v)),
        }
    }

    // --- crawl-delay + sitemap handler -------------------------------

    /// Per-group state: which UA tokens head this group and which
    /// crawl-delay (if any) the group declared. We accumulate one
    /// group at a time; a new `User-agent:` line either appends to
    /// the current group's UA list (when consecutive) or starts a
    /// new group (when separated by an Allow / Disallow / unknown).
    #[derive(Default, Clone)]
    struct Group {
        uas: Vec<String>,
        crawl_delay: Option<f64>,
    }

    /// Sitemap collector + per-UA crawl-delay tracker. Implements
    /// `RobotsParseHandler` from the `robotstxt` crate so we can run
    /// it through the same parser as the matcher.
    #[derive(Default)]
    struct Walker {
        // Sitemap URLs in document order. Sitemaps are global (the
        // I-D treats them as "other records" not bound to any UA).
        sitemaps: Vec<String>,
        // Accumulated groups in document order.
        groups: Vec<Group>,
        // True if the previous directive was `User-agent:`. That
        // lets us know whether the next `User-agent:` is part of
        // the *same* group (consecutive UA lines) or starts a new
        // one (the previous UA-line group has rules between).
        last_was_user_agent: bool,
    }

    impl Walker {
        /// On any non-UA directive (Allow / Disallow / crawl-delay /
        /// other-unknown), the "is this still the UA-introduction"
        /// flag toggles off so the *next* `User-agent:` opens a
        /// fresh group.
        fn note_rule(&mut self) {
            self.last_was_user_agent = false;
        }

        /// Get or create the group the current rule line belongs to.
        /// If we never saw a UA (rules before any `User-agent:`),
        /// open an implicit `*` group -- the Google parser treats
        /// that as global.
        fn current_group(&mut self) -> &mut Group {
            if self.groups.is_empty() {
                let mut g = Group::default();
                g.uas.push("*".to_string());
                self.groups.push(g);
            }
            self.groups.last_mut().unwrap()
        }
    }

    impl RobotsParseHandler for Walker {
        fn handle_robots_start(&mut self) {
            self.sitemaps.clear();
            self.groups.clear();
            self.last_was_user_agent = false;
        }

        fn handle_robots_end(&mut self) {}

        fn handle_user_agent(&mut self, _line_num: u32, user_agent: &str) {
            let ua = user_agent.trim();
            if ua.is_empty() {
                return;
            }
            if self.last_was_user_agent {
                // Consecutive UA lines -> same group, just add another
                // alias to the current group.
                self.current_group().uas.push(ua.to_string());
            } else {
                // Either first UA line or a UA line after rules ->
                // new group.
                let mut g = Group::default();
                g.uas.push(ua.to_string());
                self.groups.push(g);
                self.last_was_user_agent = true;
            }
        }

        fn handle_allow(&mut self, _line_num: u32, _value: &str) {
            self.note_rule();
        }

        fn handle_disallow(&mut self, _line_num: u32, _value: &str) {
            self.note_rule();
        }

        fn handle_sitemap(&mut self, _line_num: u32, value: &str) {
            let v = value.trim();
            if !v.is_empty() {
                self.sitemaps.push(v.to_string());
            }
            self.note_rule();
        }

        fn handle_unknown_action(&mut self, _line_num: u32, action: &str, value: &str) {
            // Crawl-delay is the only unknown directive we surface.
            // Bing + Yandex use it; Google ignores it. The directive
            // name match is case-insensitive per the I-D's
            // record-name rules.
            if action.eq_ignore_ascii_case("crawl-delay") {
                if let Ok(n) = value.trim().parse::<f64>() {
                    if n.is_finite() && n >= 0.0 {
                        // Last-write-wins within a group: if the
                        // group already had a delay, override it.
                        // This matches Bing's "the highest value
                        // wins" only loosely, but the most common
                        // robots.txt shape has at most one
                        // crawl-delay per group.
                        self.current_group().crawl_delay = Some(n);
                    }
                }
            }
            self.note_rule();
        }
    }

    /// Pick the crawl-delay applicable to `requested_ua`. Matching is
    /// case-insensitive identity per the Google matcher's
    /// `extract_user_agent` rule. UA-specific group wins; `*` group
    /// is fallback; absent -> None.
    fn pick_crawl_delay(groups: &[Group], requested_ua: &str) -> Option<f64> {
        let req = requested_ua.trim();
        // The matcher's extract_user_agent stops at the first non
        // [a-zA-Z_-] char. Mirror that so "Googlebot/2.1" matches a
        // "Googlebot" header.
        let req_token: String = req
            .chars()
            .take_while(|c| c.is_ascii_alphabetic() || *c == '-' || *c == '_')
            .collect();

        let mut star_delay: Option<f64> = None;
        for g in groups {
            for ua in &g.uas {
                let ua = ua.trim();
                if ua == "*" {
                    if let Some(d) = g.crawl_delay {
                        star_delay = Some(d);
                    }
                    continue;
                }
                if !req_token.is_empty() && ua.eq_ignore_ascii_case(&req_token) {
                    if let Some(d) = g.crawl_delay {
                        return Some(d);
                    }
                }
            }
        }
        star_delay
    }

    /// JSON-escape a string for embedding in a JSON array. Handles
    /// the seven mandatory escapes (", \, \b, \f, \n, \r, \t) plus
    /// other control chars (< 0x20) as \u00XX. We don't escape
    /// non-ASCII -- the SQL TEXT type is utf-8 and embedded utf-8 in
    /// a JSON string is well-formed.
    fn json_escape(s: &str, out: &mut String) {
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\u{08}' => out.push_str("\\b"),
                '\u{0c}' => out.push_str("\\f"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => {
                    out.push_str(&format!("\\u{:04x}", c as u32));
                }
                c => out.push(c),
            }
        }
        out.push('"');
    }

    fn sitemaps_to_json(sitemaps: &[String]) -> String {
        let mut s = String::from("[");
        for (i, url) in sitemaps.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            json_escape(url, &mut s);
        }
        s.push(']');
        s
    }

    // --- guest impl --------------------------------------------------

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
                name: "robotstxt".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_IS_ALLOWED, "robots_is_allowed", 3, det),
                    s(FID_CRAWL_DELAY, "robots_crawl_delay", 2, det),
                    s(FID_SITEMAPS, "robots_sitemaps", 1, det),
                    s(FID_VERSION, "robotstxt_version", 0, det),
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
                preferred_prefix: Some("robotstxt".into()),
                prefix_expansion: Some("com.tegmentum.sqlink.ext.robotstxt".into()),
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_IS_ALLOWED => {
                    let Some(body) = req_str(&args, 0)? else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(ua) = req_str(&args, 1)? else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(url) = req_str(&args, 2)? else {
                        return Ok(SqlValue::Null);
                    };
                    // DefaultMatcher needs &'a str borrows that all
                    // outlive the call; the locals here own them so
                    // that's automatic.
                    let mut matcher = DefaultMatcher::default();
                    let allowed = matcher.one_agent_allowed_by_robots(&body, &ua, &url);
                    Ok(SqlValue::Integer(allowed as i64))
                }

                FID_CRAWL_DELAY => {
                    let Some(body) = req_str(&args, 0)? else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(ua) = req_str(&args, 1)? else {
                        return Ok(SqlValue::Null);
                    };
                    let mut walker = Walker::default();
                    parse_robotstxt(&body, &mut walker);
                    Ok(match pick_crawl_delay(&walker.groups, &ua) {
                        Some(d) => SqlValue::Real(d),
                        None => SqlValue::Null,
                    })
                }

                FID_SITEMAPS => {
                    let Some(body) = req_str(&args, 0)? else {
                        return Ok(SqlValue::Null);
                    };
                    let mut walker = Walker::default();
                    parse_robotstxt(&body, &mut walker);
                    Ok(SqlValue::Text(sitemaps_to_json(&walker.sitemaps)))
                }

                FID_VERSION => {
                    // The robotstxt crate's own version is pinned in
                    // Cargo.toml; surface it alongside ours so callers
                    // can assert against either independently.
                    let v = format!(
                        "robotstxt crate 0.3 (Google REP / RFC 9309); extension {}",
                        env!("CARGO_PKG_VERSION")
                    );
                    Ok(SqlValue::Text(v))
                }

                other => Err(format!("robotstxt: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
