//! Embed path for text-nlp. All scalar bodies live at crate level
//! (text_diff, markdown_to_html, stem_porter, soundex, metaphone,
//! ...) so this module is a thin dispatch table over them.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_int;
use sqlite_embed::{register_scalars, ScalarSpec, SqlValueOwned};

const FID_DIFF: u64 = 1;
const FID_MD: u64 = 2;
const FID_STEM: u64 = 3;
const FID_SOUNDEX: u64 = 4;
const FID_METAPHONE: u64 = 5;
const FID_DIFF_ADDED: u64 = 6;
const FID_DIFF_REMOVED: u64 = 7;
const FID_DIFF_SUMMARY: u64 = 8;
const FID_SIMILARITY: u64 = 9;
const FID_MD_TEXT: u64 = 10;
const FID_MD_LINKS: u64 = 11;
const FID_MD_HEADINGS: u64 = 12;
const FID_HTML_TO_MD: u64 = 13;

fn arg_text(args: &[SqlValueOwned], i: usize, fname: &str) -> Result<String, String> {
    match args.get(i) {
        Some(SqlValueOwned::Text(s)) => Ok(s.clone()),
        _ => Err(format!("{fname}: TEXT arg at {i}")),
    }
}

pub fn call_scalar(func_id: u64, args: Vec<SqlValueOwned>) -> Result<SqlValueOwned, String> {
    match func_id {
        FID_DIFF => {
            let a = arg_text(&args, 0, "text_diff")?;
            let b = arg_text(&args, 1, "text_diff")?;
            Ok(SqlValueOwned::Text(crate::text_diff(&a, &b)))
        }
        FID_MD => {
            let m = arg_text(&args, 0, "markdown_to_html")?;
            Ok(SqlValueOwned::Text(crate::markdown_to_html(&m)))
        }
        FID_STEM => {
            let w = arg_text(&args, 0, "stem_porter")?;
            Ok(SqlValueOwned::Text(crate::stem_porter(&w)))
        }
        FID_SOUNDEX => {
            let w = arg_text(&args, 0, "soundex")?;
            Ok(SqlValueOwned::Text(crate::soundex(&w)))
        }
        FID_METAPHONE => {
            let w = arg_text(&args, 0, "metaphone")?;
            Ok(SqlValueOwned::Text(crate::metaphone(&w)))
        }
        FID_DIFF_ADDED => {
            let a = arg_text(&args, 0, "text_diff_added")?;
            let b = arg_text(&args, 1, "text_diff_added")?;
            Ok(SqlValueOwned::Text(crate::text_diff_added(&a, &b)))
        }
        FID_DIFF_REMOVED => {
            let a = arg_text(&args, 0, "text_diff_removed")?;
            let b = arg_text(&args, 1, "text_diff_removed")?;
            Ok(SqlValueOwned::Text(crate::text_diff_removed(&a, &b)))
        }
        FID_DIFF_SUMMARY => {
            let a = arg_text(&args, 0, "text_diff_summary")?;
            let b = arg_text(&args, 1, "text_diff_summary")?;
            Ok(SqlValueOwned::Text(crate::text_diff_summary(&a, &b)))
        }
        FID_SIMILARITY => {
            let a = arg_text(&args, 0, "text_similarity")?;
            let b = arg_text(&args, 1, "text_similarity")?;
            Ok(SqlValueOwned::Real(crate::text_similarity(&a, &b)))
        }
        FID_MD_TEXT => {
            let m = arg_text(&args, 0, "markdown_to_text")?;
            Ok(SqlValueOwned::Text(crate::markdown_to_text(&m)))
        }
        FID_MD_LINKS => {
            let m = arg_text(&args, 0, "markdown_extract_links")?;
            Ok(SqlValueOwned::Text(crate::markdown_extract_links(&m)))
        }
        FID_MD_HEADINGS => {
            let m = arg_text(&args, 0, "markdown_extract_headings")?;
            Ok(SqlValueOwned::Text(crate::markdown_extract_headings(&m)))
        }
        FID_HTML_TO_MD => {
            let h = arg_text(&args, 0, "html_to_markdown")?;
            Ok(SqlValueOwned::Text(crate::html_to_markdown(&h)))
        }
        other => Err(format!("text-nlp: unknown func id {other}")),
    }
}

const SCALARS: &[ScalarSpec] = &[
    ScalarSpec {
        func_id: FID_DIFF,
        name: b"text_diff\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_MD,
        name: b"markdown_to_html\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_STEM,
        name: b"stem_porter\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_SOUNDEX,
        name: b"soundex\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_METAPHONE,
        name: b"metaphone\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_DIFF_ADDED,
        name: b"text_diff_added\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_DIFF_REMOVED,
        name: b"text_diff_removed\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_DIFF_SUMMARY,
        name: b"text_diff_summary\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_SIMILARITY,
        name: b"text_similarity\0",
        num_args: 2,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_MD_TEXT,
        name: b"markdown_to_text\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_MD_LINKS,
        name: b"markdown_extract_links\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_MD_HEADINGS,
        name: b"markdown_extract_headings\0",
        num_args: 1,
        deterministic: true,
    },
    ScalarSpec {
        func_id: FID_HTML_TO_MD,
        name: b"html_to_markdown\0",
        num_args: 1,
        deterministic: true,
    },
];

pub unsafe fn register_into(db: *mut libsqlite3_sys::sqlite3) -> c_int {
    register_scalars(db, SCALARS, call_scalar)
}
