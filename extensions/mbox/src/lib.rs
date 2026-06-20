//! mbox + maildir email format reader scalars.
//!
//! mbox splitter is hand-rolled (mbox-reader 0.2 only takes a file
//! path via memmap, so it's not usable from a wasm32-wasip2 sandbox
//! which receives bytes via SQL TEXT). Per-message parsing is done
//! by mailparse 0.15.
//!
//! See PLAN-more-extensions plan for the spec; the surface is:
//!   mbox_message_count(text)        -> integer
//!   mbox_subjects(text)             -> text  (JSON array)
//!   mbox_message_at(text, idx)      -> text  (raw RFC 822)
//!   mbox_from_at(text, idx)         -> text
//!   mbox_subject_at(text, idx)      -> text
//!   mbox_date_at(text, idx)         -> text  (ISO 8601 UTC)
//!   mbox_body_at(text, idx)         -> text  (decoded body)
//!   mbox_version()                  -> text

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

    use mailparse::{dateparse, parse_mail, MailHeaderMap};

    // ---- Function IDs (stable u64; ABI break to renumber). ----
    const FID_COUNT: u64 = 1;
    const FID_SUBJECTS: u64 = 2;
    const FID_MESSAGE_AT: u64 = 3;
    const FID_FROM_AT: u64 = 4;
    const FID_SUBJECT_AT: u64 = 5;
    const FID_DATE_AT: u64 = 6;
    const FID_BODY_AT: u64 = 7;
    const FID_VERSION: u64 = 8;

    struct Ext;

    // ---- Arg helpers ----
    fn arg_int(args: &[SqlValue], i: usize, fname: &str) -> Result<i64, String> {
        match args.get(i) {
            Some(SqlValue::Integer(n)) => Ok(*n),
            _ => Err(format!("{fname}: INTEGER arg at {i}")),
        }
    }

    // ---- mbox splitter ----
    //
    // An mbox-O file is one-or-more messages, each prefixed by a
    // "From " line at column 0 (any leading whitespace disqualifies
    // it as a delimiter). Subsequent messages are separated by a
    // blank line followed by another "From " line.
    //
    // We accept two practical liberalizations vs. strict RFC 4155:
    //   1. The very first "From " may sit at offset 0 with no
    //      preceding blank line.
    //   2. Trailing whitespace after a message body is fine; the
    //      splitter eats the blank separator line.
    //
    // mbox-rd content escaping (`>From ` -> `From ` on read) is
    // applied only to extracted bodies via `unescape_mbox_rd` on
    // the body slice that the caller asks for.

    /// Split the raw mbox text into the byte offsets of each
    /// message (start of "From " line through last byte before the
    /// next "From " line). Empty for malformed input.
    fn split_messages(raw: &str) -> Vec<(usize, usize)> {
        let bytes = raw.as_bytes();
        let mut starts: Vec<usize> = Vec::new();

        // Find every "From " at column 0 (offset 0, or after \n).
        // A bare "From " at offset 0 (no preceding \n) still
        // qualifies.
        let mut i = 0;
        while i < bytes.len() {
            let at_col0 = i == 0 || bytes[i - 1] == b'\n';
            if at_col0
                && i + 5 <= bytes.len()
                && &bytes[i..i + 5] == b"From "
            {
                starts.push(i);
                // Skip past the From_ line so we don't re-match
                // inside it. Next iteration of the outer scan
                // re-validates column-0 anchoring.
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            i += 1;
        }

        let mut out = Vec::with_capacity(starts.len());
        for (idx, &s) in starts.iter().enumerate() {
            let end = if idx + 1 < starts.len() {
                starts[idx + 1]
            } else {
                bytes.len()
            };
            out.push((s, end));
        }
        out
    }

    /// Strip the "From " envelope line and return (envelope_line,
    /// rest_of_message). rest_of_message is the RFC 822 portion
    /// (headers + blank + body) of the message.
    fn split_envelope(slice: &str) -> (&str, &str) {
        match slice.find('\n') {
            Some(pos) => (&slice[..pos], &slice[pos + 1..]),
            None => (slice, ""),
        }
    }

    /// mbox-rd unescaping: lines beginning with one-or-more `>`
    /// followed by "From " lose one leading `>`. Applied per-line.
    fn unescape_mbox_rd(body: &str) -> String {
        let mut out = String::with_capacity(body.len());
        for (i, line) in body.split('\n').enumerate() {
            if i > 0 {
                out.push('\n');
            }
            // Count leading '>' then check for "From ".
            let bs = line.as_bytes();
            let mut k = 0;
            while k < bs.len() && bs[k] == b'>' {
                k += 1;
            }
            if k > 0 && bs.len() >= k + 5 && &bs[k..k + 5] == b"From " {
                // Drop one '>'.
                out.push_str(&line[1..]);
            } else {
                out.push_str(line);
            }
        }
        out
    }

    /// Extract the message body (post-headers) from the RFC 822
    /// portion. The body is the bytes after the first blank line.
    fn header_body_split(rfc822: &str) -> (&str, &str) {
        // RFC 822 separator is a blank line (CRLF CRLF or LF LF).
        if let Some(pos) = rfc822.find("\r\n\r\n") {
            (&rfc822[..pos], &rfc822[pos + 4..])
        } else if let Some(pos) = rfc822.find("\n\n") {
            (&rfc822[..pos], &rfc822[pos + 2..])
        } else {
            (rfc822, "")
        }
    }

    /// Convert a Unix epoch timestamp to ISO 8601 UTC ("Z" suffix).
    fn epoch_to_iso(secs: i64) -> Option<String> {
        let odt = time::OffsetDateTime::from_unix_timestamp(secs).ok()?;
        // RFC 3339 / ISO 8601, e.g. "2016-10-02T14:06:22Z".
        odt.format(&time::format_description::well_known::Rfc3339)
            .ok()
    }

    /// Fetch the headers of a message by index. Returns the raw
    /// RFC 822 slice (post-envelope-line) on success.
    fn message_rfc822<'a>(raw: &'a str, idx: i64) -> Option<&'a str> {
        if idx < 0 {
            return None;
        }
        let ranges = split_messages(raw);
        let i = idx as usize;
        if i >= ranges.len() {
            return None;
        }
        let (s, e) = ranges[i];
        let slice = &raw[s..e];
        let (_env, rest) = split_envelope(slice);
        Some(rest)
    }

    /// Lookup a single header value by name.
    fn header_value(rfc822: &str, name: &str) -> Option<String> {
        let parsed = parse_mail(rfc822.as_bytes()).ok()?;
        parsed
            .headers
            .get_first_value(name)
            .filter(|s| !s.is_empty())
    }

    impl MetadataGuest for Ext {
        fn describe() -> Manifest {
            let det = FunctionFlags::DETERMINISTIC;
            let s = |id, name: &str, n: i32| ScalarFunctionSpec {
                id,
                name: name.into(),
                num_args: n,
                func_flags: det,
            };
            Manifest {
                name: "mbox".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_COUNT, "mbox_message_count", 1),
                    s(FID_SUBJECTS, "mbox_subjects", 1),
                    s(FID_MESSAGE_AT, "mbox_message_at", 2),
                    s(FID_FROM_AT, "mbox_from_at", 2),
                    s(FID_SUBJECT_AT, "mbox_subject_at", 2),
                    s(FID_DATE_AT, "mbox_date_at", 2),
                    s(FID_BODY_AT, "mbox_body_at", 2),
                    s(FID_VERSION, "mbox_version", 0),
                ],
                aggregate_functions: alloc::vec![],
                collations: alloc::vec![],
                vtabs: alloc::vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                declared_capabilities: alloc::vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            // Version is the no-arg outlier.
            if func_id == FID_VERSION {
                return Ok(SqlValue::Text(env!("CARGO_PKG_VERSION").to_string()));
            }

            // All other scalars require a TEXT mbox arg at slot 0.
            // NULL-in / non-text -> NULL-out (per spec).
            let raw = match args.first() {
                Some(SqlValue::Text(s)) => s.clone(),
                Some(SqlValue::Null) | None => return Ok(SqlValue::Null),
                _ => return Ok(SqlValue::Null),
            };

            match func_id {
                FID_COUNT => {
                    let n = split_messages(&raw).len() as i64;
                    Ok(SqlValue::Integer(n))
                }
                FID_SUBJECTS => {
                    let ranges = split_messages(&raw);
                    let mut subs: Vec<String> = Vec::with_capacity(ranges.len());
                    for (s, e) in ranges {
                        let slice = &raw[s..e];
                        let (_env, rest) = split_envelope(slice);
                        let sub = header_value(rest, "Subject").unwrap_or_default();
                        subs.push(sub);
                    }
                    let json = serde_json::to_string(&subs)
                        .unwrap_or_else(|_| "[]".to_string());
                    Ok(SqlValue::Text(json))
                }
                FID_MESSAGE_AT => {
                    let idx = arg_int(&args, 1, "mbox_message_at")?;
                    Ok(message_rfc822(&raw, idx)
                        .map(|s| SqlValue::Text(s.to_string()))
                        .unwrap_or(SqlValue::Null))
                }
                FID_FROM_AT => {
                    let idx = arg_int(&args, 1, "mbox_from_at")?;
                    Ok(message_rfc822(&raw, idx)
                        .and_then(|rfc| header_value(rfc, "From"))
                        .map(SqlValue::Text)
                        .unwrap_or(SqlValue::Null))
                }
                FID_SUBJECT_AT => {
                    let idx = arg_int(&args, 1, "mbox_subject_at")?;
                    Ok(message_rfc822(&raw, idx)
                        .and_then(|rfc| header_value(rfc, "Subject"))
                        .map(SqlValue::Text)
                        .unwrap_or(SqlValue::Null))
                }
                FID_DATE_AT => {
                    let idx = arg_int(&args, 1, "mbox_date_at")?;
                    let Some(rfc) = message_rfc822(&raw, idx) else {
                        return Ok(SqlValue::Null);
                    };
                    let Some(date_raw) = header_value(rfc, "Date") else {
                        return Ok(SqlValue::Null);
                    };
                    // dateparse returns Unix epoch on success;
                    // fall back to the raw header text on failure
                    // so the caller still gets something.
                    match dateparse(&date_raw) {
                        Ok(epoch) => match epoch_to_iso(epoch) {
                            Some(iso) => Ok(SqlValue::Text(iso)),
                            None => Ok(SqlValue::Text(date_raw)),
                        },
                        Err(_) => Ok(SqlValue::Text(date_raw)),
                    }
                }
                FID_BODY_AT => {
                    let idx = arg_int(&args, 1, "mbox_body_at")?;
                    let Some(rfc) = message_rfc822(&raw, idx) else {
                        return Ok(SqlValue::Null);
                    };
                    // Prefer mailparse's decoded body (handles
                    // quoted-printable, base64, charset). Fall
                    // back to the raw split on parse failure.
                    let body_decoded = match parse_mail(rfc.as_bytes()) {
                        Ok(m) => m.get_body().ok(),
                        Err(_) => None,
                    };
                    let body = match body_decoded {
                        Some(b) => b,
                        None => {
                            let (_h, b) = header_body_split(rfc);
                            b.to_string()
                        }
                    };
                    // mbox-rd unescape on the body, best-effort.
                    Ok(SqlValue::Text(unescape_mbox_rd(&body)))
                }
                other => Err(format!("mbox: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
