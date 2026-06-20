.load extensions/oauth-pkce/target/wasm32-wasip2/release/oauth_pkce_extension.component.wasm

/* ─── RFC 7636 §4.6 / Appendix B acceptance vector ───
 * The canonical PKCE S256 test pair from the RFC itself:
 *   verifier  = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
 *   challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM" */
SELECT pkce_challenge_s256('dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk');

/* "plain" method: challenge is the verifier itself (RFC 7636 §4.4). */
SELECT pkce_challenge_plain('dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk');

/* SHA-256 output is 32 bytes  base64url-no-pad encoded as 43 chars. */
SELECT length(pkce_challenge_s256('dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk'));

/* Empty-verifier corner: SHA256("")  base64url-no-pad. The output
 * shape is fixed by the digest size, not the input. */
SELECT pkce_challenge_s256('');

/* ─── Verifier length ───
 * Default = 32 random bytes  43 base64url-no-pad chars (RFC 7636
 * §4.1 recommended minimum). */
SELECT length(pkce_verifier());

/* Explicit 32 bytes = 43 chars. */
SELECT length(pkce_verifier(32));

/* 48 bytes  64 chars (every 3 raw  4 encoded; 48 is divisible). */
SELECT length(pkce_verifier(48));

/* 96 bytes  128 chars (the RFC 7636 §4.1 maximum). */
SELECT length(pkce_verifier(96));

/* Charset check: the verifier must contain only base64url-unreserved
 * chars (A-Z / a-z / 0-9 / '-' / '_'). After stripping the two
 * non-alnum members ('_' and '-'), what's left must be only alnum;
 * GLOB '*[^A-Za-z0-9]*' (SQLite uses '^' for char-class negation,
 * not '!') should match nothing. (NOT match = 1.) */
SELECT NOT (replace(replace(pkce_verifier(), '_', ''), '-', '')
            GLOB '*[^A-Za-z0-9]*');

/* Round-trip: feed the generated verifier into the S256 transform.
 * Output is fixed-shape (43 chars) regardless of the random input. */
SELECT length(pkce_challenge_s256(pkce_verifier(32)));

/* ─── version ─── */
SELECT length(pkce_version()) > 0;
