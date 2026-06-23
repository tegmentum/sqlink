//! ID3 tag parsing from MP3 (and any other id3-bearing) blobs via the
//! `id3` 1.x pure-rust crate. Covers ID3v1, ID3v2.3, ID3v2.4.
//!
//! Function surface (PLAN-more-extensions-4.md  2):
//!
//!   id3_title(blob)         -> TEXT
//!   id3_artist(blob)        -> TEXT
//!   id3_album(blob)         -> TEXT
//!   id3_year(blob)          -> INTEGER
//!   id3_genre(blob)         -> TEXT
//!   id3_track(blob)         -> INTEGER
//!   id3_disc(blob)          -> INTEGER
//!   id3_comment(blob)       -> TEXT       (first comment)
//!   id3_album_artist(blob)  -> TEXT
//!   id3_composer(blob)      -> TEXT
//!   id3_duration_ms(blob)   -> INTEGER    (TLEN frame; MPEG decode not implemented)
//!   id3_version(blob)       -> TEXT       ("ID3v2.4" / "ID3v2.3" / "ID3v2.2" / "ID3v1")
//!   id3_all(blob)           -> TEXT       (JSON of every text frame + summary fields)
//!   id3_meta_version()      -> TEXT       (extension + crate version banner)
//!
//! Per the plan, the ID3v2.3 vs 2.4 frame naming differences (TIT2 vs
//! TT2, etc.) are hidden behind the named accessors  the `id3` crate
//! itself does this translation in its TagLike trait via `title()`,
//! `artist()`, etc.
//!
//! NULL contract: every accessor returns SQL NULL on
//!   - SqlValue::Null input
//!   - non-BLOB / non-TEXT input
//!   - blobs that contain neither an ID3v2 prefix nor an ID3v1 suffix
//!   - the requested field being absent from a successfully-parsed tag
//!
//! Errors are never surfaced to SQL  mirrors the `exif` / `image-meta`
//! convention. Each call re-parses the blob fresh; no shared state.
//!
//! Caveat: id3_duration_ms reflects the TLEN ID3v2 frame (if present)
//! rather than decoding MPEG frame headers. Full MPEG decode is
//! intentionally out of scope per the plan  the id3 crate's surface
//! here is metadata-only.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::fmt::Write as _;
    use std::io::Cursor;

    use id3::{Tag, TagLike};

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

    // ---- Function IDs (stable; changing breaks the id->name map). ----
    const FID_TITLE: u64 = 1;
    const FID_ARTIST: u64 = 2;
    const FID_ALBUM: u64 = 3;
    const FID_YEAR: u64 = 4;
    const FID_GENRE: u64 = 5;
    const FID_TRACK: u64 = 6;
    const FID_DISC: u64 = 7;
    const FID_COMMENT: u64 = 8;
    const FID_ALBUM_ARTIST: u64 = 9;
    const FID_COMPOSER: u64 = 10;
    const FID_DURATION_MS: u64 = 11;
    const FID_VERSION: u64 = 12;
    const FID_ALL: u64 = 13;
    const FID_META_VERSION: u64 = 14;

    struct Ext;

    // ---- Input coercion ----
    //
    // BLOB / TEXT are the only acceptable arg-0 types. TEXT is treated
    // as raw UTF-8 byte view; callers with hex columns can skip a
    // CAST  X'..'.
    fn opt_bytes(args: &[SqlValue]) -> Option<Vec<u8>> {
        match args.first() {
            Some(SqlValue::Blob(b)) => Some(b.clone()),
            Some(SqlValue::Text(s)) => Some(s.as_bytes().to_vec()),
            _ => None,
        }
    }

    /// Parsed-tag union  ID3v2 and ID3v1 land in different structs in
    /// the `id3` crate. `read_from2` handles the v2 case (any of 2.2 /
    /// 2.3 / 2.4); v1 is a separate 128-byte trailer-based format.
    enum AnyTag {
        V2(Tag),
        V1(id3::v1::Tag),
    }

    /// Try ID3v2 first, then fall back to ID3v1 trailer scan. Both
    /// readers consume a `Read + Seek` so we wrap the byte slice in a
    /// `Cursor`. Returns `None` on every failure mode  no tag, bad
    /// frame data, truncated header  preserving the NULL contract.
    fn parse(bytes: &[u8]) -> Option<AnyTag> {
        // v2: prefix-located. Tag::read_from2 walks the synchsafe
        // header at offset 0 and bails on missing magic.
        if let Ok(tag) = Tag::read_from2(Cursor::new(bytes)) {
            return Some(AnyTag::V2(tag));
        }
        // v1: trailer-located. The reader seeks to file_len-128, checks
        // for the "TAG" magic, and parses the fixed-width fields. Fails
        // on files smaller than 128 bytes or missing the magic.
        if let Ok(v1) = id3::v1::Tag::read_from(Cursor::new(bytes)) {
            return Some(AnyTag::V1(v1));
        }
        None
    }

    /// Strip trailing NUL + whitespace, drop the value if empty post-trim.
    /// ID3v1 fields are NUL-padded fixed-width strings; the `id3` crate's
    /// v1 parser stops at the first NUL but downstream callers still
    /// occasionally see trailing whitespace from extension fields. The
    /// empty-string  None collapse keeps "(blank artist)" off SQL.
    fn clean(s: &str) -> Option<String> {
        let t = s.trim_end_matches('\0').trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    }

    /// Title across all versions. v2 uses TIT2 (handled by id3::title);
    /// v1 has it inline at offset 3..33.
    fn get_title(tag: &AnyTag) -> Option<String> {
        match tag {
            AnyTag::V2(t) => t.title().and_then(clean),
            AnyTag::V1(t) => clean(&t.title),
        }
    }
    fn get_artist(tag: &AnyTag) -> Option<String> {
        match tag {
            AnyTag::V2(t) => t.artist().and_then(clean),
            AnyTag::V1(t) => clean(&t.artist),
        }
    }
    fn get_album(tag: &AnyTag) -> Option<String> {
        match tag {
            AnyTag::V2(t) => t.album().and_then(clean),
            AnyTag::V1(t) => clean(&t.album),
        }
    }
    /// Year is INTEGER in our surface. v2 returns it as i32 already;
    /// v1 stores it as a 4-char ASCII field that we parse as i64.
    /// Unparseable v1 year  None.
    fn get_year(tag: &AnyTag) -> Option<i64> {
        match tag {
            AnyTag::V2(t) => t.year().map(|y| y as i64),
            AnyTag::V1(t) => t.year.trim_end_matches('\0').trim().parse::<i64>().ok(),
        }
    }
    /// Genre  v2: TCON resolved by id3::genre. v1: the genre_id byte
    /// indexes into the standardized + Winamp-extended genre list; the
    /// `id3` crate exposes this via v1::Tag::genre().
    fn get_genre(tag: &AnyTag) -> Option<String> {
        match tag {
            AnyTag::V2(t) => t.genre().and_then(clean),
            AnyTag::V1(t) => t.genre().and_then(clean),
        }
    }
    /// Track number. v2 splits "n/m" into (track, total_tracks); we
    /// return the leading number only. v1 stores it as a single byte
    /// (Option<u8>) when the v1.1 marker is present.
    fn get_track(tag: &AnyTag) -> Option<i64> {
        match tag {
            AnyTag::V2(t) => t.track().map(|n| n as i64),
            AnyTag::V1(t) => t.track.map(|n| n as i64),
        }
    }
    /// Disc number. v2-only (v1 has no concept of multi-disc).
    fn get_disc(tag: &AnyTag) -> Option<i64> {
        match tag {
            AnyTag::V2(t) => t.disc().map(|n| n as i64),
            AnyTag::V1(_) => None,
        }
    }
    /// First comment. v2 may have multiple COMM frames keyed by
    /// (lang, description); we return the first one's text. v1's
    /// comment is the inline 30-byte field.
    fn get_comment(tag: &AnyTag) -> Option<String> {
        match tag {
            AnyTag::V2(t) => t.comments().next().map(|c| c.text.clone()).and_then(|s| clean(&s)),
            AnyTag::V1(t) => clean(&t.comment),
        }
    }
    fn get_album_artist(tag: &AnyTag) -> Option<String> {
        match tag {
            AnyTag::V2(t) => t.album_artist().and_then(clean),
            AnyTag::V1(_) => None,
        }
    }
    /// Composer (TCOM). v2-only.
    fn get_composer(tag: &AnyTag) -> Option<String> {
        match tag {
            AnyTag::V2(t) => t.text_for_frame_id("TCOM").and_then(clean),
            AnyTag::V1(_) => None,
        }
    }
    /// Duration in milliseconds. Reads the TLEN frame (v2.3+) when
    /// present  not an MPEG-frame decode, just the embedded tag.
    /// id3::duration() returns u32 already (Option). v1 has no TLEN.
    fn get_duration_ms(tag: &AnyTag) -> Option<i64> {
        match tag {
            AnyTag::V2(t) => t.duration().map(|d| d as i64),
            AnyTag::V1(_) => None,
        }
    }
    /// Human-readable version banner. v2 uses the Tag's stored version
    /// enum; v1 always reports "ID3v1" (we don't distinguish v1 vs v1.1
    /// at the surface  the spec calls both "ID3v1").
    fn get_version(tag: &AnyTag) -> String {
        match tag {
            AnyTag::V2(t) => format!("{}", t.version()),
            AnyTag::V1(_) => "ID3v1".to_string(),
        }
    }

    /// JSON-escape into the given buffer. Used by `all_json` for both
    /// keys and values  ID3 text frames can carry arbitrary UTF-8.
    fn push_json_string(out: &mut String, s: &str) {
        out.push('"');
        for c in s.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => out.push(' '),
                c => out.push(c),
            }
        }
        out.push('"');
    }

    /// Build a JSON object dump of every text frame plus the well-known
    /// summary fields. Frame IDs are emitted verbatim (TIT2, TPE1, ...)
    /// so callers can map back to the canonical ID3 frame names. The
    /// "_version" key carries the version banner string. v1-only tags
    /// fall back to the named fields  v1 has no frame ID concept.
    fn all_json(tag: &AnyTag) -> String {
        let mut out = String::from("{");
        match tag {
            AnyTag::V2(t) => {
                let _ = write!(&mut out, "\"_version\":\"{}\"", t.version());
                // Emit every text frame's first text value, keyed by
                // frame ID. id3::frame::Content::text() returns the
                // first value of a multi-value text frame.
                for f in t.frames() {
                    if let Some(txt) = f.content().text() {
                        out.push(',');
                        push_json_string(&mut out, f.id());
                        out.push(':');
                        push_json_string(&mut out, txt);
                    } else if let Some(c) = f.content().comment() {
                        // Comments are (lang, desc, text); collapse to text.
                        out.push(',');
                        push_json_string(&mut out, f.id());
                        out.push(':');
                        push_json_string(&mut out, &c.text);
                    }
                    // Other frame kinds (picture, popularimeter, ...)
                    // are intentionally skipped  binary payloads don't
                    // belong in a metadata JSON dump.
                }
            }
            AnyTag::V1(t) => {
                out.push_str("\"_version\":\"ID3v1\"");
                let mut push = |k: &str, v: &str| {
                    let trimmed = v.trim_end_matches('\0').trim();
                    if !trimmed.is_empty() {
                        out.push(',');
                        push_json_string(&mut out, k);
                        out.push(':');
                        push_json_string(&mut out, trimmed);
                    }
                };
                push("title", &t.title);
                push("artist", &t.artist);
                push("album", &t.album);
                push("year", &t.year);
                push("comment", &t.comment);
                if let Some(tn) = t.track {
                    let _ = write!(&mut out, ",\"track\":{}", tn);
                }
                if let Some(g) = t.genre() {
                    out.push(',');
                    push_json_string(&mut out, "genre");
                    out.push(':');
                    push_json_string(&mut out, g);
                }
            }
        }
        out.push('}');
        out
    }

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
                name: "id3".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_TITLE, "id3_title", 1, det),
                    s(FID_ARTIST, "id3_artist", 1, det),
                    s(FID_ALBUM, "id3_album", 1, det),
                    s(FID_YEAR, "id3_year", 1, det),
                    s(FID_GENRE, "id3_genre", 1, det),
                    s(FID_TRACK, "id3_track", 1, det),
                    s(FID_DISC, "id3_disc", 1, det),
                    s(FID_COMMENT, "id3_comment", 1, det),
                    s(FID_ALBUM_ARTIST, "id3_album_artist", 1, det),
                    s(FID_COMPOSER, "id3_composer", 1, det),
                    s(FID_DURATION_MS, "id3_duration_ms", 1, det),
                    s(FID_VERSION, "id3_version", 1, det),
                    s(FID_ALL, "id3_all", 1, det),
                    s(FID_META_VERSION, "id3_meta_version", 0, det),
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
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            if func_id == FID_META_VERSION {
                return Ok(SqlValue::Text(format!(
                    "id3 1.17; extension {}",
                    env!("CARGO_PKG_VERSION")
                )));
            }

            let Some(bytes) = opt_bytes(&args) else {
                return Ok(SqlValue::Null);
            };
            let Some(tag) = parse(&bytes) else {
                return Ok(SqlValue::Null);
            };

            match func_id {
                FID_TITLE => match get_title(&tag) {
                    Some(s) => Ok(SqlValue::Text(s)),
                    None => Ok(SqlValue::Null),
                },
                FID_ARTIST => match get_artist(&tag) {
                    Some(s) => Ok(SqlValue::Text(s)),
                    None => Ok(SqlValue::Null),
                },
                FID_ALBUM => match get_album(&tag) {
                    Some(s) => Ok(SqlValue::Text(s)),
                    None => Ok(SqlValue::Null),
                },
                FID_YEAR => match get_year(&tag) {
                    Some(y) => Ok(SqlValue::Integer(y)),
                    None => Ok(SqlValue::Null),
                },
                FID_GENRE => match get_genre(&tag) {
                    Some(s) => Ok(SqlValue::Text(s)),
                    None => Ok(SqlValue::Null),
                },
                FID_TRACK => match get_track(&tag) {
                    Some(n) => Ok(SqlValue::Integer(n)),
                    None => Ok(SqlValue::Null),
                },
                FID_DISC => match get_disc(&tag) {
                    Some(n) => Ok(SqlValue::Integer(n)),
                    None => Ok(SqlValue::Null),
                },
                FID_COMMENT => match get_comment(&tag) {
                    Some(s) => Ok(SqlValue::Text(s)),
                    None => Ok(SqlValue::Null),
                },
                FID_ALBUM_ARTIST => match get_album_artist(&tag) {
                    Some(s) => Ok(SqlValue::Text(s)),
                    None => Ok(SqlValue::Null),
                },
                FID_COMPOSER => match get_composer(&tag) {
                    Some(s) => Ok(SqlValue::Text(s)),
                    None => Ok(SqlValue::Null),
                },
                FID_DURATION_MS => match get_duration_ms(&tag) {
                    Some(n) => Ok(SqlValue::Integer(n)),
                    None => Ok(SqlValue::Null),
                },
                FID_VERSION => Ok(SqlValue::Text(get_version(&tag))),
                FID_ALL => Ok(SqlValue::Text(all_json(&tag))),
                other => Err(format!("id3: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
