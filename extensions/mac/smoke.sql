.load extensions/mac/target/wasm32-wasip2/release/mac_extension.component.wasm

SELECT mac_nic('AA:BB:CC:11:22:33');
SELECT mac_is_multicast('01:00:5E:11:22:33');
SELECT mac_is_multicast('AA:BB:CC:11:22:33');
SELECT mac_is_local('02:00:00:11:22:33');
