.load extensions/webauthn/target/wasm32-wasip2/release/webauthn_extension.component.wasm

/* ─── webauthn_version() ─── */
SELECT length(webauthn_version()) > 0;

/* ─── webauthn_register_options(): produced JSON has the right shape.
 * The challenge is random so we assert structure, not the value. */
SELECT json_extract(
  webauthn_register_options('example.com','Example RP', X'0102030405', 'alice', 'Alice'),
  '$.rp.id'
);
SELECT json_extract(
  webauthn_register_options('example.com','Example RP', X'0102030405', 'alice', 'Alice'),
  '$.user.name'
);
SELECT json_extract(
  webauthn_register_options('example.com','Example RP', X'0102030405', 'alice', 'Alice'),
  '$.attestation'
);
/* pubKeyCredParams: 3 entries (ES256, EdDSA, RS256). */
SELECT json_array_length(json_extract(
  webauthn_register_options('example.com','Example RP', X'0102030405', 'alice', 'Alice'),
  '$.pubKeyCredParams'
));
/* challenge is a base64url string  43 chars for 32 raw bytes (no padding). */
SELECT length(json_extract(
  webauthn_register_options('example.com','Example RP', X'0102030405', 'alice', 'Alice'),
  '$.challenge'
)) >= 43;

/* ─── webauthn_auth_options(): two-arity (default UV=preferred). */
SELECT json_extract(
  webauthn_auth_options('example.com', '[]'),
  '$.rpId'
);
SELECT json_extract(
  webauthn_auth_options('example.com', '[]'),
  '$.userVerification'
);
/* Three-arity override. */
SELECT json_extract(
  webauthn_auth_options('example.com', '[]', 'required'),
  '$.userVerification'
);
/* allowCredentials passthrough. */
SELECT json_array_length(json_extract(
  webauthn_auth_options('example.com',
    '[{"type":"public-key","id":"AQIDBA","transports":["usb"]}]'),
  '$.allowCredentials'
));

/* ─── verify_registration: garbage -> NULL. */
SELECT webauthn_verify_registration('{}', '{}', 'example.com') IS NULL;
SELECT webauthn_verify_registration(
  '{"challenge":"AAAA","user":{"id":"dXNlcjE"}}',
  '{"response":{"clientDataJSON":"AAAA","attestationObject":"AAAA"}}',
  'example.com') IS NULL;

/* ─── verify_authentication: garbage -> NULL. */
SELECT webauthn_verify_authentication('{}', '{}', '{}', 0) IS NULL;

/* ─── End-to-end EdDSA AUTHENTICATION round-trip ───
 * Vectors generated from the RFC 8037 Ed25519 seed
 * 9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60
 * with rpId=example.com, origin=https://example.com, fixed
 * challenge "deterministictestchallenge012345". The 64-byte
 * Ed25519 signature is deterministic so this vector is stable
 * across runs / platforms. */
SELECT json_extract(
  webauthn_verify_authentication(
    '{"challenge":"ZGV0ZXJtaW5pc3RpY3Rlc3RjaGFsbGVuZ2UwMTIzNDU","rpId":"example.com","allowCredentials":[],"userVerification":"preferred"}',
    '{"id":"test-credential-id","rawId":"test-credential-id","type":"public-key","response":{"clientDataJSON":"eyJ0eXBlIjoid2ViYXV0aG4uZ2V0IiwiY2hhbGxlbmdlIjoiWkdWMFpYSnRhVzVwYzNScFkzUmxjM1JqYUdGc2JHVnVaMlV3TVRJek5EVSIsIm9yaWdpbiI6Imh0dHBzOi8vZXhhbXBsZS5jb20ifQ","authenticatorData":"o3mm9u6vuaVeN4wRgDTidR5oL6ufLTCrE9ISVYbOGUcFAAAAAQ","signature":"740vqfLclv7QCTmONhcv8vg2lG3dRhDYa7sQefUlnmPi8zBc9Kn4AZxyDklUp72EGl9X6L4BrGMNpLI9w3R8Cw"}}',
    '{"alg":"EdDSA","kty":"OKP","crv":"Ed25519","x":"11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo"}',
    0),
  '$.new_sign_count');

