.load extensions/faker/target/wasm32-wasip2/release/faker_extension.component.wasm

SELECT length(fake_name()) > 0;
SELECT length(fake_email()) > 0;
SELECT instr(fake_email(), '@') > 0;
SELECT length(fake_ipv4()) > 6;
SELECT length(fake_company()) > 0;
SELECT length(fake_city()) > 0;
SELECT length(fake_password()) BETWEEN 8 AND 32;
