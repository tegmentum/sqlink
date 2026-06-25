//! Pure-function argv parser for the sqlink cli.
//!
//! The cli crate (`sqlite-cli`) is `#![cfg(target_arch = "wasm32")]`
//! because its wit-bindgen-emitted trampolines reference symbols
//! that only exist for wasm targets. That gate also means native
//! `cargo test` produces zero tests for the cli, leaving the argv
//! parser uncovered.
//!
//! This crate hosts the parsing logic as plain Rust on `&[String]`
//! / `&str` so it builds for any target and is unit-testable. The
//! cli imports `parse_argv` and feeds it `std::env::args()`.
//!
//! ## Argv shape (sqlite-cli)
//!
//! ```text
//! sqlite_cli.component.wasm <db_path>
//!     [--load FILE.wasm]*
//!     [--keep-open]
//!     [--bundle-grant-spawn-build]
//!     [.NAME args...]
//! ```
//!
//! `--bundle-grant-spawn-build` may appear at any position; the
//! original code searches the whole argv for it before walking
//! the rest. We preserve that semantics so passing it after the
//! dot-cmd still grants.

/// Parsed cli argv.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedArgs {
    /// `argv[1]`, treated as the db path. Empty string if absent.
    pub db_path: String,
    /// True if `--bundle-grant-spawn-build` appears anywhere in
    /// argv (Gap C plumbing; passed through to the core-dotcmd
    /// auto-load so bundle-cli gets SpawnBuild).
    pub bundle_grant_spawn_build: bool,
    /// True if `--keep-open` appears before the dot-cmd.
    pub keep_open: bool,
    /// `--load FILE` values, in order. Each becomes a synthesized
    /// `.load FILE` line.
    pub preload: Vec<String>,
    /// Trailing tokens starting at the first `.NAME` argument.
    /// Joined with spaces to form the single dot-cmd line.
    pub dot_args: Vec<String>,
    /// True if any token starting with `.` was seen.
    pub dot_seen: bool,
}

