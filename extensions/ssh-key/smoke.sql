.load extensions/ssh-key/target/wasm32-wasip2/release/ssh_key_extension.component.wasm

/* ssh-key  OpenSSH key file parsing (RFC 4253 + RFC 4716 + the
 * wrapped OpenSSH private-key format).
 *
 * Fixtures generated with ssh-keygen and embedded as TEXT-cast hex
 * blobs so the smoke is deterministic across runs (no random keys
 * regenerated mid-test). Each public-key value is the exact ascii
 * bytes ssh-keygen wrote to disk (single line, no trailing newline).
 *
 *   ed25519.pub   = `ssh-ed25519 AAAA...V9jG smoke@example.com`
 *                   SHA256 fp  : SHA256:O0/oKMoNiFA5dZbnFbuSrtNjAcH4nCTAOv9N/fS0Y0c
 *                   MD5 fp     : MD5:91:ba:26:70:ff:da:cd:75:0b:8f:fa:40:c0:37:b9:61
 *
 *   rsa.pub       = `ssh-rsa AAAAB3Nz...g44cWb rsa-smoke@example.com`  (2048-bit)
 *                   SHA256 fp  : SHA256:+k4u7vS2Jp4JdCiimuRv8+e9VerJv8GU9702LToZeQY
 *                   MD5 fp     : MD5:5e:26:1b:40:9a:7f:0a:96:75:09:a5:17:35:2f:20:2d
 *
 *   ecdsa.pub     = `ecdsa-sha2-nistp256 AAAA...w7I= ecdsa-smoke@example.com`
 *                   SHA256 fp  : SHA256:AcryHb+GKnxKFEsgh7Fb2HqhEPtgNk7Xyl/3zvmBTJ4
 *
 *   ed25519.priv  = unencrypted -----BEGIN OPENSSH PRIVATE KEY----- block
 *                   (matches ed25519.pub above)
 *
 *   enc.priv      = encrypted (aes256-ctr / bcrypt) wrapped OpenSSH block,
 *                   public half corresponds to ed25519 SHA256:nA1y... pub
 *
 * Fingerprints below were captured from `ssh-keygen -lf <key>.pub`
 * and `ssh-keygen -E md5 -lf <key>.pub`; the extension must match
 * those byte-for-byte.
 */

CREATE TABLE keys(name TEXT PRIMARY KEY, pub TEXT, priv TEXT);

/* ed25519 public key (single line, no trailing newline). */
INSERT INTO keys(name, pub) VALUES (
  'ed25519',
  CAST(x'7373682d65643235353139204141414143334e7a6143316c5a4449314e54453541414141494362344e30536562536d302f766b4648365230476e6e776c327755324c6544665171614b46687156396a4720736d6f6b65406578616d706c652e636f6d' AS TEXT)
);

/* rsa public key (2048-bit). */
INSERT INTO keys(name, pub) VALUES (
  'rsa',
  CAST(x'7373682d727361204141414142334e7a61433179633245414141414441514142414141424151433679784e4a2b4874577a75784e376a464a357746753263434f69625773495239736c5265506d614974484f657456754a4238416d3936374e36547731712f6175336778597572414754727849725a6b57357a51526364724464475635586f79387157727146666e645342366e5564546939595054484b713752413053635755326753706858586e6b375a2b563278327646774879336530646e7977547844756d7644507564706138775a4f534939654b32494b7937584d67644d2f53666b534a6532497158482b3150795a467957683331426f55556c2b676575664f646468566f336c7a724a596c6c6d757665634d766e4b6a4d6c326e56497a79434449647059366237524a316d756669524356797a79435352564a564d344464336f45786a2b394c474a472f5166424c2f6f56684c65547074554a553743734d39664a4a6a3377664a3030482b725a4b6d636367593434635762207273612d736d6f6b65406578616d706c652e636f6d' AS TEXT)
);

