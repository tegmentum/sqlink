//! Native host bridge for `sqlite:extension/s3-base`.
//!
//! Routes the WIT-described S3 surface (get/put/delete/head/list/
//! copy-object) into in-host machinery: aws-sigv4 builds the
//! canonical request + signature, reqwest's blocking client sends
//! it. This is an "in-host" implementation rather than a wasm
//! component composition because:
//!
//!   - s3-wasm (the original component) imports
//!     `wasi:http/outgoing-handler` and `aws:sigv4/{types,signer}`.
//!     Satisfying those would require wiring wasmtime-wasi-http +
//!     instantiating an additional aws-sigv4 component into every
//!     loaded-extension Store. The bookkeeping cost dwarfs the
//!     payoff for a single sink-style SPI.
//!   - The WIT contract (sqlite-loader-wit/wit/host-spi.wit) is
//!     mirrored exactly from s3-wasm's s3-base interface, so a
//!     future iteration can swap implementations without touching
//!     the extension-facing surface.
//!
//! Substrate for PLAN-wal-archive-extension.md (#440)  the wal-
//! archive extension's primary off-box sink.

use std::time::SystemTime;

use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4::SigningParams as V4SigningParams;

use crate::loaded::sqlite::extension::s3_base::{
    S3Credentials, S3EndpointConfig, S3Error, S3GetObjectOptions, S3GetObjectOutput,
    S3HeadObjectOutput, S3ListObjectsOptions, S3ListObjectsOutput, S3ObjectInfo, S3ObjectMetadata,
    S3PutObjectOptions, S3PutObjectOutput,
};

/// HTTP verbs we support against S3.
#[derive(Copy, Clone)]
enum S3Method {
    Get,
    Put,
    Delete,
    Head,
}

impl S3Method {
    fn as_str(self) -> &'static str {
        match self {
            S3Method::Get => "GET",
            S3Method::Put => "PUT",
            S3Method::Delete => "DELETE",
            S3Method::Head => "HEAD",
        }
    }
}

/// Build the URL + Host header for a bucket/key against the
/// configured endpoint. `path_style` selects between virtual-host
/// (bucket.host) and path (host/bucket/key) addressing.
///
/// Returns `(url, host_header_value)`.
fn build_url(
    endpoint: &S3EndpointConfig,
    bucket: &str,
    key: &str,
    query: &[(String, String)],
) -> Result<(String, String), S3Error> {
    let base = endpoint.url.trim_end_matches('/');
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return Err(S3Error::InvalidRequest(format!(
            "endpoint URL must begin with http:// or https://: {base:?}"
        )));
    }
    let scheme_end = base.find("://").unwrap() + 3;
    let host_part = &base[scheme_end..];
    let (host_only, path_prefix) = match host_part.find('/') {
        Some(i) => (&host_part[..i], &host_part[i..]),
        None => (host_part, ""),
    };

    // Build the URL path. S3 keys can contain `/`  forward them
    // through; reqwest's URL parser handles the percent-encoding
    // we need for safety on the query side. The key itself is left
    // as-is so non-ASCII / weird-byte keys still address correctly
    // (the SigV4 signing path canonicalizes anyway).
    let encoded_key = url::form_urlencoded::byte_serialize(key.as_bytes())
        .collect::<String>()
        // form_urlencoded turns `/` into `%2F`; restore them so
        // path navigation works against the upstream S3 layout.
        .replace("%2F", "/");

    let (full_url, host_header) = if endpoint.path_style {
        let path = if bucket.is_empty() {
            format!("{path_prefix}")
        } else if key.is_empty() {
            format!("{path_prefix}/{bucket}")
        } else {
            format!("{path_prefix}/{bucket}/{encoded_key}")
        };
        let url = format!("{}://{}{}", &base[..scheme_end - 3], host_only, path);
        (url, host_only.to_string())
    } else {
        // Virtual-host style.
        let host = if bucket.is_empty() {
            host_only.to_string()
        } else {
            format!("{bucket}.{host_only}")
        };
        let path = if key.is_empty() {
            format!("{path_prefix}/")
        } else {
            format!("{path_prefix}/{encoded_key}")
        };
        let url = format!("{}://{}{}", &base[..scheme_end - 3], host, path);
        (url, host)
    };

    let url_with_query = if query.is_empty() {
        full_url
    } else {
        let mut qs = String::new();
        for (i, (k, v)) in query.iter().enumerate() {
            if i > 0 {
                qs.push('&');
            }
            qs.push_str(&urlencode(k));
            if !v.is_empty() {
                qs.push('=');
                qs.push_str(&urlencode(v));
            }
        }
        format!("{full_url}?{qs}")
    };

    Ok((url_with_query, host_header))
}

