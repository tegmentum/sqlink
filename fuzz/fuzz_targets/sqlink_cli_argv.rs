#![no_main]

// PLAN-followups.md P3 round-2 fuzz target.
//
// Exercises sqlink-cli-argv's pure-fn argv parser. Asserts the
// parser never panics on arbitrary argv  it should always
// return a ParsedArgs (or an error result, depending on the
// public API).
//
// sqlink-cli-argv was extracted from cli/ to its own native
// workspace crate in a prior round, so no extraction step is
// needed here.

use libfuzzer_sys::fuzz_target;

use sqlink_cli_argv::parse_argv;

fuzz_target!(|argv: Vec<String>| {
    let _ = parse_argv(&argv);
});
