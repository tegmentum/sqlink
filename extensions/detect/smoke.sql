-- Smoke test for the `detect` extension.
.load detect

SELECT slug('Hello, World!');
SELECT lang_detect('This is an English sentence.');
SELECT lang_detect('Esto es una oración en español.');
SELECT mime_detect(x'89504e470d0a1a0a');
SELECT mime_extension(x'504b0304');