fn urlencode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// Build + sign + send one S3 request via reqwest::blocking.
/// `extra_headers` carries the per-method overrides (Content-Type
/// for PUT, x-amz-copy-source for COPY, etc.). Returns the
/// response.
fn send_signed(
    method: S3Method,
    url: &str,
    host_header: &str,
    endpoint: &S3EndpointConfig,
    credentials: &S3Credentials,
    body: &[u8],
    extra_headers: &[(String, String)],
) -> Result<reqwest::blocking::Response, S3Error> {
    // Construct the SigV4 identity. aws-sigv4's signing path takes
    // ownership of strings for the credential scope, so allocate
    // once here.
    let identity: aws_credential_types::Credentials = aws_credential_types::Credentials::new(
        credentials.access_key_id.clone(),
        credentials.secret_access_key.clone(),
        credentials.session_token.clone(),
        None,
        "sqlink-host",
    );
    let identity_ref = identity.into();

    let mut settings = SigningSettings::default();
    // S3 ALWAYS uses unsigned-payload OR a sha256-of-payload header.
    // Use the latter so the signature matches; aws-sigv4 computes
    // the body hash for us when given the SignableBody::Bytes path.
    settings.payload_checksum_kind = aws_sigv4::http_request::PayloadChecksumKind::XAmzSha256;

    let signing_params: V4SigningParams<'_, SigningSettings> = V4SigningParams::builder()
        .identity(&identity_ref)
        .region(&endpoint.region)
        .name("s3")
        .time(SystemTime::now())
        .settings(settings)
        .build()
        .map_err(|e| S3Error::Internal(format!("sigv4 params: {e}")))?;

    // Build the headers list for signing.
    let mut headers_for_signing: Vec<(String, String)> =
        Vec::with_capacity(extra_headers.len() + 1);
    headers_for_signing.push(("host".to_string(), host_header.to_string()));
    for (k, v) in extra_headers {
        headers_for_signing.push((k.clone(), v.clone()));
    }

    let signable = SignableRequest::new(
        method.as_str(),
        url,
        headers_for_signing
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str())),
        SignableBody::Bytes(body),
    )
    .map_err(|e| S3Error::Internal(format!("sigv4 signable: {e}")))?;

    let (signing_instructions, _signature) = sign(signable, &signing_params.into())
        .map_err(|e| S3Error::Internal(format!("sigv4 sign: {e}")))?
        .into_parts();

    // Apply the sigv4 instructions on top of a reqwest::blocking
    // request. The instructions yield typed Header values + query
    // params we need to inject; the body stays as-is.
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| S3Error::NetworkError(format!("reqwest client: {e}")))?;

    let mut builder = match method {
        S3Method::Get => client.get(url),
        S3Method::Put => client.put(url),
        S3Method::Delete => client.delete(url),
        S3Method::Head => client.head(url),
    };
    builder = builder.header("host", host_header);
    for (k, v) in extra_headers {
        builder = builder.header(k.as_str(), v.as_str());
    }
    let (signed_headers, _signed_query) = signing_instructions.into_parts();
    for header in signed_headers {
        builder = builder.header(header.name(), header.value());
    }
    if matches!(method, S3Method::Put) {
        builder = builder.body(body.to_vec());
    }
    builder
        .send()
        .map_err(|e| S3Error::NetworkError(format!("reqwest send: {e}")))
}