/* ecdsa-sha2-nistp256 public key. */
INSERT INTO keys(name, pub) VALUES (
  'ecdsa',
  CAST(x'65636473612d736861322d6e6973747032353620414141414532566a5a484e684c584e6f59544974626d6c7a644841794e54594141414149626d6c7a644841794e54594141414242424d6674787737727a797235594a5366706c6b4e33615638486c545a6863384b656757426966693976777931757853706b39624d4f585231463364575a73374c582f3874545175616168395262616667414e68527737493d2065636473612d736d6f6b65406578616d706c652e636f6d' AS TEXT)
);

/* unencrypted ed25519 private key (matches ed25519 pub above). */
INSERT INTO keys(name, priv) VALUES (
  'ed25519priv',
  CAST(x'2d2d2d2d2d424547494e204f50454e5353482050524956415445204b45592d2d2d2d2d0a6233426c626e4e7a614331725a586b74646a45414141414142473576626d554141414145626d39755a5141414141414141414142414141414d7741414141747a633267745a570a51794e5455784f5141414143416d2b4464456e6d30707450373542522b6b64427035384a6473464e69336733304b6d696859616c665978674141414a6965675453386e6f45300a764141414141747a633267745a5751794e5455784f5141414143416d2b4464456e6d30707450373542522b6b64427035384a6473464e69336733304b6d696859616c665978670a41414145427958424844525056574f774933744d2f723775743654547648464c6163444e6f31456d556f7a467935666962344e30536562536d302f766b4648365230476e6e770a6c327755324c6544665171614b46687156396a474141414145584e746232746c514756345957317762475575593239744151494442413d3d0a2d2d2d2d2d454e44204f50454e5353482050524956415445204b45592d2d2d2d2d' AS TEXT)
);

/* encrypted ed25519 private key (aes256-ctr / bcrypt). */
INSERT INTO keys(name, priv) VALUES (
  'encpriv',
  CAST(x'2d2d2d2d2d424547494e204f50454e5353482050524956415445204b45592d2d2d2d2d0a6233426c626e4e7a614331725a586b74646a454141414141436d466c637a49314e69316a6448494141414147596d4e796558423041414141474141414142423472646171346e0a52486868745075496d53523965324141414147414141414145414141417a4141414143334e7a6143316c5a4449314e54453541414141494233586c2b4e37587569355263706d0a42355139374b345332594f43414955344639624372784f556c4a3276414141416f443063346f7743364d4b61785a474636317058774f5a4e35457062506932666b72717555710a376b7553573277357a71596d324750443258686d41446a53743165744a2f7a4b632b50486f4b3562566d594d464d4661685a6365722b536263504d795955743571484d594c460a50536f326b4368723076565953704d7057475430755a5130562b31614b6e585434694151705876554e427561577459314364443246546d5748637437644e4c3353522b4b73540a722f446d68524541336e7944736c58784131756f4f415a755031416f6146415643313554633d0a2d2d2d2d2d454e44204f50454e5353482050524956415445204b45592d2d2d2d2d' AS TEXT)
);

/* one garbage row to exercise the NULL-on-bad-input path. */
INSERT INTO keys(name, pub) VALUES ('garbage', 'not an ssh key');

/* Acceptance: algorithm + bits + comment, all three key types. */
SELECT ssh_key_algorithm(pub) FROM keys WHERE name='ed25519';
SELECT ssh_key_algorithm(pub) FROM keys WHERE name='rsa';
SELECT ssh_key_algorithm(pub) FROM keys WHERE name='ecdsa';
SELECT ssh_key_bits(pub)      FROM keys WHERE name='ed25519';
SELECT ssh_key_bits(pub)      FROM keys WHERE name='rsa';
SELECT ssh_key_bits(pub)      FROM keys WHERE name='ecdsa';
SELECT ssh_key_comment(pub)   FROM keys WHERE name='ed25519';
SELECT ssh_key_comment(pub)   FROM keys WHERE name='rsa';
SELECT ssh_key_comment(pub)   FROM keys WHERE name='ecdsa';

