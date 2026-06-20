.load extensions/http-signature/target/wasm32-wasip2/release/http_signature_extension.component.wasm

/* ─── http_sig_input: the signature-input header value ───
 * Just the params list + sf-dict params; this is what goes in the
 * Signature-Input HTTP header per RFC 9421 §3. serde_json (no
 * preserve_order feature) sorts param keys alphabetically, so the
 * stable ordering here is alg / created / keyid even though the
 * JSON spec lists them in a different order. */
SELECT http_sig_input(
  'POST',
  '/foo',
  '{"host":"example.com","date":"Tue, 20 Apr 2021 02:07:55 GMT"}',
  '{"components":["@method","@path","host","date"],"params":{"created":1700000000,"keyid":"test-key","alg":"hmac-sha256"}}'
);

/* ─── http_sig_base: canonical signature base ───
 * The base contains literal newlines (RFC 9421 §2.5). The CLI's
 * default list mode would split them into separate result rows,
 * so we replace LF with '|' to keep it one line. */
SELECT replace(
  http_sig_base(
    'POST',
    '/foo',
    '{"host":"example.com","date":"Tue, 20 Apr 2021 02:07:55 GMT"}',
    '{"components":["@method","@path","host","date"],"params":{"created":1700000000,"keyid":"test-key","alg":"hmac-sha256"}}'
  ),
  char(10), '|'
);

/* ─── HMAC-SHA256 sign + verify round-trip ───
 * key = 'shared-secret'. The signature is computed over the
 * canonical base from the previous SELECT. */
SELECT http_sig_sign_hmac(
  http_sig_base(
    'POST', '/foo',
    '{"host":"example.com","date":"Tue, 20 Apr 2021 02:07:55 GMT"}',
    '{"components":["@method","@path","host","date"],"params":{"created":1700000000,"keyid":"test-key","alg":"hmac-sha256"}}'
  ),
  'shared-secret',
  'hmac-sha256'
);

SELECT http_sig_verify_hmac(
  http_sig_base(
    'POST', '/foo',
    '{"host":"example.com","date":"Tue, 20 Apr 2021 02:07:55 GMT"}',
    '{"components":["@method","@path","host","date"],"params":{"created":1700000000,"keyid":"test-key","alg":"hmac-sha256"}}'
  ),
  'QGpGWGqH/WYroqwzcVk4dfY0jRt67uAIZU7vRpesZUY=',
  'shared-secret',
  'hmac-sha256'
);

/* Wrong key → 0. */
SELECT http_sig_verify_hmac(
  http_sig_base(
    'POST', '/foo',
    '{"host":"example.com","date":"Tue, 20 Apr 2021 02:07:55 GMT"}',
    '{"components":["@method","@path","host","date"],"params":{"created":1700000000,"keyid":"test-key","alg":"hmac-sha256"}}'
  ),
  'QGpGWGqH/WYroqwzcVk4dfY0jRt67uAIZU7vRpesZUY=',
  'wrong-secret',
  'hmac-sha256'
);

/* ─── HMAC-SHA512 round-trip ─── */
SELECT http_sig_sign_hmac(
  http_sig_base(
    'POST', '/foo',
    '{"host":"example.com","date":"Tue, 20 Apr 2021 02:07:55 GMT"}',
    '{"components":["@method","@path","host","date"],"params":{"created":1700000000,"keyid":"test-key","alg":"hmac-sha256"}}'
  ),
  'shared-secret',
  'hmac-sha512'
);

/* ─── Ed25519 sign + verify ───
 * RFC 8037 test seed / public key. The signing alg is part of
 * the message (it's in @signature-params), but Ed25519 ignores
 * the alg label — the sig is deterministic over the base bytes. */
SELECT http_sig_sign_ed25519(
  http_sig_base(
    'POST', '/foo',
    '{"host":"example.com","date":"Tue, 20 Apr 2021 02:07:55 GMT"}',
    '{"components":["@method","@path","host","date"],"params":{"created":1700000000,"keyid":"test-key","alg":"hmac-sha256"}}'
  ),
  X'9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60'
);

SELECT http_sig_verify_ed25519(
  http_sig_base(
    'POST', '/foo',
    '{"host":"example.com","date":"Tue, 20 Apr 2021 02:07:55 GMT"}',
    '{"components":["@method","@path","host","date"],"params":{"created":1700000000,"keyid":"test-key","alg":"hmac-sha256"}}'
  ),
  '8h/Xqac9WO/g7Ow7KkhLo1xhptemZuRwEEkBDRMZzMVepQzWKReJ13KXRpvwatZe0IgDmaVf/N0P2kTcny4vCA==',
  X'd75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a'
);

/* Wrong pubkey (flipped last byte) → 0. */
SELECT http_sig_verify_ed25519(
  http_sig_base(
    'POST', '/foo',
    '{"host":"example.com","date":"Tue, 20 Apr 2021 02:07:55 GMT"}',
    '{"components":["@method","@path","host","date"],"params":{"created":1700000000,"keyid":"test-key","alg":"hmac-sha256"}}'
  ),
  '8h/Xqac9WO/g7Ow7KkhLo1xhptemZuRwEEkBDRMZzMVepQzWKReJ13KXRpvwatZe0IgDmaVf/N0P2kTcny4vCA==',
  X'd75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511b'
);

/* ─── header value normalization ───
 * Leading + trailing OWS stripped, internal runs collapsed to a
 * single SP (RFC 9421 §2.1). Verifies via the visible base string. */
SELECT replace(
  http_sig_base(
    'GET', '/r',
    '{"x-test":"  multi   space\t value  "}',
    '{"components":["x-test"],"params":{}}'
  ),
  char(10), '|'
);

/* ─── @path strips query string ───
 * Query lives in @query (separately requestable). */
SELECT replace(
  http_sig_base(
    'GET', '/items?id=42&sort=asc',
    '{}',
    '{"components":["@method","@path","@query"],"params":{}}'
  ),
  char(10), '|'
);

/* ─── case-insensitive header lookup ───
 * Component is lowercase "date" but the JSON map has "Date". */
SELECT replace(
  http_sig_base(
    'GET', '/',
    '{"Date":"Mon, 01 Jan 2024 00:00:00 GMT"}',
    '{"components":["date"],"params":{}}'
  ),
  char(10), '|'
);

/* Unknown alg on verify → 0 (not an error). */
SELECT http_sig_verify_hmac('anything', 'YW55', 'k', 'hmac-sha999');

/* Bad base64 sig → 0 (not an error). */
SELECT http_sig_verify_hmac('anything', '!!!not-b64!!!', 'k', 'hmac-sha256');

/* Version is non-empty. */
SELECT length(http_sig_version()) > 0;
