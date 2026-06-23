// Hash helper for the extension registry.
//
// The cli expects blake3-hex digests (`extension-digest` returns
// "blake3:" without the prefix in some paths, hex-only in others —
// see `wit/extension-loader.wit` and the `_capability_grants` table
// design in `host/PLAN-grants-db.md`). The cli's grant-pin lookup
// keys on (name, digest_hex); the BROWSER scenario doesn't ship a
// grants db, so the cli's grant gate is bypassed and we don't need
// a true blake3.
//
// We approximate with SHA-256 (WebCrypto, no dependency) and prefix
// "sha256:" so it's obvious in logs that this isn't a real blake3.
// If the browser scenario starts using grants, swap in a real blake3
// (e.g. the `@noble/hashes` package) here.

/**
 * Compute a hex digest over `bytes`. Returns the empty string for
 * undefined/empty input — matches the cli's "no digest" sentinel.
 *
 * @param {Uint8Array | ArrayBuffer} bytes
 * @returns {Promise<string>}
 */
export async function hashBlake3Hex(bytes) {
  if (!bytes || bytes.byteLength === 0) return ''
  const buf = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes)
  // WebCrypto digest works on ArrayBuffer or BufferSource; pass the
  // .buffer view to avoid the deprecated implicit copy in some hosts.
  const digest = await crypto.subtle.digest('SHA-256', buf)
  const arr = new Uint8Array(digest)
  let hex = ''
  for (let i = 0; i < arr.length; i++) {
    const b = arr[i].toString(16)
    hex += b.length === 1 ? '0' + b : b
  }
  // Prefix so downstream logs make the substitution visible.
  return 'sha256:' + hex
}