/* Acceptance: SHA-256 fingerprint matches `ssh-keygen -lf <file>`. */
SELECT ssh_key_fingerprint_sha256(pub) FROM keys WHERE name='ed25519';
SELECT ssh_key_fingerprint_sha256(pub) FROM keys WHERE name='rsa';
SELECT ssh_key_fingerprint_sha256(pub) FROM keys WHERE name='ecdsa';

/* MD5 fingerprint matches `ssh-keygen -E md5 -lf <file>`. */
SELECT ssh_key_fingerprint_md5(pub)    FROM keys WHERE name='ed25519';
SELECT ssh_key_fingerprint_md5(pub)    FROM keys WHERE name='rsa';
SELECT ssh_key_fingerprint_md5(pub)    FROM keys WHERE name='ecdsa';

/* Private-key path: parses, exposes algorithm + comment, fingerprint
 * of the embedded public half matches the public-key fixture. */
SELECT ssh_key_algorithm(priv)            FROM keys WHERE name='ed25519priv';
SELECT ssh_key_comment(priv)              FROM keys WHERE name='ed25519priv';
SELECT ssh_key_fingerprint_sha256(priv)   FROM keys WHERE name='ed25519priv';
SELECT ssh_key_is_encrypted(priv)         FROM keys WHERE name='ed25519priv';

/* pub_from_priv on the unencrypted private key  recovers the
 * canonical one-line form matching the ed25519 fixture pub. The
 * `ssh-key` to_openssh helper writes `<algo> <base64> <comment>`. */
SELECT ssh_key_pub_from_priv(priv) = (SELECT pub FROM keys WHERE name='ed25519')
  FROM keys WHERE name='ed25519priv';

/* Encrypted private key  metadata parses, is_encrypted = 1,
 * pub_from_priv returns NULL (plan acceptance). Note the comment
 * lives inside the encrypted payload so an unopened encrypted key
 * exposes an empty comment string; we coalesce to a sentinel so
 * the empty result doesn't get swallowed by the smoke harness's
 * blank-line filter. */
SELECT ssh_key_algorithm(priv)                          FROM keys WHERE name='encpriv';
SELECT coalesce(nullif(ssh_key_comment(priv), ''), '<no-comment>')
                                                        FROM keys WHERE name='encpriv';
SELECT ssh_key_is_encrypted(priv)                       FROM keys WHERE name='encpriv';
SELECT ssh_key_fingerprint_sha256(priv)                 FROM keys WHERE name='encpriv';
SELECT ssh_key_pub_from_priv(priv)                      FROM keys WHERE name='encpriv';

/* JSON-shaped metadata: all fields populated. Spot-check via
 * json_extract  no need to assert the entire JSON string. */
SELECT json_extract(ssh_key_all(pub), '$.algorithm')          FROM keys WHERE name='ed25519';
SELECT json_extract(ssh_key_all(pub), '$.bits')               FROM keys WHERE name='ed25519';
SELECT json_extract(ssh_key_all(pub), '$.fingerprint_sha256') FROM keys WHERE name='ed25519';
SELECT json_extract(ssh_key_all(pub), '$.is_private')         FROM keys WHERE name='ed25519';
SELECT json_extract(ssh_key_all(pub), '$.is_encrypted')       FROM keys WHERE name='ed25519';

/* NULL / garbage  every field-extracting fn NULLs out (no panic). */
SELECT ssh_key_algorithm(pub)             FROM keys WHERE name='garbage';
SELECT ssh_key_fingerprint_sha256(pub)    FROM keys WHERE name='garbage';
SELECT ssh_key_bits(pub)                  FROM keys WHERE name='garbage';
SELECT ssh_key_all(pub)                   FROM keys WHERE name='garbage';
SELECT ssh_key_algorithm(NULL);
SELECT ssh_key_fingerprint_sha256(NULL);

/* version  non-empty. */
SELECT length(ssh_key_version()) > 0;