/// Translate a raw HTTP status into the appropriate S3Error variant.
/// 2xx returns Ok(()); 4xx/5xx pick a meaningful S3Error.
fn check_status(status: u16, body_preview: &str) -> Result<(), S3Error> {
    if (200..300).contains(&status) {
        return Ok(());
    }
    match status {
        403 => Err(S3Error::AccessDenied),
        404 => {
            // Distinguish NoSuchBucket vs NoSuchKey by the body
            // text if the upstream sent the S3-style error code.
            if body_preview.contains("NoSuchBucket") {
                Err(S3Error::NoSuchBucket)
            } else {
                Err(S3Error::NoSuchKey)
            }
        }
        400 => {
            if body_preview.contains("InvalidBucketName") {
                Err(S3Error::InvalidBucketName)
            } else {
                Err(S3Error::InvalidRequest(format!("HTTP 400: {body_preview}")))
            }
        }
        _ => Err(S3Error::Internal(format!("HTTP {status}: {body_preview}"))),
    }
}

/// Pull the standard S3 metadata headers off a response into the
/// WIT-defined `S3ObjectMetadata`. `custom` carries any
/// `x-amz-meta-*` headers (without the prefix).
fn extract_metadata(headers: &reqwest::header::HeaderMap) -> S3ObjectMetadata {
    let h = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
    };
    let content_length = headers
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());
    let last_modified = headers
        .get("last-modified")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_http_date);
    let custom = headers
        .iter()
        .filter_map(|(k, v)| {
            let key = k.as_str();
            key.strip_prefix("x-amz-meta-").and_then(|name| {
                v.to_str()
                    .ok()
                    .map(|val| (name.to_string(), val.to_string()))
            })
        })
        .collect();
    S3ObjectMetadata {
        content_type: h("content-type"),
        content_length,
        etag: h("etag").map(|s| s.trim_matches('"').to_string()),
        last_modified,
        custom,
    }
}

/// Parse RFC 1123 dates into Unix epoch seconds. Returns None on
/// any parse failure  S3 sometimes returns subtly off-format
/// dates and we'd rather expose None than panic.
fn parse_http_date(s: &str) -> Option<u64> {
    // Minimal hand-roll: "Tue, 15 Nov 1994 08:12:31 GMT". Use
    // httpdate-style approach via a manual sscanf. We don't want
    // a chrono dep here; httpdate is small.
    httpdate_parse(s)
}

/// Tiny RFC 1123 / 850 date parser. Returns Unix epoch seconds on
/// success. Implemented inline to avoid pulling chrono /
/// jiff / time.
fn httpdate_parse(s: &str) -> Option<u64> {
    // "Tue, 15 Nov 1994 08:12:31 GMT"
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 6 {
        return None;
    }
    let day: u32 = parts[1].parse().ok()?;
    let month = match parts[2] {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: i32 = parts[3].parse().ok()?;
    let hms: Vec<&str> = parts[4].split(':').collect();
    if hms.len() != 3 {
        return None;
    }
    let hour: u32 = hms[0].parse().ok()?;
    let minute: u32 = hms[1].parse().ok()?;
    let second: u32 = hms[2].parse().ok()?;
    // Days from civil (proleptic Gregorian) reference: Howard
    // Hinnant's algorithm. Reasonably compact.
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y / 400 } else { (y - 399) / 400 };
    let yoe = (y - era * 400) as u32;
    let month_i: i32 = month as i32 + if month > 2 { -3 } else { 9 };
    let doy = (153 * month_i as u32 + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days_since_epoch = (era * 146097 + doe as i32) - 719468;
    let secs =
        days_since_epoch as i64 * 86400 + hour as i64 * 3600 + minute as i64 * 60 + second as i64;
    if secs < 0 {
        None
    } else {
        Some(secs as u64)
    }
}

/// Parse the body of a `ListObjectsV2` response into the WIT-
/// defined `S3ListObjectsOutput`. Uses a minimal hand-roll XML
/// reader  good enough for the test fixtures s3s-fs produces +
/// the AWS reference shape. Returns InvalidRequest on parse
/// failure so the caller sees a typed error.
fn parse_list_response(body: &str) -> Result<S3ListObjectsOutput, S3Error> {
    let mut objects = Vec::new();
    let mut common_prefixes = Vec::new();
    let mut next_continuation_token: Option<String> = None;
    let mut is_truncated = false;

    // Pull all <Contents>...</Contents> blocks.
    for block in xml_blocks(body, "Contents") {
        let key = xml_tag(&block, "Key").unwrap_or_default();
        let size: u64 = xml_tag(&block, "Size")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let etag = xml_tag(&block, "ETag").map(|s| s.trim_matches('"').to_string());
        let last_modified = xml_tag(&block, "LastModified").and_then(|s| iso8601_to_epoch(&s));
        let storage_class = xml_tag(&block, "StorageClass");
        objects.push(S3ObjectInfo {
            key,
            size,
            etag,
            last_modified,
            storage_class,
        });
    }
    for block in xml_blocks(body, "CommonPrefixes") {
        if let Some(p) = xml_tag(&block, "Prefix") {
            common_prefixes.push(p);
        }
    }
    if let Some(s) = xml_tag(body, "IsTruncated") {
        is_truncated = s.eq_ignore_ascii_case("true");
    }
    if let Some(s) = xml_tag(body, "NextContinuationToken") {
        next_continuation_token = Some(s);
    }
    Ok(S3ListObjectsOutput {
        objects,
        common_prefixes,
        next_continuation_token,
        is_truncated,
    })
}

/// Yield every `<tag>...</tag>` block in `body` as a slice.
fn xml_blocks<'a>(body: &'a str, tag: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut cursor = 0;
    while let Some(start) = body[cursor..].find(&open) {
        let abs_start = cursor + start + open.len();
        if let Some(end) = body[abs_start..].find(&close) {
            let abs_end = abs_start + end;
            out.push(&body[abs_start..abs_end]);
            cursor = abs_end + close.len();
        } else {
            break;
        }
    }
    out
}

