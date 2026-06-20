.load extensions/qrcode/target/wasm32-wasip2/release/qrcode_extension.component.wasm

/* ---- qr_svg: full SVG document, stripped XML prolog so it starts
 * with `<svg ` (PLAN-more-extensions-3.md #6 acceptance). */
SELECT substr(qr_svg('hello'), 1, 5);

/* SVG output is deterministic for a fixed (text, ecc) pair: same
 * input MUST produce the same byte-for-byte document, otherwise
 * we can't memoize results. */
SELECT qr_svg('hello') = qr_svg('hello');

/* ---- qr_unicode: terminal-friendly. Output is non-empty TEXT.
 * Exact bytes are sensitive to the qrcode crate's masking choice,
 * so we just assert the shape (non-empty + contains a unicode
 * block character). */
SELECT length(qr_unicode('hello')) > 0;
SELECT instr(qr_unicode('hello'), char(0x2588)) > 0;

/* ---- qr_size: modules-per-side. Smallest QR is 21x21 (Version 1).
 * 'hello' fits in version 1 at every ECC level. */
SELECT qr_size('hello');
SELECT qr_size('hello') >= 21;

/* ECC affects the smallest version that fits, so qr_size can step
 * up at higher ECC for the same input. L <= H by spec. */
SELECT qr_size('hello', 'L') <= qr_size('hello', 'H');

/* ---- qr_version_for: 1..40 for Normal versions. */
SELECT qr_version_for('hello');
SELECT qr_version_for('hello') BETWEEN 1 AND 40;

/* Monotonicity: a longer input should bump the version up
 * (5 chars -> 1, 50 chars -> >1) at constant ECC. */
SELECT qr_version_for(printf('%.50c', 'x'), 'M') > qr_version_for('hello', 'M');

/* ---- qr_modules: JSON 0/1 grid. Outer length should equal the
 * module-side reported by qr_size. We pick out the structure with
 * json_array_length to avoid eyeballing the entire 21x21 grid. */
SELECT json_array_length(qr_modules('hello'));
SELECT json_array_length(qr_modules('hello'), '$[0]');
SELECT json_array_length(qr_modules('hello')) = qr_size('hello');

/* The top-left finder pattern has a dark module at (0,0). */
SELECT json_extract(qr_modules('hello'), '$[0][0]');

/* ---- ECC argument parsing: case-insensitive, NULL = default M. */
SELECT qr_size('hello', 'l') = qr_size('hello', 'L');
SELECT qr_size('hello', NULL) = qr_size('hello');

/* Bad ECC label errors cleanly (caught by the harness as an
 * Error: line  we just want NOT to panic). */
SELECT qr_size('hello', 'X');

/* ---- NULL input propagates NULL across every variant. */
SELECT qr_svg(NULL) IS NULL;
SELECT qr_unicode(NULL) IS NULL;
SELECT qr_modules(NULL) IS NULL;
SELECT qr_size(NULL) IS NULL;
SELECT qr_version_for(NULL) IS NULL;

/* ---- INTEGER input is coerced to its TEXT form, matching the
 * convention used by sha3 / blake3 / hashes-fast. */
SELECT qr_size(42) = qr_size('42');

/* ---- Pairs with `totp.totp_url()`  the natural use-case is
 * QR'ing an otpauth:// URI. */
SELECT substr(qr_svg('otpauth://totp/Example:alice@example.com?secret=JBSWY3DPEHPK3PXP'), 1, 5);

/* ---- qrcode_version reports the upstream + extension version. */
SELECT length(qrcode_version()) > 0;
