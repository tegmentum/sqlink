//! Pure-fn argv parsers for bundle-cli's dot-commands.
//!
//! Extracted from `extensions/bundle-cli/src/lib.rs` so the fuzz
//! harness can exercise the same code path the wasm extension's
//! `.bundle save / build / gc / alias / delete` handlers run.
//! The extension's handlers wrap each parser with side-effects
//! (spi.execute, build orchestration, list-loaded-extensions);
//! the pure parsing step lives here.
//!
//! Pattern matches the parse_duration / parse_load_args
//! extraction: bundle-cli imports from sqlink-parsers + the fuzz
//! crate imports from sqlink-parsers, source-of-truth in one place.

#![cfg(feature = "std")]

extern crate std;

use alloc::format;
use alloc::string::{String, ToString};

/// Parsed shape of `.bundle save [--no-build] NAME`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveArgs {
    pub name: String,
    pub no_build: bool,
}

/// Parse argv for `.bundle save`. Accepts `--no-build` flag and one
/// NAME positional. `--name` is a noop swallow (legacy alias for
/// the next positional). Unknown `--flag` rejected. Multiple
/// positionals rejected.
pub fn parse_save(args: &[&str]) -> Result<SaveArgs, String> {
    let mut name: Option<String> = None;
    let mut no_build = false;
    for a in args {
        match *a {
            "--no-build" => no_build = true,
            "--name" => {} // legacy alias for next positional; swallow
            other if other.starts_with("--") => {
                return Err(format!(".bundle save: unknown flag {other:?}"));
            }
            other => {
                if name.is_some() {
                    return Err(".bundle save: only one NAME positional accepted".to_string());
                }
                name = Some(other.to_string());
            }
        }
    }
    let name = name
        .ok_or_else(|| ".bundle save: NAME required (usage: .bundle save NAME [--no-build])".to_string())?;
    Ok(SaveArgs { name, no_build })
}

/// Parsed shape of `.bundle build NAME [--target TRIPLE]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildArgs {
    pub name: String,
    pub target: Option<String>,
}

/// Parse argv for `.bundle build`. Accepts NAME positional and
/// `--target TRIPLE` flag pair. Triple value must follow `--target`
/// or come as the next argv slot.
pub fn parse_build(args: &[&str]) -> Result<BuildArgs, String> {
    let mut name: Option<String> = None;
    let mut target: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let a = args[i];
        match a {
            "--target" => {
                i += 1;
                if i >= args.len() {
                    return Err(".bundle build: --target requires a triple value".to_string());
                }
                target = Some(args[i].to_string());
            }
            other if other.starts_with("--") => {
                return Err(format!(".bundle build: unknown flag {other:?}"));
            }
            other => {
                if name.is_some() {
                    return Err(".bundle build: only one NAME positional accepted".to_string());
                }
                name = Some(other.to_string());
            }
        }
        i += 1;
    }
    let name = name
        .ok_or_else(|| ".bundle build: NAME required".to_string())?;
    Ok(BuildArgs { name, target })
}

/// Parsed shape of `.bundle gc [--keep N | --older-than DURATION]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcArgs {
    pub keep: Option<u32>,
    pub older_than: Option<String>,
}

/// Parse argv for `.bundle gc`. Mutually-exclusive `--keep N` /
/// `--older-than DURATION`; neither = full noop (caller decides).
pub fn parse_gc(args: &[&str]) -> Result<GcArgs, String> {
    let mut keep: Option<u32> = None;
    let mut older_than: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let a = args[i];
        match a {
            "--keep" => {
                i += 1;
                if i >= args.len() {
                    return Err(".bundle gc: --keep requires N value".to_string());
                }
                keep = Some(
                    args[i]
                        .parse::<u32>()
                        .map_err(|e| format!(".bundle gc: --keep value not u32: {e}"))?,
                );
            }
            "--older-than" => {
                i += 1;
                if i >= args.len() {
                    return Err(".bundle gc: --older-than requires DURATION value".to_string());
                }
                older_than = Some(args[i].to_string());
            }
            other if other.starts_with("--") => {
                return Err(format!(".bundle gc: unknown flag {other:?}"));
            }
            other => {
                return Err(format!(
                    ".bundle gc: unexpected positional {other:?} (expected --keep / --older-than)"
                ));
            }
        }
        i += 1;
    }
    if keep.is_some() && older_than.is_some() {
        return Err(".bundle gc: --keep + --older-than are mutually exclusive".to_string());
    }
    Ok(GcArgs { keep, older_than })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_happy_path() {
        assert_eq!(
            parse_save(&["myset"]),
            Ok(SaveArgs { name: "myset".into(), no_build: false })
        );
    }

    #[test]
    fn save_no_build_flag() {
        assert_eq!(
            parse_save(&["--no-build", "myset"]),
            Ok(SaveArgs { name: "myset".into(), no_build: true })
        );
        assert_eq!(
            parse_save(&["myset", "--no-build"]),
            Ok(SaveArgs { name: "myset".into(), no_build: true })
        );
    }

    #[test]
    fn save_requires_name() {
        assert!(parse_save(&[]).is_err());
        assert!(parse_save(&["--no-build"]).is_err());
    }

    #[test]
    fn save_rejects_multi_positional() {
        assert!(parse_save(&["a", "b"]).is_err());
    }

    #[test]
    fn save_rejects_unknown_flag() {
        assert!(parse_save(&["--frobnicate", "myset"]).is_err());
    }

    #[test]
    fn save_swallows_name_alias() {
        assert_eq!(
            parse_save(&["--name", "myset"]),
            Ok(SaveArgs { name: "myset".into(), no_build: false })
        );
    }

    #[test]
    fn build_happy_path() {
        assert_eq!(
            parse_build(&["myset"]),
            Ok(BuildArgs { name: "myset".into(), target: None })
        );
        assert_eq!(
            parse_build(&["myset", "--target", "aarch64-apple-darwin"]),
            Ok(BuildArgs {
                name: "myset".into(),
                target: Some("aarch64-apple-darwin".into())
            })
        );
    }

    #[test]
    fn build_target_value_required() {
        assert!(parse_build(&["myset", "--target"]).is_err());
    }

    #[test]
    fn gc_keep_value() {
        assert_eq!(
            parse_gc(&["--keep", "10"]),
            Ok(GcArgs { keep: Some(10), older_than: None })
        );
    }

    #[test]
    fn gc_older_than_value() {
        assert_eq!(
            parse_gc(&["--older-than", "30d"]),
            Ok(GcArgs { keep: None, older_than: Some("30d".into()) })
        );
    }

    #[test]
    fn gc_keep_and_older_than_mutually_exclusive() {
        assert!(parse_gc(&["--keep", "10", "--older-than", "30d"]).is_err());
    }

    #[test]
    fn gc_rejects_positional() {
        assert!(parse_gc(&["myset"]).is_err());
    }

    #[test]
    fn gc_keep_value_not_u32() {
        assert!(parse_gc(&["--keep", "notanum"]).is_err());
        assert!(parse_gc(&["--keep", "-1"]).is_err());
    }
}