fn xml_tag(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)?;
    Some(body[start..start + end].to_string())
}

/// Parse `2024-01-15T08:12:31.000Z` into Unix epoch seconds. Tries
/// both with and without fractional seconds.
fn iso8601_to_epoch(s: &str) -> Option<u64> {
    // Strip fractional seconds + trailing Z.
    let s = s.trim_end_matches('Z');
    let (date_part, time_part) = s.split_once('T')?;
    let date_bits: Vec<&str> = date_part.split('-').collect();
    if date_bits.len() != 3 {
        return None;
    }
    let year: i32 = date_bits[0].parse().ok()?;
    let month: u32 = date_bits[1].parse().ok()?;
    let day: u32 = date_bits[2].parse().ok()?;
    let time_clean = time_part.split('.').next().unwrap_or(time_part);
    let time_bits: Vec<&str> = time_clean.split(':').collect();
    if time_bits.len() != 3 {
        return None;
    }
    let hour: u32 = time_bits[0].parse().ok()?;
    let minute: u32 = time_bits[1].parse().ok()?;
    let second: u32 = time_bits[2].parse().ok()?;
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y / 400 } else { (y - 399) / 400 };
    let yoe = (y - era * 400) as u32;
    let month_i: i32 = month as i32 + if month > 2 { -3 } else { 9 };
    let doy = (153 * month_i as u32 + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days_since_epoch = (era * 146097 + doe as i32) - 719468;
    let secs =
        days_since_epoch as i64 * 86400 + hour as i64 * 3600 + minute as i64 * 60 + second as i64;
    if secs < 0 {
        None
    } else {
        Some(secs as u64)
    }
}

// ----------------------------------------------------------------
// Top-level operations  what the s3_base::Host trait dispatches.
// ----------------------------------------------------------------

pub(crate) fn op_get_object(
    endpoint: S3EndpointConfig,
    credentials: S3Credentials,
    bucket: String,
    key: String,
    options: Option<S3GetObjectOptions>,
) -> Result<S3GetObjectOutput, S3Error> {
    let (url, host) = build_url(&endpoint, &bucket, &key, &[])?;
    let mut extras = Vec::new();
    if let Some(opts) = options {
        if let Some((start, end)) = opts.range {
            extras.push(("range".to_string(), format!("bytes={start}-{end}")));
        }
        if let Some(if_match) = opts.if_match {
            extras.push(("if-match".to_string(), if_match));
        }
        if let Some(if_none) = opts.if_none_match {
            extras.push(("if-none-match".to_string(), if_none));
        }
    }
    let resp = send_signed(
        S3Method::Get,
        &url,
        &host,
        &endpoint,
        &credentials,
        &[],
        &extras,
    )?;
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let body = resp
        .bytes()
        .map_err(|e| S3Error::NetworkError(format!("read body: {e}")))?
        .to_vec();
    if !(200..300).contains(&status) {
        let preview = String::from_utf8_lossy(&body)
            .chars()
            .take(512)
            .collect::<String>();
        check_status(status, &preview)?;
    }
    let metadata = extract_metadata(&headers);
    Ok(S3GetObjectOutput { body, metadata })
}

