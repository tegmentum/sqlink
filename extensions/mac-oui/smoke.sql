.load extensions/mac-oui/target/wasm32-wasip2/release/mac_oui_extension.component.wasm

/* ─── mac_is_valid: accepts every common shape ─── */
SELECT mac_is_valid('aa:bb:cc:dd:ee:ff');
SELECT mac_is_valid('AA-BB-CC-DD-EE-FF');
SELECT mac_is_valid('aabb.ccdd.eeff');
SELECT mac_is_valid('aabbccddeeff');
SELECT mac_is_valid('not a mac');
SELECT mac_is_valid('aa:bb:cc:dd:ee');           -- too short
SELECT mac_is_valid('aa:bb:cc:dd:ee:ff:gg');     -- non-hex tail

/* ─── mac_normalize: lowercase colon-separated, regardless of input ─── */
SELECT mac_normalize('AA-BB-CC-DD-EE-FF');
SELECT mac_normalize('AABB.CCDD.EEFF');
SELECT mac_normalize('aabbccddeeff');
SELECT mac_normalize('garbage');                 -- NULL

/* ─── mac_format: explicit style argument ─── */
SELECT mac_format('aabbccddeeff', 'colon');
SELECT mac_format('aabbccddeeff', 'dash');
SELECT mac_format('aabbccddeeff', 'dot');
SELECT mac_format('aabbccddeeff', 'bare');
SELECT mac_format('aabbccddeeff');               -- default colon

/* ─── mac_oui: first 3 octets, uppercase, no separator ─── */
SELECT mac_oui('aa:bb:cc:dd:ee:ff');
SELECT mac_oui('00:00:0C:dd:ee:ff');

/* ─── mac_vendor: well-known IEEE assignments ─── */
SELECT mac_vendor('00:00:0C:dd:ee:ff');          -- Cisco Systems, Inc
SELECT mac_vendor('FF:FF:FF:00:00:00');          -- unassigned → NULL

/* ─── I/G bit (mac_is_unicast) and U/L bit (mac_is_universal) ───
 * 0xAA = 10101010
 *   bit 0 (I/G) = 0 → unicast (mac_is_unicast = 1)
 *   bit 1 (U/L) = 1 → locally administered (mac_is_universal = 0)
 * 0x00 = 00000000
 *   bit 0 = 0 → unicast
 *   bit 1 = 0 → universally administered
 * 0x01 = 00000001 (multicast)
 *   bit 0 = 1 → multicast (mac_is_unicast = 0)
 */
SELECT mac_is_unicast('aa:bb:cc:dd:ee:ff');
SELECT mac_is_universal('aa:bb:cc:dd:ee:ff');
SELECT mac_is_unicast('00:11:22:33:44:55');
SELECT mac_is_universal('00:11:22:33:44:55');
SELECT mac_is_unicast('01:00:5e:00:00:01');      -- IPv4 multicast
SELECT mac_is_universal('01:00:5e:00:00:01');

/* ─── mac_random: returns valid LAA address (U/L = 1 → mac_is_universal = 0) ─── */
SELECT mac_is_valid(mac_random());
SELECT mac_is_universal(mac_random());
SELECT mac_is_unicast(mac_random());

/* ─── version stamp non-empty ─── */
SELECT length(mac_oui_version()) > 0;
