//! DICOM (Digital Imaging and Communications in Medicine) metadata
//! extraction from medical-imaging blobs. Hand-rolled parser for the
//! Part 10 file format  the upstream `dicom-rs` 0.9 family
//! (`dicom-object`, `dicom-parser`, ...) is a large multi-crate
//! workspace whose async / network / image-codec dependencies do not
//! cross-compile cleanly to wasm32-wasip2. DICOM Part 10 itself is a
//! small, well-defined binary format and a metadata-only parser fits
//! in well under 500 LOC.
//!
//! Function surface:
//!
//!   dicom_patient_name(blob)        -> TEXT
//!   dicom_patient_id(blob)          -> TEXT
//!   dicom_study_date(blob)          -> TEXT  (ISO 8601: YYYY-MM-DD)
//!   dicom_modality(blob)            -> TEXT  (CT / MR / CR / US / ...)
//!   dicom_manufacturer(blob)        -> TEXT
//!   dicom_dimensions(blob)          -> JSON  {"rows":N,"cols":N,"bits":N}
//!   dicom_transfer_syntax(blob)     -> TEXT  (the UID, e.g. 1.2.840.10008.1.2.1)
//!   dicom_tag(blob, group, elem)    -> TEXT  (read any tag; group/elem
//!                                            are 4-hex-digit TEXTs)
//!   dicom_metadata(blob)            -> JSON  (object of common tags)
//!   dicom_is_valid(blob)            -> INTEGER (0 / 1)
//!   dicom_version()                 -> TEXT
//!
//! NULL contract: every accessor returns SQL NULL on
//!   - SqlValue::Null input
//!   - non-BLOB / non-TEXT input
//!   - blobs that don't parse as DICOM Part 10
//!   - the requested tag being absent
//!
//! `dicom_is_valid` is the only fn that returns 0 (not NULL) on a
//! non-DICOM blob  callers want the boolean signal explicitly.
//!
//! DICOM Part 10 wire format (the bits we care about):
//!
//!   bytes 0..128       preamble (any content; almost always 0x00 padding)
//!   bytes 128..132     "DICM" magic
//!   bytes 132..        File Meta Information  group 0x0002, ALWAYS
//!                      Explicit VR Little Endian. First element is
//!                      (0002,0000) FileMetaInformationGroupLength UL,
//!                      its value tells us where group 0x0002 ends.
//!   after FMI          Dataset, encoded with the Transfer Syntax UID
//!                      from (0002,0010). Common syntaxes:
//!                        1.2.840.10008.1.2    Implicit VR Little Endian
//!                        1.2.840.10008.1.2.1  Explicit VR Little Endian
//!                        1.2.840.10008.1.2.2  Explicit VR Big Endian
//!                      Other UIDs are compressed pixel data variants
//!                      whose dataset headers are STILL Explicit VR
//!                      Little Endian per the spec (DICOM PS3.5 10.1).
//!
//! Each Data Element is (group:u16, element:u16, VR, length, value).
//! In Explicit VR LE: VR is 2 ASCII bytes; for VRs in {OB,OW,OF,SQ,UT,UN}
//! the layout is VR(2) + reserved(2) + length(u32 LE). For other VRs
//! the layout is VR(2) + length(u16 LE). In Implicit VR LE: VR is
//! inferred from a tag dictionary; layout is length(u32 LE) directly.
//! We don't carry the full DICOM data dictionary  we only need to
//! decode a known subset of tags by group/element, so for implicit
//! VR we just consume the 4-byte length and treat the value bytes as
//! the storage VR of the requested tag.
//!
//! Errors are NEVER surfaced to SQL  every scalar collapses to NULL
//! on bad input. Each call re-parses the blob fresh; no shared state.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use core::fmt::Write as _;

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

    // ---- Stable function IDs. ----
    const FID_PATIENT_NAME: u64 = 1;
    const FID_PATIENT_ID: u64 = 2;
    const FID_STUDY_DATE: u64 = 3;
    const FID_MODALITY: u64 = 4;
    const FID_MANUFACTURER: u64 = 5;
    const FID_DIMENSIONS: u64 = 6;
    const FID_TRANSFER_SYNTAX: u64 = 7;
    const FID_TAG: u64 = 8;
    const FID_METADATA: u64 = 9;
    const FID_IS_VALID: u64 = 10;
    const FID_VERSION: u64 = 11;

    struct Ext;

    // ---- DICOM constants ----
    const PREAMBLE_LEN: usize = 128;
    const MAGIC: &[u8; 4] = b"DICM";

    // Tags we care about. (group << 16) | element makes a single u32
    // sortable key. We don't carry a full data dictionary  these
    // are the dozen the function surface exposes.
    const TAG_FMI_GROUP_LENGTH: u32 = 0x0002_0000;
    const TAG_TRANSFER_SYNTAX_UID: u32 = 0x0002_0010;
    const TAG_PATIENT_NAME: u32 = 0x0010_0010;
    const TAG_PATIENT_ID: u32 = 0x0010_0020;
    const TAG_STUDY_DATE: u32 = 0x0008_0020;
    const TAG_MODALITY: u32 = 0x0008_0060;
    const TAG_MANUFACTURER: u32 = 0x0008_0070;
    const TAG_ROWS: u32 = 0x0028_0010;
    const TAG_COLUMNS: u32 = 0x0028_0011;
    const TAG_BITS_ALLOCATED: u32 = 0x0028_0100;

    // Standard transfer syntax UIDs that we know how to read past
    // File Meta Information.
    const TS_IMPLICIT_VR_LE: &str = "1.2.840.10008.1.2";
    const TS_EXPLICIT_VR_LE: &str = "1.2.840.10008.1.2.1";
    const TS_EXPLICIT_VR_BE: &str = "1.2.840.10008.1.2.2";

    // ---- Input coercion ----
    //
    // BLOB is the canonical carrier; TEXT is accepted as its byte
    // view so callers that have stuffed a DICOM file into a TEXT
    // column (rare, but possible via lossy decodes) don't need a
    // CAST. NULL / other types -> None -> SQL NULL upstream.
    fn opt_bytes(args: &[SqlValue]) -> Option<Vec<u8>> {
        match args.first() {
            Some(SqlValue::Blob(b)) => Some(b.clone()),
            Some(SqlValue::Text(s)) => Some(s.as_bytes().to_vec()),
            _ => None,
        }
    }

    // ---- Magic check ----
    //
    // The Part 10 preamble is exactly 128 bytes followed by the
    // ASCII string "DICM". A blob without the magic isn't a valid
    // Part 10 file  we refuse it rather than guessing at "raw"
    // DICOM (group 0x0008 first), which is a different surface.
    fn has_magic(bytes: &[u8]) -> bool {
        bytes.len() >= PREAMBLE_LEN + 4 && &bytes[PREAMBLE_LEN..PREAMBLE_LEN + 4] == MAGIC
    }

    // ---- Data Element representation ----
    //
    // VR is two ASCII bytes (e.g. b"PN", b"UI"). We store it as
    // Option<[u8; 2]> because Implicit VR encodes no VR on the wire
    // it has to be inferred from a data dictionary, which we don't
    // carry. None means "decode using the value bytes as-is".
    struct DataElement<'a> {
        tag: u32,
        vr: Option<[u8; 2]>,
        value: &'a [u8],
    }

    // ---- VR length encoding ----
    //
    // Per PS3.5 7.1.2 these VRs use the extended (32-bit) length form
    // in Explicit VR encodings; everything else uses 16-bit. The
    // 32-bit-length VRs are: OB OD OF OL OW SQ UC UN UR UT.
    fn is_long_length_vr(vr: &[u8; 2]) -> bool {
        matches!(
            vr,
            b"OB" | b"OD" | b"OF" | b"OL" | b"OW" | b"SQ" | b"UC" | b"UN" | b"UR" | b"UT"
        )
    }

    // ---- Byte readers ----
    //
    // Tiny LE/BE primitives. Each returns None when the buffer is
    // too short so a truncated DICOM file collapses to NULL rather
    // than panicking.
    fn read_u16_le(b: &[u8], off: usize) -> Option<u16> {
        b.get(off..off + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
    }
    fn read_u16_be(b: &[u8], off: usize) -> Option<u16> {
        b.get(off..off + 2).map(|s| u16::from_be_bytes([s[0], s[1]]))
    }
    fn read_u32_le(b: &[u8], off: usize) -> Option<u32> {
        b.get(off..off + 4)
            .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn read_u32_be(b: &[u8], off: usize) -> Option<u32> {
        b.get(off..off + 4)
            .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }

    // ---- Parse one Explicit VR data element ----
    //
    // Returns (DataElement, bytes consumed). The `big_endian` flag
    // only flips the value-length integers; the tag-group/element
    // pair is ALWAYS encoded with the transfer syntax's byte order
    // per spec (and File Meta Information is always little-endian
    // even when the rest of the dataset is big-endian).
    fn parse_explicit_vr<'a>(
        buf: &'a [u8],
        off: usize,
        big_endian: bool,
    ) -> Option<(DataElement<'a>, usize)> {
        let group = if big_endian {
            read_u16_be(buf, off)?
        } else {
            read_u16_le(buf, off)?
        };
        let element = if big_endian {
            read_u16_be(buf, off + 2)?
        } else {
            read_u16_le(buf, off + 2)?
        };
        let tag = ((group as u32) << 16) | (element as u32);
        let vr_bytes = buf.get(off + 4..off + 6)?;
        let vr = [vr_bytes[0], vr_bytes[1]];
        let (length, header_len) = if is_long_length_vr(&vr) {
            // VR(2) + reserved(2) + length(4) = 8 bytes of header
            let l = if big_endian {
                read_u32_be(buf, off + 8)?
            } else {
                read_u32_le(buf, off + 8)?
            };
            (l as usize, 12usize)
        } else {
            // VR(2) + length(2) = 4 bytes of header (after tag)
            let l = if big_endian {
                read_u16_be(buf, off + 6)?
            } else {
                read_u16_le(buf, off + 6)?
            };
            (l as usize, 8usize)
        };
        // 0xFFFFFFFF is the "undefined length" marker for SQ / OB /
        // pixel data. We don't recurse into sequences  for the
        // metadata-only surface we either skip them entirely (top-
        // level value=&[]) or rely on the FMI never containing one.
        let value_len = if length == 0xFFFF_FFFF { 0 } else { length };
        let value = buf.get(off + header_len..off + header_len + value_len)?;
        Some((DataElement { tag, vr: Some(vr), value }, header_len + value_len))
    }

    // ---- Parse one Implicit VR data element ----
    //
    // Implicit VR has no VR on the wire; the layout is
    // group(2) + element(2) + length(4) = 8-byte header. Endianness
    // is always little for the only standard implicit syntax.
    fn parse_implicit_vr_le<'a>(buf: &'a [u8], off: usize) -> Option<(DataElement<'a>, usize)> {
        let group = read_u16_le(buf, off)?;
        let element = read_u16_le(buf, off + 2)?;
        let tag = ((group as u32) << 16) | (element as u32);
        let length = read_u32_le(buf, off + 4)? as usize;
        let value_len = if length == 0xFFFF_FFFF { 0 } else { length };
        let value = buf.get(off + 8..off + 8 + value_len)?;
        Some((
            DataElement {
                tag,
                vr: None,
                value,
            },
            8 + value_len,
        ))
    }

    // ---- File Meta Information ----
    //
    // FMI is ALWAYS Explicit VR Little Endian per PS3.10 7.1. We walk
    // it forward, picking up the group-length value (first element,
    // tells us where FMI ends) and the transfer syntax UID. Returns
    // (dataset_start_offset, transfer_syntax_uid).
    fn parse_fmi(bytes: &[u8]) -> Option<(usize, String)> {
        let start = PREAMBLE_LEN + 4; // skip preamble + "DICM"
        // First element MUST be (0002,0000) FileMetaInformationGroupLength
        // with VR=UL, length=4. Parse it as a normal Explicit VR LE
        // element rather than special-casing, so we tolerate any of
        // the VR variants compliant writers might emit.
        let (first, used) = parse_explicit_vr(bytes, start, false)?;
        if first.tag != TAG_FMI_GROUP_LENGTH || first.value.len() < 4 {
            return None;
        }
        let fmi_len = u32::from_le_bytes([
            first.value[0],
            first.value[1],
            first.value[2],
            first.value[3],
        ]) as usize;
        let dataset_off = start + used + fmi_len;
        // Walk remaining FMI elements looking for TransferSyntaxUID.
        let mut off = start + used;
        let mut ts: Option<String> = None;
        while off < start + used + fmi_len && off < bytes.len() {
            let (de, n) = match parse_explicit_vr(bytes, off, false) {
                Some(x) => x,
                None => break,
            };
            if de.tag == TAG_TRANSFER_SYNTAX_UID {
                // UI is a string of OIDs, NUL-padded to even length.
                let s = core::str::from_utf8(de.value).ok()?;
                ts = Some(s.trim_end_matches('\0').trim().to_string());
            }
            // Heuristic guard: zero-length step means corruption; bail
            // rather than loop forever.
            if n == 0 {
                return None;
            }
            off += n;
        }
        // Spec default if TS UID is missing: Implicit VR Little Endian.
        let ts = ts.unwrap_or_else(|| TS_IMPLICIT_VR_LE.to_string());
        Some((dataset_off, ts))
    }

    // ---- Dataset walker ----
    //
    // Walks the body past FMI looking for a specific tag. Returns the
    // value bytes (and inferred VR if known) on first hit, None on
    // exhaustion. Encoding is chosen by the transfer syntax UID:
    //   Implicit VR LE: no VR on the wire
    //   Explicit VR LE / BE: VR + length-of-variable-width
    //   any other UID: dataset is still Explicit VR LE per PS3.5 10.1
    //                  (compressed pixel data only changes the
    //                  PixelData encoding, not the surrounding tags)
    fn find_tag<'a>(
        bytes: &'a [u8],
        dataset_off: usize,
        ts: &str,
        target: u32,
    ) -> Option<DataElement<'a>> {
        let mut off = dataset_off;
        let big_endian = ts == TS_EXPLICIT_VR_BE;
        let implicit = ts == TS_IMPLICIT_VR_LE;
        while off < bytes.len() {
            let (de, n) = if implicit {
                parse_implicit_vr_le(bytes, off)?
            } else {
                parse_explicit_vr(bytes, off, big_endian)?
            };
            // Sequences (VR=SQ) and undefined-length items contain
            // nested data elements we don't want to scan into for
            // top-level tag lookups. If we hit one, skip its value
            // bytes wholesale  the slice was already taken from
            // off+header_len. parse_explicit_vr handled length so we
            // just advance by `n`.
            if de.tag == target {
                return Some(de);
            }
            if n == 0 {
                return None;
            }
            off += n;
        }
        None
    }

    // ---- VR-aware value formatters ----
    //
    // PN (Person Name): caret-delimited components, returned as a
    // single string with carets converted to spaces. NUL-padded.
    fn format_pn(value: &[u8]) -> Option<String> {
        let s = core::str::from_utf8(value).ok()?;
        let s = s.trim_end_matches('\0').trim();
        // PS3.5: family^given^middle^prefix^suffix. Convert to
        // "given middle family" when feasible; collapse multiple
        // empty caret slots gracefully. For the surface we just
        // join non-empty parts with spaces.
        let parts: Vec<&str> = s.split('^').map(str::trim).filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" "))
        }
    }

    // String VRs (LO, SH, CS, UI, UC, UR, ST, LT, IS, DS, etc.): just
    // strip trailing NUL/space pad.
    fn format_str(value: &[u8]) -> Option<String> {
        let s = core::str::from_utf8(value).ok()?;
        let trimmed = s.trim_end_matches('\0').trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }

    // DA (Date): YYYYMMDD -> YYYY-MM-DD (ISO 8601 calendar date).
    // Anything else passes through trimmed.
    fn format_da(value: &[u8]) -> Option<String> {
        let s = format_str(value)?;
        let b = s.as_bytes();
        if b.len() >= 8 && b[..8].iter().all(|c| c.is_ascii_digit()) {
            Some(format!("{}-{}-{}", &s[0..4], &s[4..6], &s[6..8]))
        } else {
            Some(s)
        }
    }

    // US (Unsigned Short) is a 16-bit LE integer per Explicit VR LE.
    // Big-endian datasets store it as BE.
    fn format_us(value: &[u8], big_endian: bool) -> Option<u16> {
        if value.len() < 2 {
            return None;
        }
        Some(if big_endian {
            u16::from_be_bytes([value[0], value[1]])
        } else {
            u16::from_le_bytes([value[0], value[1]])
        })
    }

    // ---- Generic tag-to-text conversion ----
    //
    // Given a DataElement and the dataset endianness, return the
    // best-effort string representation. Used by dicom_tag, which
    // accepts arbitrary (group, element) pairs.
    fn data_element_to_text(de: &DataElement, big_endian: bool) -> Option<String> {
        // VR-aware path when we know the VR (explicit encodings).
        if let Some(vr) = de.vr {
            return match &vr {
                b"PN" => format_pn(de.value),
                // String-like VRs.
                b"LO" | b"SH" | b"CS" | b"UI" | b"UC" | b"UR" | b"ST" | b"LT" | b"AE" | b"AS"
                | b"DS" | b"IS" | b"TM" | b"DT" => format_str(de.value),
                b"DA" => format_da(de.value),
                b"US" => format_us(de.value, big_endian).map(|n| n.to_string()),
                b"SS" => {
                    if de.value.len() < 2 {
                        return None;
                    }
                    let n = if big_endian {
                        i16::from_be_bytes([de.value[0], de.value[1]])
                    } else {
                        i16::from_le_bytes([de.value[0], de.value[1]])
                    };
                    Some(n.to_string())
                }
                b"UL" => {
                    if de.value.len() < 4 {
                        return None;
                    }
                    let n = if big_endian {
                        u32::from_be_bytes([
                            de.value[0],
                            de.value[1],
                            de.value[2],
                            de.value[3],
                        ])
                    } else {
                        u32::from_le_bytes([
                            de.value[0],
                            de.value[1],
                            de.value[2],
                            de.value[3],
                        ])
                    };
                    Some(n.to_string())
                }
                b"SL" => {
                    if de.value.len() < 4 {
                        return None;
                    }
                    let n = if big_endian {
                        i32::from_be_bytes([
                            de.value[0],
                            de.value[1],
                            de.value[2],
                            de.value[3],
                        ])
                    } else {
                        i32::from_le_bytes([
                            de.value[0],
                            de.value[1],
                            de.value[2],
                            de.value[3],
                        ])
                    };
                    Some(n.to_string())
                }
                b"FL" => {
                    if de.value.len() < 4 {
                        return None;
                    }
                    let bits = if big_endian {
                        u32::from_be_bytes([
                            de.value[0],
                            de.value[1],
                            de.value[2],
                            de.value[3],
                        ])
                    } else {
                        u32::from_le_bytes([
                            de.value[0],
                            de.value[1],
                            de.value[2],
                            de.value[3],
                        ])
                    };
                    Some(f32::from_bits(bits).to_string())
                }
                b"FD" => {
                    if de.value.len() < 8 {
                        return None;
                    }
                    let arr = [
                        de.value[0],
                        de.value[1],
                        de.value[2],
                        de.value[3],
                        de.value[4],
                        de.value[5],
                        de.value[6],
                        de.value[7],
                    ];
                    let bits = if big_endian {
                        u64::from_be_bytes(arr)
                    } else {
                        u64::from_le_bytes(arr)
                    };
                    Some(f64::from_bits(bits).to_string())
                }
                // Binary VRs  hex-encode for visibility.
                b"OB" | b"OW" | b"OD" | b"OF" | b"OL" | b"UN" => {
                    let mut out = String::with_capacity(de.value.len() * 2);
                    for b in de.value {
                        let _ = write!(&mut out, "{:02x}", b);
                    }
                    Some(out)
                }
                // Sequences and items we don't recurse into.
                b"SQ" => None,
                // Unknown VR  fall back to UTF-8 best effort.
                _ => format_str(de.value),
            };
        }
        // Implicit VR  no VR known. Best-effort: try UTF-8 first,
        // fall back to hex if non-printable.
        if let Ok(s) = core::str::from_utf8(de.value) {
            let trimmed = s.trim_end_matches('\0').trim();
            // Heuristic: if every byte is printable ASCII (or NUL) we
            // call it a string; otherwise we hex-dump.
            let printable = de
                .value
                .iter()
                .all(|&b| b == 0 || (b >= 0x20 && b < 0x7F));
            if printable && !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
        let mut out = String::with_capacity(de.value.len() * 2);
        for b in de.value {
            let _ = write!(&mut out, "{:02x}", b);
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    // ---- Parse a 4-hex-digit text into a u16 ----
    //
    // The dicom_tag scalar accepts group / element as TEXT containing
    // 4 hex digits (e.g. "0010" / "0020"). We tolerate any case and
    // an optional "0x" prefix. Returns None when the input doesn't
    // parse  caller collapses to SQL NULL.
    fn parse_hex16(s: &str) -> Option<u16> {
        let s = s.trim();
        let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
        u16::from_str_radix(s, 16).ok()
    }

    // ---- JSON helpers ----
    //
    // Tiny JSON-string escaper; we only use it for the metadata /
    // dimensions outputs which are all simple key:value objects.
    fn json_str(out: &mut String, s: &str) {
        out.push('"');
        for c in s.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => {
                    let _ = write!(out, "\\u{:04x}", c as u32);
                }
                c => out.push(c),
            }
        }
        out.push('"');
    }

    // ---- High-level helpers ----
    //
    // Each of these wraps parse_fmi + find_tag into a one-shot lookup.
    // They return None on any failure  caller maps to SQL NULL.
    fn read_string_tag(bytes: &[u8], tag: u32) -> Option<String> {
        if !has_magic(bytes) {
            return None;
        }
        let (off, ts) = parse_fmi(bytes)?;
        let big_endian = ts == TS_EXPLICIT_VR_BE;
        let de = find_tag(bytes, off, &ts, tag)?;
        if tag == TAG_PATIENT_NAME {
            format_pn(de.value).or_else(|| format_str(de.value))
        } else if tag == TAG_STUDY_DATE {
            format_da(de.value)
        } else if let Some(_vr) = de.vr {
            data_element_to_text(&de, big_endian)
        } else {
            format_str(de.value)
        }
    }

    fn read_transfer_syntax(bytes: &[u8]) -> Option<String> {
        if !has_magic(bytes) {
            return None;
        }
        let (_, ts) = parse_fmi(bytes)?;
        Some(ts)
    }

    fn build_metadata_json(bytes: &[u8]) -> Option<String> {
        if !has_magic(bytes) {
            return None;
        }
        let (off, ts) = parse_fmi(bytes)?;
        let big_endian = ts == TS_EXPLICIT_VR_BE;
        let mut out = String::from("{");
        let mut first = true;
        let emit_str = |out: &mut String, first: &mut bool, key: &str, val: &str| {
            if !*first {
                out.push(',');
            }
            *first = false;
            json_str(out, key);
            out.push(':');
            json_str(out, val);
        };
        let emit_int = |out: &mut String, first: &mut bool, key: &str, n: i64| {
            if !*first {
                out.push(',');
            }
            *first = false;
            json_str(out, key);
            out.push(':');
            let _ = write!(out, "{n}");
        };
        emit_str(&mut out, &mut first, "transfer_syntax", &ts);
        // Patient name / id
        if let Some(de) = find_tag(bytes, off, &ts, TAG_PATIENT_NAME) {
            if let Some(s) = format_pn(de.value).or_else(|| format_str(de.value)) {
                emit_str(&mut out, &mut first, "patient_name", &s);
            }
        }
        if let Some(de) = find_tag(bytes, off, &ts, TAG_PATIENT_ID) {
            if let Some(s) = format_str(de.value) {
                emit_str(&mut out, &mut first, "patient_id", &s);
            }
        }
        if let Some(de) = find_tag(bytes, off, &ts, TAG_STUDY_DATE) {
            if let Some(s) = format_da(de.value) {
                emit_str(&mut out, &mut first, "study_date", &s);
            }
        }
        if let Some(de) = find_tag(bytes, off, &ts, TAG_MODALITY) {
            if let Some(s) = format_str(de.value) {
                emit_str(&mut out, &mut first, "modality", &s);
            }
        }
        if let Some(de) = find_tag(bytes, off, &ts, TAG_MANUFACTURER) {
            if let Some(s) = format_str(de.value) {
                emit_str(&mut out, &mut first, "manufacturer", &s);
            }
        }
        if let Some(de) = find_tag(bytes, off, &ts, TAG_ROWS) {
            if let Some(n) = format_us(de.value, big_endian) {
                emit_int(&mut out, &mut first, "rows", n as i64);
            }
        }
        if let Some(de) = find_tag(bytes, off, &ts, TAG_COLUMNS) {
            if let Some(n) = format_us(de.value, big_endian) {
                emit_int(&mut out, &mut first, "cols", n as i64);
            }
        }
        if let Some(de) = find_tag(bytes, off, &ts, TAG_BITS_ALLOCATED) {
            if let Some(n) = format_us(de.value, big_endian) {
                emit_int(&mut out, &mut first, "bits", n as i64);
            }
        }
        out.push('}');
        Some(out)
    }

    fn build_dimensions_json(bytes: &[u8]) -> Option<String> {
        if !has_magic(bytes) {
            return None;
        }
        let (off, ts) = parse_fmi(bytes)?;
        let big_endian = ts == TS_EXPLICIT_VR_BE;
        let rows = find_tag(bytes, off, &ts, TAG_ROWS)
            .and_then(|de| format_us(de.value, big_endian));
        let cols = find_tag(bytes, off, &ts, TAG_COLUMNS)
            .and_then(|de| format_us(de.value, big_endian));
        let bits = find_tag(bytes, off, &ts, TAG_BITS_ALLOCATED)
            .and_then(|de| format_us(de.value, big_endian));
        if rows.is_none() && cols.is_none() && bits.is_none() {
            return None;
        }
        let mut out = String::from("{");
        let mut first = true;
        let emit = |out: &mut String, first: &mut bool, key: &str, n: u16| {
            if !*first {
                out.push(',');
            }
            *first = false;
            json_str(out, key);
            out.push(':');
            let _ = write!(out, "{n}");
        };
        if let Some(r) = rows {
            emit(&mut out, &mut first, "rows", r);
        }
        if let Some(c) = cols {
            emit(&mut out, &mut first, "cols", c);
        }
        if let Some(b) = bits {
            emit(&mut out, &mut first, "bits", b);
        }
        out.push('}');
        Some(out)
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
                name: "dicom".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: alloc::vec![
                    s(FID_PATIENT_NAME, "dicom_patient_name", 1, det),
                    s(FID_PATIENT_ID, "dicom_patient_id", 1, det),
                    s(FID_STUDY_DATE, "dicom_study_date", 1, det),
                    s(FID_MODALITY, "dicom_modality", 1, det),
                    s(FID_MANUFACTURER, "dicom_manufacturer", 1, det),
                    s(FID_DIMENSIONS, "dicom_dimensions", 1, det),
                    s(FID_TRANSFER_SYNTAX, "dicom_transfer_syntax", 1, det),
                    s(FID_TAG, "dicom_tag", 3, det),
                    s(FID_METADATA, "dicom_metadata", 1, det),
                    s(FID_IS_VALID, "dicom_is_valid", 1, det),
                    s(FID_VERSION, "dicom_version", 0, det),
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
            // Version is the only function with no blob input.
            if func_id == FID_VERSION {
                return Ok(SqlValue::Text(format!(
                    "dicom hand-rolled DICOM Part 10 parser; extension {}",
                    env!("CARGO_PKG_VERSION")
                )));
            }

            // dicom_is_valid wants 0 / 1, not NULL, for "not DICOM"
            // inputs. NULL input still maps to NULL (consistent with
            // SQL three-valued logic).
            if func_id == FID_IS_VALID {
                match args.first() {
                    Some(SqlValue::Null) | None => return Ok(SqlValue::Null),
                    _ => {}
                }
                let bytes = match opt_bytes(&args) {
                    Some(b) => b,
                    None => return Ok(SqlValue::Integer(0)),
                };
                // Valid = has DICM magic AND we can walk the FMI to
                // the dataset boundary. Anything weaker (just magic)
                // would let truncated files pass.
                if !has_magic(&bytes) {
                    return Ok(SqlValue::Integer(0));
                }
                return Ok(match parse_fmi(&bytes) {
                    Some(_) => SqlValue::Integer(1),
                    None => SqlValue::Integer(0),
                });
            }

            let Some(bytes) = opt_bytes(&args) else {
                return Ok(SqlValue::Null);
            };

            match func_id {
                FID_PATIENT_NAME => match read_string_tag(&bytes, TAG_PATIENT_NAME) {
                    Some(s) => Ok(SqlValue::Text(s)),
                    None => Ok(SqlValue::Null),
                },
                FID_PATIENT_ID => match read_string_tag(&bytes, TAG_PATIENT_ID) {
                    Some(s) => Ok(SqlValue::Text(s)),
                    None => Ok(SqlValue::Null),
                },
                FID_STUDY_DATE => match read_string_tag(&bytes, TAG_STUDY_DATE) {
                    Some(s) => Ok(SqlValue::Text(s)),
                    None => Ok(SqlValue::Null),
                },
                FID_MODALITY => match read_string_tag(&bytes, TAG_MODALITY) {
                    Some(s) => Ok(SqlValue::Text(s)),
                    None => Ok(SqlValue::Null),
                },
                FID_MANUFACTURER => match read_string_tag(&bytes, TAG_MANUFACTURER) {
                    Some(s) => Ok(SqlValue::Text(s)),
                    None => Ok(SqlValue::Null),
                },
                FID_DIMENSIONS => match build_dimensions_json(&bytes) {
                    Some(s) => Ok(SqlValue::Text(s)),
                    None => Ok(SqlValue::Null),
                },
                FID_TRANSFER_SYNTAX => match read_transfer_syntax(&bytes) {
                    Some(s) => Ok(SqlValue::Text(s)),
                    None => Ok(SqlValue::Null),
                },
                FID_TAG => {
                    // dicom_tag(blob, group_hex, element_hex). The
                    // two hex args are TEXT; integer args are
                    // accepted as decimal numbers too (callers who
                    // pass 16 instead of '0010' shouldn't be left
                    // hanging).
                    let group = match args.get(1) {
                        Some(SqlValue::Text(s)) => match parse_hex16(s) {
                            Some(g) => g,
                            None => return Ok(SqlValue::Null),
                        },
                        Some(SqlValue::Integer(n)) => {
                            if *n < 0 || *n > 0xFFFF {
                                return Ok(SqlValue::Null);
                            }
                            *n as u16
                        }
                        _ => return Ok(SqlValue::Null),
                    };
                    let element = match args.get(2) {
                        Some(SqlValue::Text(s)) => match parse_hex16(s) {
                            Some(e) => e,
                            None => return Ok(SqlValue::Null),
                        },
                        Some(SqlValue::Integer(n)) => {
                            if *n < 0 || *n > 0xFFFF {
                                return Ok(SqlValue::Null);
                            }
                            *n as u16
                        }
                        _ => return Ok(SqlValue::Null),
                    };
                    let target = ((group as u32) << 16) | (element as u32);
                    if !has_magic(&bytes) {
                        return Ok(SqlValue::Null);
                    }
                    let Some((off, ts)) = parse_fmi(&bytes) else {
                        return Ok(SqlValue::Null);
                    };
                    let big_endian = ts == TS_EXPLICIT_VR_BE;
                    // The FMI group (0x0002) lives BEFORE the dataset
                    // offset and is always Explicit VR LE. Search
                    // there first when the requested tag is in
                    // group 0x0002 so callers can read e.g.
                    // (0002,0002) MediaStorageSOPClassUID.
                    let de = if group == 0x0002 {
                        let fmi_start = PREAMBLE_LEN + 4;
                        find_tag(&bytes, fmi_start, TS_EXPLICIT_VR_LE, target)
                    } else {
                        find_tag(&bytes, off, &ts, target)
                    };
                    match de {
                        Some(de) => match data_element_to_text(&de, big_endian) {
                            Some(s) => Ok(SqlValue::Text(s)),
                            None => Ok(SqlValue::Null),
                        },
                        None => Ok(SqlValue::Null),
                    }
                }
                FID_METADATA => match build_metadata_json(&bytes) {
                    Some(s) => Ok(SqlValue::Text(s)),
                    None => Ok(SqlValue::Null),
                },
                _ => unreachable!("FID_VERSION and FID_IS_VALID handled above"),
            }
        }
    }

    // Aggressive: data_element_to_text + the FID_TAG handler must use
    // each other's helpers. Mark unused-VR variants as referenced so
    // the compiler doesn't dead-code them if a particular smoke
    // doesn't exercise them.
    #[allow(dead_code)]
    fn _ref_unused() {
        let _ = parse_hex16("0000");
    }

    bindings::export!(Ext with_types_in bindings);
}