/// Parse the cli's argv. `argv[0]` (the program name) is ignored
/// per the original code path; `argv[1]` becomes `db_path`. The
/// parser is forgiving (mirrors the original behaviour):
///
/// - Unknown args before a dot-cmd are silently dropped.
/// - `--load` without a following value drops the flag (no panic).
/// - After the first `.NAME` token everything else is dot-cmd args.
/// - `--bundle-grant-spawn-build` is detected anywhere in argv,
///   including after the dot-cmd boundary.
pub fn parse_argv(argv: &[String]) -> ParsedArgs {
    let mut out = ParsedArgs::default();

    out.bundle_grant_spawn_build = argv.iter().any(|a| a == "--bundle-grant-spawn-build");

    if argv.len() > 1 {
        out.db_path = argv[1].clone();
    }

    let mut i = 2;
    while i < argv.len() {
        let a = &argv[i];
        if !out.dot_seen {
            if a == "--keep-open" {
                out.keep_open = true;
                i += 1;
                continue;
            }
            if a == "--load" {
                i += 1;
                if i < argv.len() {
                    out.preload.push(argv[i].clone());
                }
                i += 1;
                continue;
            }
            if a.starts_with('.') {
                out.dot_seen = true;
                out.dot_args.push(a.clone());
                i += 1;
                continue;
            }
        } else {
            out.dot_args.push(a.clone());
        }
        i += 1;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv<I, S>(parts: I) -> Vec<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        parts.into_iter().map(|s| s.as_ref().to_string()).collect()
    }

    #[test]
    fn empty_argv_is_default() {
        let a = argv::<_, &str>([]);
        let r = parse_argv(&a);
        assert_eq!(r, ParsedArgs::default());
    }

    #[test]
    fn only_program_name_is_default() {
        let a = argv(["sqlite_cli.wasm"]);
        let r = parse_argv(&a);
        assert_eq!(r, ParsedArgs::default());
    }

    #[test]
    fn db_path_from_argv_1() {
        let a = argv(["sqlite_cli.wasm", "/tmp/x.db"]);
        let r = parse_argv(&a);
        assert_eq!(r.db_path, "/tmp/x.db");
        assert!(r.preload.is_empty());
        assert!(!r.dot_seen);
    }

    #[test]
    fn single_preload() {
        let a = argv([
            "sqlite_cli.wasm",
            "/tmp/x.db",
            "--load",
            "uuid.component.wasm",
        ]);
        let r = parse_argv(&a);
        assert_eq!(r.preload, vec!["uuid.component.wasm".to_string()]);
        assert!(!r.dot_seen);
    }

    #[test]
    fn multiple_preloads_preserved_in_order() {
        let a = argv([
            "sqlite_cli.wasm",
            "/tmp/x.db",
            "--load",
            "a.wasm",
            "--load",
            "b.wasm",
            "--load",
            "c.wasm",
        ]);
        let r = parse_argv(&a);
        assert_eq!(
            r.preload,
            vec![
                "a.wasm".to_string(),
                "b.wasm".to_string(),
                "c.wasm".to_string()
            ]
        );
    }

    #[test]
    fn load_without_value_silently_dropped() {
        // Mirrors original code: --load with no next arg is a no-op.
        let a = argv(["sqlite_cli.wasm", "/tmp/x.db", "--load"]);
        let r = parse_argv(&a);
        assert!(r.preload.is_empty());
    }

    #[test]
    fn keep_open_flag() {
        let a = argv(["sqlite_cli.wasm", "/tmp/x.db", "--keep-open"]);
        let r = parse_argv(&a);
        assert!(r.keep_open);
    }

    #[test]
    fn dot_cmd_starts_dot_mode() {
        let a = argv(["sqlite_cli.wasm", "/tmp/x.db", ".bundle"]);
        let r = parse_argv(&a);
        assert!(r.dot_seen);
        assert_eq!(r.dot_args, vec![".bundle".to_string()]);
    }

    #[test]
    fn dot_cmd_with_args_collected() {
        let a = argv([
            "sqlite_cli.wasm",
            "/tmp/x.db",
            ".bundle",
            "save",
            "myset",
            "--no-build",
        ]);
        let r = parse_argv(&a);
        assert!(r.dot_seen);
        assert_eq!(
            r.dot_args,
            vec![
                ".bundle".to_string(),
                "save".to_string(),
                "myset".to_string(),
                "--no-build".to_string(),
            ]
        );
    }

    #[test]
    fn flags_after_dot_cmd_become_dot_args() {
        // Anything after the first dot token is dot-cmd payload,
        // including things that look like flags. Original behaviour.
        let a = argv([
            "sqlite_cli.wasm",
            "/tmp/x.db",
            ".bundle",
            "save",
            "myset",
            "--keep-open", // this is now a dot-cmd arg, not a cli flag
        ]);
        let r = parse_argv(&a);
        assert!(
            !r.keep_open,
            "--keep-open after dot-cmd is dot-arg, not flag"
        );
        assert!(r.dot_args.contains(&"--keep-open".to_string()));
    }

    #[test]
    fn load_before_dot_cmd_still_works() {
        let a = argv([
            "sqlite_cli.wasm",
            "/tmp/x.db",
            "--load",
            "uuid.wasm",
            ".bundle",
            "save",
            "myset",
        ]);
        let r = parse_argv(&a);
        assert_eq!(r.preload, vec!["uuid.wasm".to_string()]);
        assert!(r.dot_seen);
        assert_eq!(
            r.dot_args,
            vec![
                ".bundle".to_string(),
                "save".to_string(),
                "myset".to_string()
            ]
        );
    }

    #[test]
    fn keep_open_and_load_combined() {
        let a = argv([
            "sqlite_cli.wasm",
            "/tmp/x.db",
            "--load",
            "a.wasm",
            "--keep-open",
            "--load",
            "b.wasm",
        ]);
        let r = parse_argv(&a);
        assert!(r.keep_open);
        assert_eq!(r.preload, vec!["a.wasm".to_string(), "b.wasm".to_string()]);
    }

    #[test]
    fn bundle_grant_spawn_build_before_dot() {
        let a = argv([
            "sqlite_cli.wasm",
            "/tmp/x.db",
            "--bundle-grant-spawn-build",
            ".bundle",
            "build",
            "myset",
        ]);
        let r = parse_argv(&a);
        assert!(r.bundle_grant_spawn_build);
    }

    #[test]
    fn bundle_grant_spawn_build_after_dot_still_detected() {
        // Original code does a whole-argv search, so this works
        // even though everything after the dot is normally dot-args.
        let a = argv([
            "sqlite_cli.wasm",
            "/tmp/x.db",
            ".bundle",
            "build",
            "myset",
            "--bundle-grant-spawn-build",
        ]);
        let r = parse_argv(&a);
        assert!(r.bundle_grant_spawn_build);
        // It ALSO ends up in dot_args (whole-argv-search is
        // independent of the walk):
        assert!(r
            .dot_args
            .contains(&"--bundle-grant-spawn-build".to_string()));
    }

    #[test]
    fn bundle_grant_spawn_build_absent() {
        let a = argv(["sqlite_cli.wasm", "/tmp/x.db", ".bundle", "list"]);
        let r = parse_argv(&a);
        assert!(!r.bundle_grant_spawn_build);
    }

    #[test]
    fn unknown_flag_before_dot_silently_dropped() {
        // Mirrors original: any non-recognized arg before the
        // dot-cmd is skipped. No error, no panic.
        let a = argv([
            "sqlite_cli.wasm",
            "/tmp/x.db",
            "--unknown-flag",
            "garbage",
            ".bundle",
            "list",
        ]);
        let r = parse_argv(&a);
        assert!(r.dot_seen);
        assert_eq!(r.dot_args, vec![".bundle".to_string(), "list".to_string()]);
        // The unknown stuff didn't leak into dot_args:
        assert!(!r
            .dot_args
            .iter()
            .any(|s| s == "--unknown-flag" || s == "garbage"));
    }

    #[test]
    fn db_path_empty_when_first_arg_starts_with_dash() {
        // The original code unconditionally uses argv[1] as
        // db_path — even if it looks like a flag. We preserve.
        let a = argv(["sqlite_cli.wasm", "--load", "a.wasm"]);
        let r = parse_argv(&a);
        assert_eq!(r.db_path, "--load");
        // And then the parser walk from i=2 sees "a.wasm" which
        // is neither a flag nor a dot-cmd and is silently dropped:
        assert!(r.preload.is_empty());
        assert!(!r.dot_seen);
    }

    #[test]
    fn quoted_dot_args_preserved_as_separate_tokens() {
        // Shell quoting is done by the host (argv arrives pre-split).
        // We just verify we don't re-tokenize.
        let a = argv([
            "sqlite_cli.wasm",
            "/tmp/x.db",
            ".bundle",
            "save",
            "name with spaces",
        ]);
        let r = parse_argv(&a);
        assert_eq!(r.dot_args.last(), Some(&"name with spaces".to_string()));
    }

    #[test]
    fn dot_args_empty_when_no_dot_cmd() {
        let a = argv(["sqlite_cli.wasm", "/tmp/x.db", "--keep-open"]);
        let r = parse_argv(&a);
        assert!(!r.dot_seen);
        assert!(r.dot_args.is_empty());
    }

    #[test]
    fn dot_cmd_only_no_args() {
        let a = argv(["sqlite_cli.wasm", "/tmp/x.db", ".help"]);
        let r = parse_argv(&a);
        assert!(r.dot_seen);
        assert_eq!(r.dot_args, vec![".help".to_string()]);
    }

    #[test]
    fn realistic_full_invocation() {
        // Sanity: an end-to-end-looking invocation.
        let a = argv([
            "sqlite_cli.component.wasm",
            "data.sqlite",
            "--load",
            "/cache/uuid.component.wasm",
            "--load",
            "/cache/json1.component.wasm",
            "--keep-open",
            "--bundle-grant-spawn-build",
            ".bundle",
            "save",
            "myset",
            "--no-build",
        ]);
        let r = parse_argv(&a);
        assert_eq!(r.db_path, "data.sqlite");
        assert_eq!(
            r.preload,
            vec![
                "/cache/uuid.component.wasm".to_string(),
                "/cache/json1.component.wasm".to_string()
            ]
        );
        assert!(r.keep_open);
        assert!(r.bundle_grant_spawn_build);
        assert!(r.dot_seen);
        assert_eq!(
            r.dot_args,
            vec![
                ".bundle".to_string(),
                "save".to_string(),
                "myset".to_string(),
                "--no-build".to_string(),
            ]
        );
    }
}