pub(crate) fn op_put_object(
    endpoint: S3EndpointConfig,
    credentials: S3Credentials,
    bucket: String,
    key: String,
    body: Vec<u8>,
    options: Option<S3PutObjectOptions>,
) -> Result<S3PutObjectOutput, S3Error> {
    let (url, host) = build_url(&endpoint, &bucket, &key, &[])?;
    let mut extras = Vec::new();
    if let Some(opts) = options {
        if let Some(ct) = opts.content_type {
            extras.push(("content-type".to_string(), ct));
        }
        if let Some(cc) = opts.cache_control {
            extras.push(("cache-control".to_string(), cc));
        }
        for (k, v) in opts.metadata {
            extras.push((format!("x-amz-meta-{k}"), v));
        }
    }
    let resp = send_signed(
        S3Method::Put,
        &url,
        &host,
        &endpoint,
        &credentials,
        &body,
        &extras,
    )?;
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let body_bytes = resp
        .bytes()
        .map_err(|e| S3Error::NetworkError(format!("read body: {e}")))?
        .to_vec();
    if !(200..300).contains(&status) {
        let preview = String::from_utf8_lossy(&body_bytes)
            .chars()
            .take(512)
            .collect::<String>();
        check_status(status, &preview)?;
    }
    let etag = headers
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim_matches('"').to_string())
        .unwrap_or_default();
    Ok(S3PutObjectOutput { etag })
}

pub(crate) fn op_delete_object(
    endpoint: S3EndpointConfig,
    credentials: S3Credentials,
    bucket: String,
    key: String,
) -> Result<(), S3Error> {
    let (url, host) = build_url(&endpoint, &bucket, &key, &[])?;
    let resp = send_signed(
        S3Method::Delete,
        &url,
        &host,
        &endpoint,
        &credentials,
        &[],
        &[],
    )?;
    let status = resp.status().as_u16();
    let body = resp
        .bytes()
        .map_err(|e| S3Error::NetworkError(format!("read body: {e}")))?
        .to_vec();
    if !(200..300).contains(&status) {
        let preview = String::from_utf8_lossy(&body)
            .chars()
            .take(512)
            .collect::<String>();
        check_status(status, &preview)?;
    }
    Ok(())
}

pub(crate) fn op_head_object(
    endpoint: S3EndpointConfig,
    credentials: S3Credentials,
    bucket: String,
    key: String,
) -> Result<S3HeadObjectOutput, S3Error> {
    let (url, host) = build_url(&endpoint, &bucket, &key, &[])?;
    let resp = send_signed(
        S3Method::Head,
        &url,
        &host,
        &endpoint,
        &credentials,
        &[],
        &[],
    )?;
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    if !(200..300).contains(&status) {
        check_status(status, "")?;
    }
    Ok(S3HeadObjectOutput {
        metadata: extract_metadata(&headers),
    })
}

