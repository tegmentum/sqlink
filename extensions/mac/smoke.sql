.load extensions/mac/target/wasm32-wasip2/release/mac_extension.component.wasm

SELECT mac_validate('AA:BB:CC:11:22:33');
SELECT mac_validate('aabb.cc11.2233');
SELECT mac_validate('AABBCC112233');
SELECT mac_validate('not a mac');
SELECT mac_normalize('aa-bb-cc-11-22-33');
SELECT mac_normalize('AABB.CC11.2233');
SELECT mac_oui('AA:BB:CC:11:22:33');
SELECT mac_nic('AA:BB:CC:11:22:33');
SELECT mac_is_multicast('01:00:5E:11:22:33');
SELECT mac_is_multicast('AA:BB:CC:11:22:33');
SELECT mac_is_local('02:00:00:11:22:33');
SELECT mac_format('AA:BB:CC:11:22:33', '-');