/* user_verified flag (UV bit set in flags). */
SELECT json_extract(
  webauthn_verify_authentication(
    '{"challenge":"ZGV0ZXJtaW5pc3RpY3Rlc3RjaGFsbGVuZ2UwMTIzNDU","rpId":"example.com","allowCredentials":[],"userVerification":"preferred"}',
    '{"id":"test-credential-id","rawId":"test-credential-id","type":"public-key","response":{"clientDataJSON":"eyJ0eXBlIjoid2ViYXV0aG4uZ2V0IiwiY2hhbGxlbmdlIjoiWkdWMFpYSnRhVzVwYzNScFkzUmxjM1JqYUdGc2JHVnVaMlV3TVRJek5EVSIsIm9yaWdpbiI6Imh0dHBzOi8vZXhhbXBsZS5jb20ifQ","authenticatorData":"o3mm9u6vuaVeN4wRgDTidR5oL6ufLTCrE9ISVYbOGUcFAAAAAQ","signature":"740vqfLclv7QCTmONhcv8vg2lG3dRhDYa7sQefUlnmPi8zBc9Kn4AZxyDklUp72EGl9X6L4BrGMNpLI9w3R8Cw"}}',
    '{"alg":"EdDSA","kty":"OKP","crv":"Ed25519","x":"11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo"}',
    0),
  '$.user_verified');

/* Wrong challenge in options -> NULL. */
SELECT webauthn_verify_authentication(
    '{"challenge":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","rpId":"example.com","allowCredentials":[],"userVerification":"preferred"}',
    '{"id":"test-credential-id","rawId":"test-credential-id","type":"public-key","response":{"clientDataJSON":"eyJ0eXBlIjoid2ViYXV0aG4uZ2V0IiwiY2hhbGxlbmdlIjoiWkdWMFpYSnRhVzVwYzNScFkzUmxjM1JqYUdGc2JHVnVaMlV3TVRJek5EVSIsIm9yaWdpbiI6Imh0dHBzOi8vZXhhbXBsZS5jb20ifQ","authenticatorData":"o3mm9u6vuaVeN4wRgDTidR5oL6ufLTCrE9ISVYbOGUcFAAAAAQ","signature":"740vqfLclv7QCTmONhcv8vg2lG3dRhDYa7sQefUlnmPi8zBc9Kn4AZxyDklUp72EGl9X6L4BrGMNpLI9w3R8Cw"}}',
    '{"alg":"EdDSA","kty":"OKP","crv":"Ed25519","x":"11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo"}',
    0) IS NULL;

/* Wrong rpId in options -> NULL (sha256 hash mismatch). */
SELECT webauthn_verify_authentication(
    '{"challenge":"ZGV0ZXJtaW5pc3RpY3Rlc3RjaGFsbGVuZ2UwMTIzNDU","rpId":"attacker.com","allowCredentials":[],"userVerification":"preferred"}',
    '{"id":"test-credential-id","rawId":"test-credential-id","type":"public-key","response":{"clientDataJSON":"eyJ0eXBlIjoid2ViYXV0aG4uZ2V0IiwiY2hhbGxlbmdlIjoiWkdWMFpYSnRhVzVwYzNScFkzUmxjM1JqYUdGc2JHVnVaMlV3TVRJek5EVSIsIm9yaWdpbiI6Imh0dHBzOi8vZXhhbXBsZS5jb20ifQ","authenticatorData":"o3mm9u6vuaVeN4wRgDTidR5oL6ufLTCrE9ISVYbOGUcFAAAAAQ","signature":"740vqfLclv7QCTmONhcv8vg2lG3dRhDYa7sQefUlnmPi8zBc9Kn4AZxyDklUp72EGl9X6L4BrGMNpLI9w3R8Cw"}}',
    '{"alg":"EdDSA","kty":"OKP","crv":"Ed25519","x":"11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo"}',
    0) IS NULL;

/* Wrong public key (last char flipped) -> NULL (sig verify fails). */
SELECT webauthn_verify_authentication(
    '{"challenge":"ZGV0ZXJtaW5pc3RpY3Rlc3RjaGFsbGVuZ2UwMTIzNDU","rpId":"example.com","allowCredentials":[],"userVerification":"preferred"}',
    '{"id":"test-credential-id","rawId":"test-credential-id","type":"public-key","response":{"clientDataJSON":"eyJ0eXBlIjoid2ViYXV0aG4uZ2V0IiwiY2hhbGxlbmdlIjoiWkdWMFpYSnRhVzVwYzNScFkzUmxjM1JqYUdGc2JHVnVaMlV3TVRJek5EVSIsIm9yaWdpbiI6Imh0dHBzOi8vZXhhbXBsZS5jb20ifQ","authenticatorData":"o3mm9u6vuaVeN4wRgDTidR5oL6ufLTCrE9ISVYbOGUcFAAAAAQ","signature":"740vqfLclv7QCTmONhcv8vg2lG3dRhDYa7sQefUlnmPi8zBc9Kn4AZxyDklUp72EGl9X6L4BrGMNpLI9w3R8Cw"}}',
    '{"alg":"EdDSA","kty":"OKP","crv":"Ed25519","x":"11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURp"}',
    0) IS NULL;

