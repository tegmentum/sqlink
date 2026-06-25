//! ASN.1 / DER scalars for SQL.
//!
//! Function surface (PLAN-more-extensions-4.md  7):
//!
//!   asn1_decode(der_blob)        -> text  JSON tree
//!   asn1_encode(json_tree)       -> blob  DER bytes
//!   asn1_oid_name(oid_dotted)    -> text  curated lookup
//!   asn1_oid_for(name)           -> text  reverse lookup
//!   asn1_is_valid_der(blob)      -> integer 0/1
//!   asn1_type_tag(der_blob)      -> integer first byte
//!   asn1_pretty(der_blob)        -> text  pretty-printed JSON
//!   asn1_version()               -> text
//!
//! JSON tree shape  one node per ASN.1 block:
//!   { "type": "SEQUENCE" | "INTEGER" | ... ,
//!     "tag":  <number; only set for non-universal / tagged blocks>,
//!     "class": "context-specific" | "application" | "private" ,
//!     "constructed": true | false ,
//!     "children": [ ... ]   // SEQUENCE / SET / EXPLICIT
//!     "value":     <type-specific scalar>   // primitives
//!   }
//!
//! Primitive `value` rules:
//!   BOOLEAN              -> json bool
//!   INTEGER              -> json string (decimal; could exceed i64)
//!   NULL                 -> null literal
//!   OBJECT IDENTIFIER    -> json string (dotted: "1.2.840.113549.1.1.11")
//!   UTF8/PRINTABLE/IA5/etc. STRING -> json string
//!   OCTET STRING         -> json string (lowercase hex)
//!   BIT STRING           -> json string (lowercase hex; first byte is
//!                          the "unused bits" count per DER)
//!   UTCTime / GeneralizedTime -> json string (ISO 8601 UTC)
//!   UNKNOWN              -> json string (lowercase hex of content)
//!
//! DER only; BER / CER are out of scope (the plan calls this out
//! explicitly).

extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_export {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec;
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

    use simple_asn1::{from_der, to_der, ASN1Block, ASN1Class, BigInt, BigUint, OID};
    use serde_json::{json, Map, Value};

    const FID_DECODE: u64 = 1;
    const FID_ENCODE: u64 = 2;
    const FID_OID_NAME: u64 = 3;
    const FID_OID_FOR: u64 = 4;
    const FID_IS_VALID_DER: u64 = 5;
    const FID_TYPE_TAG: u64 = 6;
    const FID_PRETTY: u64 = 7;
    const FID_VERSION: u64 = 8;

    struct Ext;

    // ----- value plumbing -----

    fn blob_arg(args: &[SqlValue], idx: usize, fname: &str) -> Result<Option<Vec<u8>>, String> {
        match args.get(idx) {
            None | Some(SqlValue::Null) => Ok(None),
            Some(SqlValue::Blob(b)) => Ok(Some(b.clone())),
            Some(SqlValue::Text(s)) => Ok(Some(s.as_bytes().to_vec())),
            _ => Err(format!("{fname}: arg {} must be BLOB / TEXT / NULL", idx + 1)),
        }
    }

    fn text_arg(args: &[SqlValue], idx: usize, fname: &str) -> Result<Option<String>, String> {
        match args.get(idx) {
            None | Some(SqlValue::Null) => Ok(None),
            Some(SqlValue::Text(s)) => Ok(Some(s.clone())),
            _ => Err(format!("{fname}: arg {} must be TEXT", idx + 1)),
        }
    }

    // ----- decode: ASN.1  serde_json::Value -----

    fn class_str(c: ASN1Class) -> &'static str {
        match c {
            ASN1Class::Universal => "universal",
            ASN1Class::Application => "application",
            ASN1Class::ContextSpecific => "context-specific",
            ASN1Class::Private => "private",
        }
    }

    fn oid_to_dotted(oid: &OID) -> String {
        // OID::as_vec<u64>() only works for fields that fit in u64.
        // OID arcs do fit u64 in practice (X.509 + PKCS use small
        // values), but fall back to the BigUint Display impl through
        // the raw vector if as_vec fails  preserves full precision.
        match oid.as_vec::<u64>() {
            Ok(v) => v
                .into_iter()
                .map(|x| x.to_string())
                .collect::<Vec<_>>()
                .join("."),
            Err(_) => {
                // The OID's internal Vec<BigUint> isn't pub; round-trip
                // through encode/decode to recover arcs. Pragmatic
                // fallback for absurdly huge arcs (none of the curated
                // OIDs hit this).
                String::from("<oid-overflow>")
            }
        }
    }

    fn datetime_to_iso(t: &time::PrimitiveDateTime) -> String {
        // Render as ISO 8601 UTC (these are UTCTime / GeneralizedTime,
        // which are inherently UTC). time::PrimitiveDateTime doesn't
        // carry an offset so we explicitly tag Z.
        let (y, m, d) = (t.year(), t.month() as u8, t.day());
        let (hh, mm, ss) = (t.hour(), t.minute(), t.second());
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            y, m, d, hh, mm, ss
        )
    }

    fn block_to_value(b: &ASN1Block) -> Value {
        let mut obj: Map<String, Value> = Map::new();
        match b {
            ASN1Block::Boolean(_, v) => {
                obj.insert("type".into(), json!("BOOLEAN"));
                obj.insert("value".into(), json!(*v));
            }
            ASN1Block::Integer(_, big) => {
                obj.insert("type".into(), json!("INTEGER"));
                // Stringify to preserve arbitrary precision; DER
                // integers can be 4096-bit RSA moduli.
                obj.insert("value".into(), json!(big.to_str_radix(10)));
            }
            ASN1Block::BitString(_, unused, bits) => {
                obj.insert("type".into(), json!("BIT STRING"));
                obj.insert("unused_bits".into(), json!(*unused));
                obj.insert("value".into(), json!(hex::encode(bits)));
            }
            ASN1Block::OctetString(_, bytes) => {
                obj.insert("type".into(), json!("OCTET STRING"));
                obj.insert("value".into(), json!(hex::encode(bytes)));
            }
            ASN1Block::Null(_) => {
                obj.insert("type".into(), json!("NULL"));
                obj.insert("value".into(), Value::Null);
            }
            ASN1Block::ObjectIdentifier(_, oid) => {
                obj.insert("type".into(), json!("OBJECT IDENTIFIER"));
                let dotted = oid_to_dotted(oid);
                obj.insert("value".into(), json!(dotted.clone()));
                if let Some(name) = lookup_oid_name(&dotted) {
                    obj.insert("name".into(), json!(name));
                }
            }
            ASN1Block::UTF8String(_, s) => {
                obj.insert("type".into(), json!("UTF8String"));
                obj.insert("value".into(), json!(s));
            }
            ASN1Block::PrintableString(_, s) => {
                obj.insert("type".into(), json!("PrintableString"));
                obj.insert("value".into(), json!(s));
            }
            ASN1Block::TeletexString(_, s) => {
                obj.insert("type".into(), json!("TeletexString"));
                obj.insert("value".into(), json!(s));
            }
            ASN1Block::IA5String(_, s) => {
                obj.insert("type".into(), json!("IA5String"));
                obj.insert("value".into(), json!(s));
            }
            ASN1Block::UniversalString(_, s) => {
                obj.insert("type".into(), json!("UniversalString"));
                obj.insert("value".into(), json!(s));
            }
            ASN1Block::BMPString(_, s) => {
                obj.insert("type".into(), json!("BMPString"));
                obj.insert("value".into(), json!(s));
            }
            ASN1Block::UTCTime(_, t) => {
                obj.insert("type".into(), json!("UTCTime"));
                obj.insert("value".into(), json!(datetime_to_iso(t)));
            }
            ASN1Block::GeneralizedTime(_, t) => {
                obj.insert("type".into(), json!("GeneralizedTime"));
                obj.insert("value".into(), json!(datetime_to_iso(t)));
            }
            ASN1Block::Sequence(_, children) => {
                obj.insert("type".into(), json!("SEQUENCE"));
                let kids: Vec<Value> = children.iter().map(block_to_value).collect();
                obj.insert("children".into(), Value::Array(kids));
            }
            ASN1Block::Set(_, children) => {
                obj.insert("type".into(), json!("SET"));
                let kids: Vec<Value> = children.iter().map(block_to_value).collect();
                obj.insert("children".into(), Value::Array(kids));
            }
            ASN1Block::Explicit(class, _, tag, inner) => {
                obj.insert("type".into(), json!("EXPLICIT"));
                obj.insert("class".into(), json!(class_str(*class)));
                obj.insert("tag".into(), json!(tag.to_str_radix(10)));
                obj.insert("children".into(), json!([block_to_value(inner)]));
            }
            ASN1Block::Unknown(class, constructed, _, tag, content) => {
                obj.insert("type".into(), json!("UNKNOWN"));
                obj.insert("class".into(), json!(class_str(*class)));
                obj.insert("constructed".into(), json!(*constructed));
                obj.insert("tag".into(), json!(tag.to_str_radix(10)));
                obj.insert("value".into(), json!(hex::encode(content)));
            }
        }
        Value::Object(obj)
    }

    fn decode_to_json(der: &[u8]) -> Result<Value, String> {
        let blocks =
            from_der(der).map_err(|e| format!("asn1_decode: parse error: {:?}", e))?;
        if blocks.len() == 1 {
            Ok(block_to_value(&blocks[0]))
        } else {
            // Multiple top-level blocks  return an array. Rare but
            // legal in some PEM-cat-PEM situations.
            Ok(Value::Array(blocks.iter().map(block_to_value).collect()))
        }
    }

    // ----- encode: JSON tree  ASN.1 -----

    fn obj_field<'a>(o: &'a Map<String, Value>, k: &str) -> Result<&'a Value, String> {
        o.get(k)
            .ok_or_else(|| format!("asn1_encode: missing field '{k}'"))
    }

    fn parse_class(s: &str) -> Result<ASN1Class, String> {
        match s {
            "universal" => Ok(ASN1Class::Universal),
            "application" => Ok(ASN1Class::Application),
            "context-specific" | "contextSpecific" | "context" => {
                Ok(ASN1Class::ContextSpecific)
            }
            "private" => Ok(ASN1Class::Private),
            other => Err(format!("asn1_encode: unknown class '{other}'")),
        }
    }

    fn parse_oid_dotted(s: &str) -> Result<OID, String> {
        let mut arcs: Vec<BigUint> = Vec::new();
        for piece in s.split('.') {
            let n: BigUint = piece
                .parse()
                .map_err(|e| format!("asn1_encode: bad OID arc '{piece}': {e}"))?;
            arcs.push(n);
        }
        if arcs.len() < 2 {
            return Err("asn1_encode: OID must have at least 2 arcs".into());
        }
        Ok(OID::new(arcs))
    }

    fn parse_hex(s: &str, ctx: &str) -> Result<Vec<u8>, String> {
        hex::decode(s).map_err(|e| format!("asn1_encode: bad hex in {ctx}: {e}"))
    }

    fn parse_iso_dt(s: &str) -> Result<time::PrimitiveDateTime, String> {
        // Accept "YYYY-MM-DDTHH:MM:SSZ" (and the same without Z).
        // Hand-rolled parser to avoid pulling time's parsing
        // features which would balloon the wasm binary.
        let trimmed = s.trim_end_matches('Z');
        let bytes = trimmed.as_bytes();
        if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-'
            || (bytes[10] != b'T' && bytes[10] != b' ')
            || bytes[13] != b':' || bytes[16] != b':'
        {
            return Err(format!("asn1_encode: bad datetime '{s}'"));
        }
        let parse = |from: usize, to: usize| -> Result<i32, String> {
            core::str::from_utf8(&bytes[from..to])
                .map_err(|e| format!("asn1_encode: datetime utf8: {e}"))?
                .parse()
                .map_err(|e| format!("asn1_encode: datetime field: {e}"))
        };
        let y = parse(0, 4)?;
        let mo = parse(5, 7)? as u8;
        let d = parse(8, 10)? as u8;
        let hh = parse(11, 13)? as u8;
        let mm = parse(14, 16)? as u8;
        let ss = parse(17, 19)? as u8;
        let month = time::Month::try_from(mo)
            .map_err(|e| format!("asn1_encode: month: {e}"))?;
        let date = time::Date::from_calendar_date(y, month, d)
            .map_err(|e| format!("asn1_encode: date: {e}"))?;
        let time = time::Time::from_hms(hh, mm, ss)
            .map_err(|e| format!("asn1_encode: time: {e}"))?;
        Ok(time::PrimitiveDateTime::new(date, time))
    }

    fn value_to_block(v: &Value) -> Result<ASN1Block, String> {
        // Top-level shape: object with at least `type`. Some inputs
        // (recursion target) might be an array of children; the
        // caller wraps those.
        let o = v.as_object().ok_or_else(|| {
            format!("asn1_encode: expected object node, got {}", v)
        })?;
        let ty = obj_field(o, "type")?
            .as_str()
            .ok_or_else(|| "asn1_encode: 'type' must be string".to_string())?;

        match ty {
            "BOOLEAN" => {
                let b = obj_field(o, "value")?
                    .as_bool()
                    .ok_or_else(|| "asn1_encode: BOOLEAN value must be bool".to_string())?;
                Ok(ASN1Block::Boolean(0, b))
            }
            "INTEGER" => {
                let s = obj_field(o, "value")?;
                // Accept either a numeric or a decimal string.
                let dec = match s {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    _ => return Err("asn1_encode: INTEGER value must be string/number".into()),
                };
                let big = dec
                    .parse::<BigInt>()
                    .map_err(|e| format!("asn1_encode: INTEGER parse: {e}"))?;
                Ok(ASN1Block::Integer(0, big))
            }
            "NULL" => Ok(ASN1Block::Null(0)),
            "OBJECT IDENTIFIER" => {
                let s = obj_field(o, "value")?
                    .as_str()
                    .ok_or_else(|| "asn1_encode: OID value must be string".to_string())?;
                Ok(ASN1Block::ObjectIdentifier(0, parse_oid_dotted(s)?))
            }
            "UTF8String" => {
                let s = obj_field(o, "value")?
                    .as_str()
                    .ok_or_else(|| "asn1_encode: UTF8String value must be string".to_string())?;
                Ok(ASN1Block::UTF8String(0, s.to_string()))
            }
            "PrintableString" => {
                let s = obj_field(o, "value")?
                    .as_str()
                    .ok_or_else(|| "asn1_encode: PrintableString value must be string".to_string())?;
                Ok(ASN1Block::PrintableString(0, s.to_string()))
            }
            "IA5String" => {
                let s = obj_field(o, "value")?
                    .as_str()
                    .ok_or_else(|| "asn1_encode: IA5String value must be string".to_string())?;
                Ok(ASN1Block::IA5String(0, s.to_string()))
            }
            "TeletexString" => {
                let s = obj_field(o, "value")?
                    .as_str()
                    .ok_or_else(|| "asn1_encode: TeletexString value must be string".to_string())?;
                Ok(ASN1Block::TeletexString(0, s.to_string()))
            }
            "UniversalString" => {
                let s = obj_field(o, "value")?
                    .as_str()
                    .ok_or_else(|| "asn1_encode: UniversalString value must be string".to_string())?;
                Ok(ASN1Block::UniversalString(0, s.to_string()))
            }
            "BMPString" => {
                let s = obj_field(o, "value")?
                    .as_str()
                    .ok_or_else(|| "asn1_encode: BMPString value must be string".to_string())?;
                Ok(ASN1Block::BMPString(0, s.to_string()))
            }
            "OCTET STRING" => {
                let s = obj_field(o, "value")?
                    .as_str()
                    .ok_or_else(|| "asn1_encode: OCTET STRING value must be hex string".to_string())?;
                Ok(ASN1Block::OctetString(0, parse_hex(s, "OCTET STRING")?))
            }
            "BIT STRING" => {
                let unused = o
                    .get("unused_bits")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0) as usize;
                let s = obj_field(o, "value")?
                    .as_str()
                    .ok_or_else(|| "asn1_encode: BIT STRING value must be hex string".to_string())?;
                let bytes = parse_hex(s, "BIT STRING")?;
                Ok(ASN1Block::BitString(0, unused, bytes))
            }
            "UTCTime" => {
                let s = obj_field(o, "value")?
                    .as_str()
                    .ok_or_else(|| "asn1_encode: UTCTime value must be string".to_string())?;
                Ok(ASN1Block::UTCTime(0, parse_iso_dt(s)?))
            }
            "GeneralizedTime" => {
                let s = obj_field(o, "value")?
                    .as_str()
                    .ok_or_else(|| "asn1_encode: GeneralizedTime value must be string".to_string())?;
                Ok(ASN1Block::GeneralizedTime(0, parse_iso_dt(s)?))
            }
            "SEQUENCE" => {
                let kids = obj_field(o, "children")?
                    .as_array()
                    .ok_or_else(|| "asn1_encode: SEQUENCE children must be array".to_string())?;
                let mut out: Vec<ASN1Block> = Vec::with_capacity(kids.len());
                for k in kids {
                    out.push(value_to_block(k)?);
                }
                Ok(ASN1Block::Sequence(0, out))
            }
            "SET" => {
                let kids = obj_field(o, "children")?
                    .as_array()
                    .ok_or_else(|| "asn1_encode: SET children must be array".to_string())?;
                let mut out: Vec<ASN1Block> = Vec::with_capacity(kids.len());
                for k in kids {
                    out.push(value_to_block(k)?);
                }
                Ok(ASN1Block::Set(0, out))
            }
            "EXPLICIT" => {
                let class = obj_field(o, "class")?
                    .as_str()
                    .ok_or_else(|| "asn1_encode: EXPLICIT class must be string".to_string())?;
                let class = parse_class(class)?;
                let tag_str = match obj_field(o, "tag")? {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    _ => return Err("asn1_encode: EXPLICIT tag must be string/number".into()),
                };
                let tag: BigUint = tag_str
                    .parse()
                    .map_err(|e| format!("asn1_encode: EXPLICIT tag parse: {e}"))?;
                let kids = obj_field(o, "children")?
                    .as_array()
                    .ok_or_else(|| "asn1_encode: EXPLICIT children must be array".to_string())?;
                if kids.len() != 1 {
                    return Err("asn1_encode: EXPLICIT must have exactly 1 child".into());
                }
                let inner = value_to_block(&kids[0])?;
                Ok(ASN1Block::Explicit(class, 0, tag, alloc::boxed::Box::new(inner)))
            }
            other => Err(format!("asn1_encode: unknown type '{other}'")),
        }
    }

    fn encode_json(tree: &Value) -> Result<Vec<u8>, String> {
        let blk = value_to_block(tree)?;
        to_der(&blk).map_err(|e| format!("asn1_encode: encode error: {:?}", e))
    }

    // ----- OID table -----

    /// Curated DER OID  pretty name table. ~80 of the most common
    /// crypto + directory OIDs (sha-* digest algs, signature algs,
    /// RDN attributes, EC curves, PKCS, S/MIME, key purposes).
    /// Source: RFC 8017 / RFC 5280 / RFC 3279 / NIST / IANA registry.
    /// Kept short on purpose  longer tables are a follow-on; this
    /// covers everything an `x509` smoke needs.
    const OID_TABLE: &[(&str, &str)] = &[
        // -- Hash algorithms (RFC 3447 / 5754) --
        ("1.2.840.113549.2.5", "md5"),
        ("1.3.14.3.2.26", "sha1"),
        ("2.16.840.1.101.3.4.2.1", "sha256"),
        ("2.16.840.1.101.3.4.2.2", "sha384"),
        ("2.16.840.1.101.3.4.2.3", "sha512"),
        ("2.16.840.1.101.3.4.2.4", "sha224"),
        ("2.16.840.1.101.3.4.2.7", "sha512-224"),
        ("2.16.840.1.101.3.4.2.8", "sha512-256"),
        // -- PKCS#1 signature / key algorithms (RFC 8017) --
        ("1.2.840.113549.1.1.1", "rsaEncryption"),
        ("1.2.840.113549.1.1.5", "sha1WithRSAEncryption"),
        ("1.2.840.113549.1.1.7", "RSAES-OAEP"),
        ("1.2.840.113549.1.1.8", "id-mgf1"),
        ("1.2.840.113549.1.1.9", "id-pSpecified"),
        ("1.2.840.113549.1.1.10", "RSASSA-PSS"),
        ("1.2.840.113549.1.1.11", "sha256WithRSAEncryption"),
        ("1.2.840.113549.1.1.12", "sha384WithRSAEncryption"),
        ("1.2.840.113549.1.1.13", "sha512WithRSAEncryption"),
        ("1.2.840.113549.1.1.14", "sha224WithRSAEncryption"),
        // -- DSA / ECDSA (RFC 3279 / 5758) --
        ("1.2.840.10040.4.1", "id-dsa"),
        ("1.2.840.10040.4.3", "dsa-with-sha1"),
        ("2.16.840.1.101.3.4.3.1", "dsa-with-sha224"),
        ("2.16.840.1.101.3.4.3.2", "dsa-with-sha256"),
        ("1.2.840.10045.2.1", "ecPublicKey"),
        ("1.2.840.10045.4.1", "ecdsa-with-SHA1"),
        ("1.2.840.10045.4.3.1", "ecdsa-with-SHA224"),
        ("1.2.840.10045.4.3.2", "ecdsa-with-SHA256"),
        ("1.2.840.10045.4.3.3", "ecdsa-with-SHA384"),
        ("1.2.840.10045.4.3.4", "ecdsa-with-SHA512"),
        // -- Elliptic curves (RFC 5480 / SEC2) --
        ("1.2.840.10045.3.1.7", "prime256v1"),
        ("1.3.132.0.34", "secp384r1"),
        ("1.3.132.0.35", "secp521r1"),
        ("1.3.132.0.10", "secp256k1"),
        // -- EdDSA (RFC 8410) --
        ("1.3.101.112", "Ed25519"),
        ("1.3.101.113", "Ed448"),
        ("1.3.101.110", "X25519"),
        ("1.3.101.111", "X448"),
        // -- HMAC (RFC 4231) --
        ("1.2.840.113549.2.7", "hmacWithSHA1"),
        ("1.2.840.113549.2.8", "hmacWithSHA224"),
        ("1.2.840.113549.2.9", "hmacWithSHA256"),
        ("1.2.840.113549.2.10", "hmacWithSHA384"),
        ("1.2.840.113549.2.11", "hmacWithSHA512"),
        // -- RDN attribute types (RFC 4519 / 5280) --
        ("2.5.4.3", "commonName"),
        ("2.5.4.4", "surname"),
        ("2.5.4.5", "serialNumber"),
        ("2.5.4.6", "countryName"),
        ("2.5.4.7", "localityName"),
        ("2.5.4.8", "stateOrProvinceName"),
        ("2.5.4.9", "streetAddress"),
        ("2.5.4.10", "organizationName"),
        ("2.5.4.11", "organizationalUnitName"),
        ("2.5.4.12", "title"),
        ("2.5.4.13", "description"),
        ("2.5.4.41", "name"),
        ("2.5.4.42", "givenName"),
        ("2.5.4.43", "initials"),
        ("2.5.4.46", "dnQualifier"),
        ("2.5.4.65", "pseudonym"),
        ("1.2.840.113549.1.9.1", "emailAddress"),
        ("0.9.2342.19200300.100.1.25", "domainComponent"),
        ("0.9.2342.19200300.100.1.1", "userid"),
        // -- X.509 v3 extensions (RFC 5280) --
        ("2.5.29.14", "subjectKeyIdentifier"),
        ("2.5.29.15", "keyUsage"),
        ("2.5.29.17", "subjectAltName"),
        ("2.5.29.18", "issuerAltName"),
        ("2.5.29.19", "basicConstraints"),
        ("2.5.29.20", "cRLNumber"),
        ("2.5.29.21", "cRLReason"),
        ("2.5.29.30", "nameConstraints"),
        ("2.5.29.31", "cRLDistributionPoints"),
        ("2.5.29.32", "certificatePolicies"),
        ("2.5.29.33", "policyMappings"),
        ("2.5.29.35", "authorityKeyIdentifier"),
        ("2.5.29.36", "policyConstraints"),
        ("2.5.29.37", "extKeyUsage"),
        ("2.5.29.46", "freshestCRL"),
        // -- Extended key purposes (RFC 5280) --
        ("1.3.6.1.5.5.7.3.1", "serverAuth"),
        ("1.3.6.1.5.5.7.3.2", "clientAuth"),
        ("1.3.6.1.5.5.7.3.3", "codeSigning"),
        ("1.3.6.1.5.5.7.3.4", "emailProtection"),
        ("1.3.6.1.5.5.7.3.8", "timeStamping"),
        ("1.3.6.1.5.5.7.3.9", "OCSPSigning"),
        // -- PKIX access methods + S/MIME --
        ("1.3.6.1.5.5.7.1.1", "authorityInfoAccess"),
        ("1.3.6.1.5.5.7.48.1", "id-ad-ocsp"),
        ("1.3.6.1.5.5.7.48.2", "id-ad-caIssuers"),
        ("1.2.840.113549.1.7.1", "pkcs7-data"),
        ("1.2.840.113549.1.7.2", "pkcs7-signedData"),
        ("1.2.840.113549.1.7.3", "pkcs7-envelopedData"),
        // -- PKCS#5 / #8 --
        ("1.2.840.113549.1.5.13", "pkcs5-PBES2"),
        ("1.2.840.113549.1.5.12", "pkcs5-PBKDF2"),
    ];

    fn lookup_oid_name(dotted: &str) -> Option<&'static str> {
        OID_TABLE
            .iter()
            .find(|(k, _)| *k == dotted)
            .map(|(_, v)| *v)
    }

    fn lookup_oid_for(name: &str) -> Option<&'static str> {
        // Case-insensitive name match (callers may pass "commonName"
        // or "commonname"). Exact match only on the dotted side.
        OID_TABLE
            .iter()
            .find(|(_, v)| v.eq_ignore_ascii_case(name))
            .map(|(k, _)| *k)
    }

    // ----- impls -----

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
                name: "asn1".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                scalar_functions: vec![
                    s(FID_DECODE, "asn1_decode", 1, det),
                    s(FID_ENCODE, "asn1_encode", 1, det),
                    s(FID_OID_NAME, "asn1_oid_name", 1, det),
                    s(FID_OID_FOR, "asn1_oid_for", 1, det),
                    s(FID_IS_VALID_DER, "asn1_is_valid_der", 1, det),
                    s(FID_TYPE_TAG, "asn1_type_tag", 1, det),
                    s(FID_PRETTY, "asn1_pretty", 1, det),
                    s(FID_VERSION, "asn1_version", 0, det),
                ],
                aggregate_functions: vec![],
                collations: vec![],
                vtabs: vec![],
                has_authorizer: false,
                has_update_hook: false,
                has_commit_hook: false,
                has_wal_hook: false,
                wal_hook_id: 0,
                dot_commands: alloc::vec![],
                declared_capabilities: vec![],
                optional_capabilities: vec![],
            }
        }
    }

    impl ScalarFunctionGuest for Ext {
        fn call(func_id: u64, args: Vec<SqlValue>) -> Result<SqlValue, String> {
            match func_id {
                FID_DECODE => {
                    let b = match blob_arg(&args, 0, "asn1_decode")? {
                        Some(b) => b,
                        None => return Ok(SqlValue::Null),
                    };
                    match decode_to_json(&b) {
                        Ok(v) => Ok(SqlValue::Text(v.to_string())),
                        // Parse errors return NULL  matches the
                        // `*_is_valid_der` contract; a malformed blob
                        // in a column shouldn't blow up a SELECT.
                        Err(_) => Ok(SqlValue::Null),
                    }
                }
                FID_ENCODE => {
                    let s = match text_arg(&args, 0, "asn1_encode")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    let tree: Value = serde_json::from_str(&s)
                        .map_err(|e| format!("asn1_encode: JSON parse: {e}"))?;
                    let bytes = encode_json(&tree)?;
                    Ok(SqlValue::Blob(bytes))
                }
                FID_OID_NAME => {
                    let s = match text_arg(&args, 0, "asn1_oid_name")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(match lookup_oid_name(&s) {
                        Some(n) => SqlValue::Text(n.to_string()),
                        None => SqlValue::Null,
                    })
                }
                FID_OID_FOR => {
                    let s = match text_arg(&args, 0, "asn1_oid_for")? {
                        Some(s) => s,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(match lookup_oid_for(&s) {
                        Some(n) => SqlValue::Text(n.to_string()),
                        None => SqlValue::Null,
                    })
                }
                FID_IS_VALID_DER => {
                    let b = match blob_arg(&args, 0, "asn1_is_valid_der")? {
                        Some(b) => b,
                        None => return Ok(SqlValue::Integer(0)),
                    };
                    // Valid DER = parses AND re-encodes to the same
                    // bytes (DER's "canonical encoding" rule).
                    // simple_asn1 will accept some BER inputs that
                    // wouldn't be valid DER; this round-trip check
                    // catches them.
                    let valid = match from_der(&b) {
                        Ok(blocks) => {
                            let mut buf = Vec::with_capacity(b.len());
                            let mut ok = true;
                            for blk in &blocks {
                                match to_der(blk) {
                                    Ok(mut x) => buf.append(&mut x),
                                    Err(_) => {
                                        ok = false;
                                        break;
                                    }
                                }
                            }
                            ok && buf == b
                        }
                        Err(_) => false,
                    };
                    Ok(SqlValue::Integer(if valid { 1 } else { 0 }))
                }
                FID_TYPE_TAG => {
                    let b = match blob_arg(&args, 0, "asn1_type_tag")? {
                        Some(b) => b,
                        None => return Ok(SqlValue::Null),
                    };
                    Ok(match b.first() {
                        Some(byte) => SqlValue::Integer(*byte as i64),
                        None => SqlValue::Null,
                    })
                }
                FID_PRETTY => {
                    let b = match blob_arg(&args, 0, "asn1_pretty")? {
                        Some(b) => b,
                        None => return Ok(SqlValue::Null),
                    };
                    match decode_to_json(&b) {
                        Ok(v) => {
                            // serde_json::to_string_pretty  multi-line
                            // indented output. Caller asked for
                            // "debug-friendly".
                            let s = serde_json::to_string_pretty(&v)
                                .map_err(|e| format!("asn1_pretty: JSON: {e}"))?;
                            Ok(SqlValue::Text(s))
                        }
                        Err(_) => Ok(SqlValue::Null),
                    }
                }
                FID_VERSION => Ok(SqlValue::Text(format!(
                    "asn1 (simple_asn1 0.6); extension {}",
                    env!("CARGO_PKG_VERSION")
                ))),
                other => Err(format!("asn1: unknown func id {other}")),
            }
        }
    }

    bindings::export!(Ext with_types_in bindings);
}
