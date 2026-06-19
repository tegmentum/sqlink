-- Smoke test for the `.session` dot command suite (T-44).
-- Covers: list, create, attach, isempty, changeset, patchset,
-- enable, indirect, delete. Runs against a file-backed db (the
-- cli-smoke runner gives us a per-run tempdir as cwd + --db),
-- so changeset writes land in the wasi sandbox.
-- Inline block comments are avoided  the cli buffers them and
-- glues the buffered content onto the following dot-command.

.session list
.session s1 create
.session list
.session s1 attach *
.session s1 isempty

CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT);
INSERT INTO t VALUES (1,'a'),(2,'b'),(3,'c');

.session s1 isempty
.session s1 changeset out.cs
.session s1 patchset out.ps

.session s1 enable off
.session s1 indirect on
INSERT INTO t VALUES (4,'d');

.session s1 isempty

.session s1 delete
.session list
