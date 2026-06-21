//! `mqtt-parse` extension  decode MQTT v3.1.1 + v5.0 control
//! packets from raw bytes. Parser only; no client / no broker.
//!
//! The MQTT control packet on the wire is:
//!
//!   byte 0          packet type (high nibble) + flags (low nibble)
//!   bytes 1..N      remaining length, VarInt (1-4 bytes, MSB=continue)
//!   bytes N..       variable header + payload, exactly `remaining_len`
//!
//! Type codes (high nibble):
//!
//!   1 CONNECT     8 SUBSCRIBE    F (reserved/AUTH in v5)
//!   2 CONNACK     9 SUBACK
//!   3 PUBLISH     A UNSUBSCRIBE
//!   4 PUBACK      B UNSUBACK
//!   5 PUBREC      C PINGREQ
//!   6 PUBREL      D PINGRESP
//!   7 PUBCOMP     E DISCONNECT
//!
//! For PUBLISH the low-nibble flags are: DUP (bit3), QoS (bits2-1),
//! RETAIN (bit0). Variable header is the topic name (UTF-8 length-
//! prefixed) then  if QoS>0  a 2-byte packet id, then  for v5 only
//!  a properties block (VarInt length + opaque bytes). Payload is
//! the rest.
//!
//! Packets identified as v5 vs v3 by per-call hint OR by sniffing the
//! topmost CONNECT packet's protocol-level byte (0x04 = v3.1.1,
//! 0x05 = v5). Since SQL callers won't typically have the CONNECT
//! handshake available alongside individual PUBLISH frames, the
//! scalars accept a 2nd optional arg `version` (3 or 5); default is
//! 3 (v3.1.1) which is the more conservative choice  v5 properties
//! parsing is then only attempted when explicitly requested.

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec;
    use alloc::vec::Vec;

    use serde_json::{json, Map, Value as JValue};

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

    // ---- Function IDs ----
    const FID_PACKET_TYPE: u64 = 1;
    const FID_PACKET_ID: u64 = 2;
    const FID_TOPIC: u64 = 3;
    const FID_PAYLOAD: u64 = 4;
    const FID_QOS: u64 = 5;
    const FID_RETAIN: u64 = 6;
    const FID_PARSE: u64 = 7;
    const FID_IS_VALID: u64 = 8;
    const FID_VERSION: u64 = 9;

    struct Ext;

    // ---- Arg helpers ----

    /// Pull a BLOB / TEXT arg as bytes. NULL  None so callers can
    /// short-circuit to SqlValue::Null per the catalog's blob-scalar
    /// convention. Non-blob/text/null is an error.
    fn arg_blob_opt(args: &[SqlValue], i: usize, fname: &str) -> Result<Option<Vec<u8>>, String> {
        match args.get(i) {
            Some(SqlValue::Blob(b)) => Ok(Some(b.clone())),
            Some(SqlValue::Text(s)) => Ok(Some(s.as_bytes().to_vec())),
            Some(SqlValue::Null) | None => Ok(None),
            _ => Err(format!("{fname}: BLOB arg at {i}")),
        }
    }

    /// Pull an optional version hint (3 or 5). Anything else  3.
    /// Missing arg  3.
    fn arg_version(args: &[SqlValue], i: usize) -> u8 {
        match args.get(i) {
            Some(SqlValue::Integer(5)) => 5,
            Some(SqlValue::Integer(3)) => 3,
            _ => 3,
        }
    }

    // ---- Parser primitives ----

    /// MQTT VarInt (1..=4 bytes, 7 data bits + MSB continue flag).
    /// Returns (value, bytes consumed) or None on overflow / EOF.
    fn read_varint(buf: &[u8]) -> Option<(u32, usize)> {
        let mut value: u32 = 0;
        let mut multiplier: u32 = 1;
        for i in 0..4 {
            let b = *buf.get(i)?;
            value = value.checked_add(((b & 0x7F) as u32).checked_mul(multiplier)?)?;
            if b & 0x80 == 0 {
                return Some((value, i + 1));
            }
            multiplier = multiplier.checked_mul(128)?;
        }
        None
    }

    /// Read a 2-byte big-endian length-prefixed UTF-8 string.
    /// Returns (string, bytes consumed) or None on malformed input.
    fn read_string(buf: &[u8]) -> Option<(String, usize)> {
        if buf.len() < 2 {
            return None;
        }
        let len = u16::from_be_bytes([buf[0], buf[1]]) as usize;
        let end = 2 + len;
        if buf.len() < end {
            return None;
        }
        let s = core::str::from_utf8(&buf[2..end]).ok()?.to_string();
        Some((s, end))
    }

    // ---- Packet model ----

    /// Owned, language-neutral view of a parsed MQTT packet.
    /// Anything specific (CONNECT client_id, SUBSCRIBE filter list)
    /// goes into `extra` as opaque JSON so the SQL surface stays small
    /// while `mqtt_parse` can still expose detail.
    struct Parsed {
        type_name: &'static str,
        type_code: u8,
        flags: u8,
        remaining_length: u32,
        dup: Option<bool>,
        qos: Option<u8>,
        retain: Option<bool>,
        packet_id: Option<u16>,
        topic: Option<String>,
        payload: Option<Vec<u8>>,
        version: u8, // protocol level hint used during parse (3 or 5)
        extra: Map<String, JValue>,
    }

    fn type_name(code: u8) -> Option<&'static str> {
        match code {
            1 => Some("CONNECT"),
            2 => Some("CONNACK"),
            3 => Some("PUBLISH"),
            4 => Some("PUBACK"),
            5 => Some("PUBREC"),
            6 => Some("PUBREL"),
            7 => Some("PUBCOMP"),
            8 => Some("SUBSCRIBE"),
            9 => Some("SUBACK"),
            10 => Some("UNSUBSCRIBE"),
            11 => Some("UNSUBACK"),
            12 => Some("PINGREQ"),
            13 => Some("PINGRESP"),
            14 => Some("DISCONNECT"),
            15 => Some("AUTH"), // v5 only; reserved in v3
            _ => None,
        }
    }

    /// Parse one MQTT control packet from `buf`. Returns None on any
    /// malformed input. `version` is the protocol-level hint  3 for
    /// v3.1.1, 5 for v5.0  used to decide whether to consume the
    /// PUBLISH/SUBSCRIBE/etc. properties block on v5 packets.
    fn parse_packet(buf: &[u8], version: u8) -> Option<Parsed> {
        let head = *buf.first()?;
        let type_code = head >> 4;
        let flags = head & 0x0F;
        let name = type_name(type_code)?;

        let (rem_len, vi_bytes) = read_varint(&buf[1..])?;
        let header_size = 1 + vi_bytes;
        if buf.len() < header_size + rem_len as usize {
            return None;
        }
        let body = &buf[header_size..header_size + rem_len as usize];

        let mut p = Parsed {
            type_name: name,
            type_code,
            flags,
            remaining_length: rem_len,
            dup: None,
            qos: None,
            retain: None,
            packet_id: None,
            topic: None,
            payload: None,
            version,
            extra: Map::new(),
        };

        match type_code {
            // CONNECT  read protocol name+level so v3 vs v5 surfaces
            // in mqtt_parse output regardless of the version hint.
            1 => {
                let mut off = 0;
                let (proto_name, n) = read_string(&body[off..])?;
                off += n;
                if body.len() < off + 1 {
                    return None;
                }
                let level = body[off];
                p.extra.insert("protocol_name".into(), JValue::String(proto_name));
                p.extra.insert("protocol_level".into(), JValue::Number(level.into()));
                // Stash the actual level so the user can see it.
                if level == 4 {
                    p.extra.insert("protocol".into(), JValue::String("MQTT 3.1.1".into()));
                } else if level == 5 {
                    p.extra.insert("protocol".into(), JValue::String("MQTT 5.0".into()));
                }
            }
            // PUBLISH  the workhorse. Flags carry DUP/QoS/RETAIN, the
            // variable header is topic + maybe packet id + maybe v5
            // properties, then payload.
            3 => {
                let dup = (flags & 0b1000) != 0;
                let qos = (flags & 0b0110) >> 1;
                let retain = (flags & 0b0001) != 0;
                if qos > 2 {
                    return None;
                }
                p.dup = Some(dup);
                p.qos = Some(qos);
                p.retain = Some(retain);

                let mut off = 0;
                let (topic, n) = read_string(&body[off..])?;
                off += n;
                if qos > 0 {
                    if body.len() < off + 2 {
                        return None;
                    }
                    let pid = u16::from_be_bytes([body[off], body[off + 1]]);
                    p.packet_id = Some(pid);
                    off += 2;
                }
                // v5: PUBLISH carries a properties block before the payload.
                // Length is a VarInt; we skip the bytes verbatim. Useful
                // metadata (response-topic, content-type) is callable via a
                // later expansion; the minimal surface only exposes the
                // payload bytes.
                if version == 5 {
                    let (prop_len, vi_n) = read_varint(&body[off..])?;
                    off += vi_n;
                    let prop_end = off.checked_add(prop_len as usize)?;
                    if body.len() < prop_end {
                        return None;
                    }
                    off = prop_end;
                }
                p.topic = Some(topic);
                p.payload = Some(body[off..].to_vec());
            }
            // PUBACK / PUBREC / PUBREL / PUBCOMP  all just packet id
            // (v3) or packet id + reason code + optional properties (v5).
            // We only surface the packet id; the rest is in extra for
            // mqtt_parse.
            4 | 5 | 6 | 7 => {
                if body.len() < 2 {
                    return None;
                }
                let pid = u16::from_be_bytes([body[0], body[1]]);
                p.packet_id = Some(pid);
                if version == 5 && body.len() > 2 {
                    p.extra.insert("reason_code".into(), JValue::Number(body[2].into()));
                }
            }
            // SUBSCRIBE  packet id, then v5 properties, then list of
            // (topic filter, options) pairs.
            8 => {
                if body.len() < 2 {
                    return None;
                }
                let pid = u16::from_be_bytes([body[0], body[1]]);
                p.packet_id = Some(pid);
                let mut off = 2;
                if version == 5 {
                    let (prop_len, vi_n) = read_varint(&body[off..])?;
                    off += vi_n;
                    off = off.checked_add(prop_len as usize)?;
                    if body.len() < off {
                        return None;
                    }
                }
                let mut filters: Vec<JValue> = Vec::new();
                while off < body.len() {
                    let (topic, n) = read_string(&body[off..])?;
                    off += n;
                    if body.len() < off + 1 {
                        return None;
                    }
                    let opt = body[off];
                    off += 1;
                    let sub_qos = opt & 0b11;
                    filters.push(json!({
                        "topic_filter": topic,
                        "qos": sub_qos,
                        "options": opt,
                    }));
                }
                p.extra.insert("filters".into(), JValue::Array(filters));
            }
            // SUBACK / UNSUBACK  packet id + return codes list.
            9 | 11 => {
                if body.len() < 2 {
                    return None;
                }
                p.packet_id = Some(u16::from_be_bytes([body[0], body[1]]));
                let mut off = 2;
                if version == 5 {
                    let (prop_len, vi_n) = read_varint(&body[off..])?;
                    off += vi_n;
                    off = off.checked_add(prop_len as usize)?;
                    if body.len() < off {
                        return None;
                    }
                }
                let codes: Vec<JValue> = body[off..]
                    .iter()
                    .map(|b| JValue::Number((*b).into()))
                    .collect();
                p.extra.insert("return_codes".into(), JValue::Array(codes));
            }
            // UNSUBSCRIBE  packet id + topic filter list.
            10 => {
                if body.len() < 2 {
                    return None;
                }
                p.packet_id = Some(u16::from_be_bytes([body[0], body[1]]));
                let mut off = 2;
                if version == 5 {
                    let (prop_len, vi_n) = read_varint(&body[off..])?;
                    off += vi_n;
                    off = off.checked_add(prop_len as usize)?;
                    if body.len() < off {
                        return None;
                    }
                }
                let mut filters: Vec<JValue> = Vec::new();
                while off < body.len() {
                    let (topic, n) = read_string(&body[off..])?;
                    off += n;
                    filters.push(JValue::String(topic));
                }
                p.extra.insert("topic_filters".into(), JValue::Array(filters));
            }
            // CONNACK  session present + return code (v3) / reason (v5).
            2 => {
                if body.len() < 2 {
                    return None;
                }
                p.extra.insert(
                    "session_present".into(),
                    JValue::Bool(body[0] & 0x01 != 0),
                );
                p.extra
                    .insert("return_code".into(), JValue::Number(body[1].into()));
            }
            // PINGREQ / PINGRESP  no body, nothing to record.
            12 | 13 => {}
            // DISCONNECT  empty in v3, reason+properties in v5.
            14 => {
                if version == 5 && !body.is_empty() {
                    p.extra
                        .insert("reason_code".into(), JValue::Number(body[0].into()));
                }
            }
            // AUTH (v5)  reason + properties.
            15 => {
                if !body.is_empty() {
                    p.extra
                        .insert("reason_code".into(), JValue::Number(body[0].into()));
                }
            }
            _ => return None,
        }

        Some(p)
    }

    fn parsed_to_json(p: &Parsed) -> JValue {
        let mut obj = Map::new();
        obj.insert("type".into(), JValue::String(p.type_name.into()));
        obj.insert("type_code".into(), JValue::Number(p.type_code.into()));
        obj.insert("flags".into(), JValue::Number(p.flags.into()));
        obj.insert(
            "remaining_length".into(),
            JValue::Number(p.remaining_length.into()),
        );
        obj.insert(
            "parser_version".into(),
            JValue::Number(p.version.into()),
        );
        if let Some(d) = p.dup {
            obj.insert("dup".into(), JValue::Bool(d));
        }
        if let Some(q) = p.qos {
            obj.insert("qos".into(), JValue::Number(q.into()));
        }
        if let Some(r) = p.retain {
            obj.insert("retain".into(), JValue::Bool(r));
        }
        if let Some(pid) = p.packet_id {
            obj.insert("packet_id".into(), JValue::Number(pid.into()));
        }
        if let Some(ref t) = p.topic {
            obj.insert("topic".into(), JValue::String(t.clone()));
        }
        if let Some(ref pl) = p.payload {
            obj.insert("payload_length".into(), JValue::Number(pl.len().into()));
            // Surface payload as UTF-8 when possible; otherwise as a
            // hex string so JSON output stays printable.
            match core::str::from_utf8(pl) {
                Ok(s) => obj.insert("payload_utf8".into(), JValue::String(s.into())),
                Err(_) => {
                    let hex: String = pl.iter().map(|b| format!("{b:02x}")).collect();
                    obj.insert("payload_hex".into(), JValue::String(hex))
                }
            };
        }
        for (k, v) in p.extra.iter() {
            obj.insert(k.clone(), v.clone());
        }
        JValue::Object(obj)
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
                name: "mqtt-parse".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: vec![
                    // num_args = -1 means variadic: callers can pass an
                    // optional second arg with the protocol version (3
                    // or 5). Default is 3 (v3.1.1).
                    s(FID_PACKET_TYPE, "mqtt_packet_type", -1),
                    s(FID_PACKET_ID, "mqtt_packet_id", -1),
                    s(FID_TOPIC, "mqtt_topic", -1),
                    s(FID_PAYLOAD, "mqtt_payload", -1),
                    s(FID_QOS, "mqtt_qos", -1),
                    s(FID_RETAIN, "mqtt_retain", -1),
                    s(FID_PARSE, "mqtt_parse", -1),
                    s(FID_IS_VALID, "mqtt_is_valid", -1),
                    s(FID_VERSION, "mqtt_version", 0),
                ],
                aggregate_functions: vec![],
                collations: vec![],
                vtabs: vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                dot_commands: alloc::vec![],
                declared_capabilities: vec![],
            }
        }
    }

    /// Helper: parse + map. NULL on failure or NULL input  matches the
    /// per-task spec ("NULL on parse failure") and dovetails with the
    /// rest of the catalog's blob-scalar convention.
    fn with_parsed<F: FnOnce(&Parsed) -> SqlValue>(
        args: &[SqlValue],
        fname: &str,
        f: F,
    ) -> Result<SqlValue, String> {
        let blob = match arg_blob_opt(args, 0, fname)? {
            Some(b) => b,
            None => return Ok(SqlValue::Null),
        };
        let version = arg_version(args, 1);
        Ok(match parse_packet(&blob, version) {
            Some(p) => f(&p),
            None => SqlValue::Null,
        })
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_PACKET_TYPE => with_parsed(&args, "mqtt_packet_type", |p| {
                    SqlValue::Text(p.type_name.to_string())
                }),
                FID_PACKET_ID => with_parsed(&args, "mqtt_packet_id", |p| match p.packet_id {
                    Some(pid) => SqlValue::Integer(pid as i64),
                    None => SqlValue::Null,
                }),
                FID_TOPIC => with_parsed(&args, "mqtt_topic", |p| match &p.topic {
                    Some(t) => SqlValue::Text(t.clone()),
                    None => SqlValue::Null,
                }),
                FID_PAYLOAD => with_parsed(&args, "mqtt_payload", |p| match &p.payload {
                    Some(b) => SqlValue::Blob(b.clone()),
                    None => SqlValue::Null,
                }),
                FID_QOS => with_parsed(&args, "mqtt_qos", |p| match p.qos {
                    Some(q) => SqlValue::Integer(q as i64),
                    None => SqlValue::Null,
                }),
                FID_RETAIN => with_parsed(&args, "mqtt_retain", |p| match p.retain {
                    Some(r) => SqlValue::Integer(if r { 1 } else { 0 }),
                    None => SqlValue::Null,
                }),
                FID_PARSE => with_parsed(&args, "mqtt_parse", |p| {
                    SqlValue::Text(parsed_to_json(p).to_string())
                }),
                FID_IS_VALID => {
                    // is_valid never NULLs except on NULL input  it's the
                    // "did this parse?" probe.
                    let blob = match arg_blob_opt(&args, 0, "mqtt_is_valid")? {
                        Some(b) => b,
                        None => return Ok(SqlValue::Null),
                    };
                    let version = arg_version(&args, 1);
                    Ok(SqlValue::Integer(
                        if parse_packet(&blob, version).is_some() {
                            1
                        } else {
                            0
                        },
                    ))
                }
                FID_VERSION => Ok(SqlValue::Text(format!(
                    "mqtt-parse {}; v3.1.1 + v5.0 wire formats",
                    env!("CARGO_PKG_VERSION")
                ))),
                other => Err(format!("mqtt-parse: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