/* Sign-count regression: authData reports counter=1, we pass
 * expected=5 -> rejected, NULL. */
SELECT webauthn_verify_authentication(
    '{"challenge":"ZGV0ZXJtaW5pc3RpY3Rlc3RjaGFsbGVuZ2UwMTIzNDU","rpId":"example.com","allowCredentials":[],"userVerification":"preferred"}',
    '{"id":"test-credential-id","rawId":"test-credential-id","type":"public-key","response":{"clientDataJSON":"eyJ0eXBlIjoid2ViYXV0aG4uZ2V0IiwiY2hhbGxlbmdlIjoiWkdWMFpYSnRhVzVwYzNScFkzUmxjM1JqYUdGc2JHVnVaMlV3TVRJek5EVSIsIm9yaWdpbiI6Imh0dHBzOi8vZXhhbXBsZS5jb20ifQ","authenticatorData":"o3mm9u6vuaVeN4wRgDTidR5oL6ufLTCrE9ISVYbOGUcFAAAAAQ","signature":"740vqfLclv7QCTmONhcv8vg2lG3dRhDYa7sQefUlnmPi8zBc9Kn4AZxyDklUp72EGl9X6L4BrGMNpLI9w3R8Cw"}}',
    '{"alg":"EdDSA","kty":"OKP","crv":"Ed25519","x":"11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo"}',
    5) IS NULL;

/* ─── End-to-end EdDSA REGISTRATION round-trip ───
 * Same Ed25519 keypair; rpId=example.com; create-challenge
 * "register-test-challenge0123456789". credential id = bytes 1..8.
 * fmt = "none" so no attestation chain to validate. */
SELECT json_extract(
  webauthn_verify_registration(
    '{"challenge":"cmVnaXN0ZXItdGVzdC1jaGFsbGVuZ2UwMTIzNDU2Nzg","rp":{"id":"example.com","name":"Example RP"},"user":{"id":"dXNlci1pbnRlcm5hbC1pZC0wMDE","name":"alice","displayName":"Alice Aardvark"},"pubKeyCredParams":[{"type":"public-key","alg":-8},{"type":"public-key","alg":-7}],"timeout":60000,"attestation":"none"}',
    '{"id":"AQIDBAUGBwg","rawId":"AQIDBAUGBwg","type":"public-key","response":{"clientDataJSON":"eyJ0eXBlIjoid2ViYXV0aG4uY3JlYXRlIiwiY2hhbGxlbmdlIjoiY21WbmFYTjBaWEl0ZEdWemRDMWphR0ZzYkdWdVoyVXdNVEl6TkRVMk56ZyIsIm9yaWdpbiI6Imh0dHBzOi8vZXhhbXBsZS5jb20ifQ","attestationObject":"o2NmbXRkbm9uZWhhdXRoRGF0YVhpo3mm9u6vuaVeN4wRgDTidR5oL6ufLTCrE9ISVYbOGUdFAAAAAAAAAAAAAAAAAAAAAAAAAAAACAECAwQFBgcIpAEBAycgBiFYINdamAGCsQq31Uv-08lkBzoO4XLz2qYjJa8CGmj3B1EaZ2F0dFN0bXSg"}}',
    'example.com'),
  '$.credential_id');

/* attestation_format field. */
SELECT json_extract(
  webauthn_verify_registration(
    '{"challenge":"cmVnaXN0ZXItdGVzdC1jaGFsbGVuZ2UwMTIzNDU2Nzg","rp":{"id":"example.com","name":"Example RP"},"user":{"id":"dXNlci1pbnRlcm5hbC1pZC0wMDE","name":"alice","displayName":"Alice Aardvark"},"pubKeyCredParams":[{"type":"public-key","alg":-8},{"type":"public-key","alg":-7}],"timeout":60000,"attestation":"none"}',
    '{"id":"AQIDBAUGBwg","rawId":"AQIDBAUGBwg","type":"public-key","response":{"clientDataJSON":"eyJ0eXBlIjoid2ViYXV0aG4uY3JlYXRlIiwiY2hhbGxlbmdlIjoiY21WbmFYTjBaWEl0ZEdWemRDMWphR0ZzYkdWdVoyVXdNVEl6TkRVMk56ZyIsIm9yaWdpbiI6Imh0dHBzOi8vZXhhbXBsZS5jb20ifQ","attestationObject":"o2NmbXRkbm9uZWhhdXRoRGF0YVhpo3mm9u6vuaVeN4wRgDTidR5oL6ufLTCrE9ISVYbOGUdFAAAAAAAAAAAAAAAAAAAAAAAAAAAACAECAwQFBgcIpAEBAycgBiFYINdamAGCsQq31Uv-08lkBzoO4XLz2qYjJa8CGmj3B1EaZ2F0dFN0bXSg"}}',
    'example.com'),
  '$.attestation_format');

