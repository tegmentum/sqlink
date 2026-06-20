.load extensions/mqtt-parse/target/wasm32-wasip2/release/mqtt_parse_extension.component.wasm

/* ---- PUBLISH QoS=0 topic=a/b payload='hi' ---- */
SELECT mqtt_packet_type(x'30070003612f626869');            /* PUBLISH */
SELECT mqtt_topic(x'30070003612f626869');                  /* a/b */
SELECT mqtt_qos(x'30070003612f626869');                    /* 0 */
SELECT mqtt_retain(x'30070003612f626869');                 /* 0 */
SELECT mqtt_packet_id(x'30070003612f626869');              /* NULL  no pid for QoS0 */
SELECT CAST(mqtt_payload(x'30070003612f626869') AS TEXT);  /* hi */
SELECT mqtt_is_valid(x'30070003612f626869');               /* 1 */

/* ---- PUBLISH QoS=1 retain=1 topic=x payload='hello' pid=0x1234 ---- */
SELECT mqtt_packet_type(x'330a000178123468656c6c6f');      /* PUBLISH */
SELECT mqtt_qos(x'330a000178123468656c6c6f');              /* 1 */
SELECT mqtt_retain(x'330a000178123468656c6c6f');           /* 1 */
SELECT mqtt_packet_id(x'330a000178123468656c6c6f');        /* 4660  (0x1234) */
SELECT CAST(mqtt_payload(x'330a000178123468656c6c6f') AS TEXT);  /* hello */

/* ---- CONNECT v3.1.1 (protocol_level=4) ---- */
SELECT mqtt_packet_type(x'100f00044d5154540402003c0003616263');  /* CONNECT */
SELECT mqtt_is_valid(x'100f00044d5154540402003c0003616263');     /* 1 */

/* ---- PINGREQ (no body) ---- */
SELECT mqtt_packet_type(x'c000');                          /* PINGREQ */
SELECT mqtt_is_valid(x'c000');                             /* 1 */

/* ---- SUBSCRIBE pid=1 topic=x qos=0 ---- */
SELECT mqtt_packet_type(x'8206000100017800');              /* SUBSCRIBE */
SELECT mqtt_packet_id(x'8206000100017800');                /* 1 */

/* ---- Garbage  NULL/0 ---- */
SELECT mqtt_packet_type(x'deadbeef');                      /* NULL  malformed varint length runs off */
SELECT mqtt_is_valid(x'deadbeef');                         /* 0 */
SELECT mqtt_packet_type(NULL);                             /* NULL */
SELECT mqtt_is_valid(NULL);                                /* NULL */

/* ---- mqtt_parse: JSON over the PUBLISH ---- */
SELECT json_extract(mqtt_parse(x'30070003612f626869'), '$.type');         /* PUBLISH */
SELECT json_extract(mqtt_parse(x'30070003612f626869'), '$.topic');        /* a/b */
SELECT json_extract(mqtt_parse(x'30070003612f626869'), '$.qos');          /* 0 */
SELECT json_extract(mqtt_parse(x'30070003612f626869'), '$.payload_utf8'); /* hi */

/* ---- mqtt_version ---- */
SELECT mqtt_version() LIKE 'mqtt-parse %';                 /* 1 */
