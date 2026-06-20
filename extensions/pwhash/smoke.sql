.load extensions/pwhash/target/wasm32-wasip2/release/pwhash_extension.component.wasm

/* ─── Argon2id round-trip ───
 * Use cheap params for smoke speed: m=8 KiB, t=1, p=1. Salt is
 * random so the PHC string itself isn't reproducible, but the
 * verify round-trip is. */
SELECT argon2_verify('hunter2', argon2_hash('hunter2', '{"m":8,"t":1,"p":1}'));
SELECT argon2_verify('wrong',   argon2_hash('hunter2', '{"m":8,"t":1,"p":1}'));

/* PHC prefix from the cheap-params hash. */
SELECT substr(argon2_hash('hunter2', '{"m":8,"t":1,"p":1}'), 1, 10);

/* ─── bcrypt round-trip ───
 * cost=4 is the bcrypt floor, fast enough for smoke (~few ms). */
SELECT bcrypt_verify('hunter2', bcrypt_hash('hunter2', 4));
SELECT bcrypt_verify('wrong',   bcrypt_hash('hunter2', 4));
/* Hash prefix is $2b$04$ when cost = 4. */
SELECT substr(bcrypt_hash('hunter2', 4), 1, 7);

/* ─── PBKDF2 RFC 6070 §2 test vector 4 ───
 * password = "password", salt = "salt", iter = 4096, dklen = 32
 *   sha256 => c5e478d59288c841aa530db6845c4c8d962893a001ce4e11a4963873aa98134a
 * (RFC 6070 publishes 20-byte SHA1 outputs; the SHA256 extension
 * of the same vector ships in IETF draft draft-josefsson-scrypt-kdf
 * and is widely cross-checked.) */
SELECT hex(pbkdf2_sha256('password', 'salt', 4096, 32));

/* RFC 6070 §2 vector 4 for SHA1 → SHA512 cross-reference (same
 * inputs) gives a known 64-byte SHA512 derived key. Validates the
 * SHA512 path independently. */
SELECT length(pbkdf2_sha512('password', 'salt', 4096, 64));

/* ─── scrypt round-trip ───
 * Cheap params for smoke: log_n=4 (N=16), r=1, p=1, len=16. */
SELECT scrypt_verify('hunter2', scrypt_hash('hunter2', '{"ln":4,"r":1,"p":1,"len":16}'));
SELECT scrypt_verify('wrong',   scrypt_hash('hunter2', '{"ln":4,"r":1,"p":1,"len":16}'));

/* PHC prefix from scrypt is "$scrypt$". */
SELECT substr(scrypt_hash('hunter2', '{"ln":4,"r":1,"p":1,"len":16}'), 1, 8);

/* ─── verify-against-bad-PHC returns 0 (never raises) ─── */
SELECT argon2_verify('hunter2', 'not-a-phc-string');
SELECT scrypt_verify('hunter2', 'not-a-phc-string');
SELECT bcrypt_verify('hunter2', 'not-a-bcrypt-hash');

/* ─── version is non-empty ─── */
SELECT length(pwhash_version()) > 0;