/* Embedded user_id round-trips from /user/id in options. */
SELECT json_extract(
  webauthn_verify_registration(
    '{"challenge":"cmVnaXN0ZXItdGVzdC1jaGFsbGVuZ2UwMTIzNDU2Nzg","rp":{"id":"example.com","name":"Example RP"},"user":{"id":"dXNlci1pbnRlcm5hbC1pZC0wMDE","name":"alice","displayName":"Alice Aardvark"},"pubKeyCredParams":[{"type":"public-key","alg":-8},{"type":"public-key","alg":-7}],"timeout":60000,"attestation":"none"}',
    '{"id":"AQIDBAUGBwg","rawId":"AQIDBAUGBwg","type":"public-key","response":{"clientDataJSON":"eyJ0eXBlIjoid2ViYXV0aG4uY3JlYXRlIiwiY2hhbGxlbmdlIjoiY21WbmFYTjBaWEl0ZEdWemRDMWphR0ZzYkdWdVoyVXdNVEl6TkRVMk56ZyIsIm9yaWdpbiI6Imh0dHBzOi8vZXhhbXBsZS5jb20ifQ","attestationObject":"o2NmbXRkbm9uZWhhdXRoRGF0YVhpo3mm9u6vuaVeN4wRgDTidR5oL6ufLTCrE9ISVYbOGUdFAAAAAAAAAAAAAAAAAAAAAAAAAAAACAECAwQFBgcIpAEBAycgBiFYINdamAGCsQq31Uv-08lkBzoO4XLz2qYjJa8CGmj3B1EaZ2F0dFN0bXSg"}}',
    'example.com'),
  '$.user_id');

/* True end-to-end: register, extract the public_key, then auth-verify
 * with that very key (non-NULL on success). */
SELECT webauthn_verify_authentication(
    '{"challenge":"ZGV0ZXJtaW5pc3RpY3Rlc3RjaGFsbGVuZ2UwMTIzNDU","rpId":"example.com","allowCredentials":[],"userVerification":"preferred"}',
    '{"id":"test-credential-id","rawId":"test-credential-id","type":"public-key","response":{"clientDataJSON":"eyJ0eXBlIjoid2ViYXV0aG4uZ2V0IiwiY2hhbGxlbmdlIjoiWkdWMFpYSnRhVzVwYzNScFkzUmxjM1JqYUdGc2JHVnVaMlV3TVRJek5EVSIsIm9yaWdpbiI6Imh0dHBzOi8vZXhhbXBsZS5jb20ifQ","authenticatorData":"o3mm9u6vuaVeN4wRgDTidR5oL6ufLTCrE9ISVYbOGUcFAAAAAQ","signature":"740vqfLclv7QCTmONhcv8vg2lG3dRhDYa7sQefUlnmPi8zBc9Kn4AZxyDklUp72EGl9X6L4BrGMNpLI9w3R8Cw"}}',
    json_extract(
      webauthn_verify_registration(
        '{"challenge":"cmVnaXN0ZXItdGVzdC1jaGFsbGVuZ2UwMTIzNDU2Nzg","rp":{"id":"example.com","name":"Example RP"},"user":{"id":"dXNlci1pbnRlcm5hbC1pZC0wMDE","name":"alice","displayName":"Alice Aardvark"},"pubKeyCredParams":[{"type":"public-key","alg":-8},{"type":"public-key","alg":-7}],"timeout":60000,"attestation":"none"}',
        '{"id":"AQIDBAUGBwg","rawId":"AQIDBAUGBwg","type":"public-key","response":{"clientDataJSON":"eyJ0eXBlIjoid2ViYXV0aG4uY3JlYXRlIiwiY2hhbGxlbmdlIjoiY21WbmFYTjBaWEl0ZEdWemRDMWphR0ZzYkdWdVoyVXdNVEl6TkRVMk56ZyIsIm9yaWdpbiI6Imh0dHBzOi8vZXhhbXBsZS5jb20ifQ","attestationObject":"o2NmbXRkbm9uZWhhdXRoRGF0YVhpo3mm9u6vuaVeN4wRgDTidR5oL6ufLTCrE9ISVYbOGUdFAAAAAAAAAAAAAAAAAAAAAAAAAAAACAECAwQFBgcIpAEBAycgBiFYINdamAGCsQq31Uv-08lkBzoO4XLz2qYjJa8CGmj3B1EaZ2F0dFN0bXSg"}}',
        'example.com'),
      '$.public_key'),
    0) IS NOT NULL;
