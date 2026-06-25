#![no_main]

// PLAN-followups.md P3 round-2 fuzz target.
//
// Exercises bundle-cli's dot-command argv parsers extracted to
// sqlink-parsers::bundle_cli_argv. Three sub-cmds covered in
// rotation per arbitrary-driven dispatch:
//
//   parse_save   .bundle save [--no-build] NAME
//   parse_build  .bundle build NAME [--target TRIPLE]
//   parse_gc     .bundle gc [--keep N | --older-than DURATION]
//
// Harness invariant: none of these parsers panic on arbitrary
// argv. Anything malformed must surface as Err, never abort.

use libfuzzer_sys::{arbitrary::Arbitrary, fuzz_target};

use sqlink_parsers::bundle_cli_argv::{parse_build, parse_gc, parse_save};

#[derive(Debug, Arbitrary)]
struct Input {
    which: u8,
    args: Vec<String>,
}

fuzz_target!(|data: Input| {
    // Borrow checker dance: parse_* takes &[&str].
    let refs: Vec<&str> = data.args.iter().map(String::as_str).collect();
    match data.which % 3 {
        0 => {
            let _ = parse_save(&refs);
        }
        1 => {
            let _ = parse_build(&refs);
        }
        _ => {
            let _ = parse_gc(&refs);
        }
    }
});