pub(crate) fn op_list_objects(
    endpoint: S3EndpointConfig,
    credentials: S3Credentials,
    bucket: String,
    options: Option<S3ListObjectsOptions>,
) -> Result<S3ListObjectsOutput, S3Error> {
    // S3 list-objects-v2: bucket-level URL with query string params.
    let mut query = vec![("list-type".to_string(), "2".to_string())];
    if let Some(opts) = options {
        if let Some(p) = opts.prefix {
            query.push(("prefix".to_string(), p));
        }
        if let Some(d) = opts.delimiter {
            query.push(("delimiter".to_string(), d));
        }
        if let Some(m) = opts.max_keys {
            query.push(("max-keys".to_string(), m.to_string()));
        }
        if let Some(t) = opts.continuation_token {
            query.push(("continuation-token".to_string(), t));
        }
    }
    let (url, host) = build_url(&endpoint, &bucket, "", &query)?;
    let resp = send_signed(
        S3Method::Get,
        &url,
        &host,
        &endpoint,
        &credentials,
        &[],
        &[],
    )?;
    let status = resp.status().as_u16();
    let body_bytes = resp
        .bytes()
        .map_err(|e| S3Error::NetworkError(format!("read body: {e}")))?
        .to_vec();
    let body_str = String::from_utf8_lossy(&body_bytes).into_owned();
    if !(200..300).contains(&status) {
        check_status(status, &body_str)?;
    }
    parse_list_response(&body_str)
}

pub(crate) fn op_copy_object(
    endpoint: S3EndpointConfig,
    credentials: S3Credentials,
    source_bucket: String,
    source_key: String,
    dest_bucket: String,
    dest_key: String,
) -> Result<S3PutObjectOutput, S3Error> {
    let (url, host) = build_url(&endpoint, &dest_bucket, &dest_key, &[])?;
    let copy_source = format!("/{source_bucket}/{source_key}");
    let extras = vec![("x-amz-copy-source".to_string(), copy_source)];
    let resp = send_signed(
        S3Method::Put,
        &url,
        &host,
        &endpoint,
        &credentials,
        &[],
        &extras,
    )?;
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let body_bytes = resp
        .bytes()
        .map_err(|e| S3Error::NetworkError(format!("read body: {e}")))?
        .to_vec();
    if !(200..300).contains(&status) {
        let preview = String::from_utf8_lossy(&body_bytes)
            .chars()
            .take(512)
            .collect::<String>();
        check_status(status, &preview)?;
    }
    // The CopyObject response carries a <CopyObjectResult><ETag>...
    // body. Pull the etag out of there if the headers didn't carry
    // one (S3 returns it both places).
    let body_str = String::from_utf8_lossy(&body_bytes);
    let etag = headers
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim_matches('"').to_string())
        .or_else(|| xml_tag(&body_str, "ETag").map(|s| s.trim_matches('"').to_string()))
        .unwrap_or_default();
    Ok(S3PutObjectOutput { etag })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_url_path_style() {
        let ep = S3EndpointConfig {
            url: "http://localhost:9000".to_string(),
            region: "us-east-1".to_string(),
            path_style: true,
        };
        let (url, host) = build_url(&ep, "mybucket", "foo/bar.txt", &[]).unwrap();
        assert_eq!(url, "http://localhost:9000/mybucket/foo/bar.txt");
        assert_eq!(host, "localhost:9000");
    }

    #[test]
    fn build_url_virtual_host() {
        let ep = S3EndpointConfig {
            url: "https://s3.amazonaws.com".to_string(),
            region: "us-east-1".to_string(),
            path_style: false,
        };
        let (url, host) = build_url(&ep, "mybucket", "foo.txt", &[]).unwrap();
        assert_eq!(url, "https://mybucket.s3.amazonaws.com/foo.txt");
        assert_eq!(host, "mybucket.s3.amazonaws.com");
    }

    #[test]
    fn xml_helpers() {
        let body = "<a><Key>k1</Key><Size>10</Size></a><a><Key>k2</Key><Size>20</Size></a>";
        let blocks = xml_blocks(body, "a");
        assert_eq!(blocks.len(), 2);
        assert_eq!(xml_tag(blocks[0], "Key").as_deref(), Some("k1"));
        assert_eq!(xml_tag(blocks[1], "Size").as_deref(), Some("20"));
    }

    #[test]
    fn iso8601_round_trip() {
        // 2024-01-15T08:12:31Z = 1705306351
        let v = iso8601_to_epoch("2024-01-15T08:12:31Z").unwrap();
        assert_eq!(v, 1705306351);
    }

    #[test]
    fn httpdate_basic() {
        // "Mon, 15 Jan 2024 08:12:31 GMT"  same epoch.
        let v = httpdate_parse("Mon, 15 Jan 2024 08:12:31 GMT").unwrap();
        assert_eq!(v, 1705306351);
    }
}
