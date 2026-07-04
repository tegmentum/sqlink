.load greet

-- Smoke test for the `greet` dot-command extension.
-- Run via:  tooling/smoke.py greet
--
-- greet registers no scalar functions; it only provides the .greet dot command.
-- Each .greet invocation writes "hello, <name>!\n" to cli stdout.

.greet
.greet alice
