.load extensions/image-meta/target/wasm32-wasip2/release/image_meta_extension.component.wasm

/* image-meta  scalar image header metadata.
 *
 * Per PLAN-more-extensions-2.md  7 acceptance:
 *   - 4-byte PNG signature (89 50 4E 47) + IHDR -> width + height
 *   - JPEG SOI + APP0 -> format == 'JPEG'; SOF0 -> width + height
 *   - 0xFFD8 alone (truncated JPEG) -> format == 'JPEG', dims NULL
 *   - random bytes -> all fns return NULL
 *   - img_dimensions returns a JSON object parseable by json_extract
 *
 * Synthetic blob fixtures, in hex:
 *
 *   png_123x321:    minimal PNG  IHDR width=0x7B (123) height=0x141 (321),
 *                   8-bit RGBA. Header-only  no IDAT.
 *
 *   jpeg_100x200:   JPEG SOI + APP0 (JFIF) + SOF0 marker carrying
 *                   precision 8, height=0x00C8 (200), width=0x0064 (100),
 *                   3 components. Enough for imagesize to extract dims.
 *
 *   jpeg_trunc:     12-byte blob starting with FF D8 FF E0 plus an APP0
 *                   length that runs off the end of the buffer.
 *                   image_type matches JPEG (FF D8 FF), but blob_size's
 *                   marker walk hits EOF and returns CorruptedImage.
 *
 *   random_bytes:   12 bytes of 0xAA  matches no signature; both
 *                   image_type and blob_size return NotSupported.
 */

-- ---- Acceptance 1: PNG signature + IHDR -> width/height ----
SELECT img_format(x'89504E470D0A1A0A0000000D494844520000007B000001410806000000009A38C4');
SELECT img_width (x'89504E470D0A1A0A0000000D494844520000007B000001410806000000009A38C4');
SELECT img_height(x'89504E470D0A1A0A0000000D494844520000007B000001410806000000009A38C4');

-- ---- Acceptance 2: full JPEG with SOF0 -> 'JPEG' + dims ----
-- SOI + APP0/JFIF (length 0x0010) + SOF0 (length 0x0011, height 200, width 100, 3 comp)
SELECT img_format(x'FFD8FFE000104A46494600010200000100010000FFC000110800C8006403012200021101031101');
SELECT img_width (x'FFD8FFE000104A46494600010200000100010000FFC000110800C8006403012200021101031101');
SELECT img_height(x'FFD8FFE000104A46494600010200000100010000FFC000110800C8006403012200021101031101');

-- ---- Acceptance 3: truncated JPEG -> 'JPEG', dims NULL ----
-- 12 bytes: FF D8 FF E0 00 10 + 6 zeros. image_type reads 12 bytes, sees
-- FF D8 FF and tags it JPEG. blob_size's marker walk reads APP0 length
-- 0x10 then runs off the end  CorruptedImage  NULL.
SELECT img_format(x'FFD8FFE000100000000000000000');
SELECT img_width (x'FFD8FFE000100000000000000000');
SELECT img_height(x'FFD8FFE000100000000000000000');

-- ---- Acceptance 4: random bytes -> all NULL ----
SELECT img_format(x'AAAAAAAAAAAAAAAAAAAAAAAA');
SELECT img_width (x'AAAAAAAAAAAAAAAAAAAAAAAA');
SELECT img_height(x'AAAAAAAAAAAAAAAAAAAAAAAA');
SELECT img_dimensions(x'AAAAAAAAAAAAAAAAAAAAAAAA');

-- ---- Acceptance 5: img_dimensions returns JSON parseable by json_extract ----
SELECT img_dimensions(x'89504E470D0A1A0A0000000D494844520000007B000001410806000000009A38C4');
SELECT json_extract(img_dimensions(x'89504E470D0A1A0A0000000D494844520000007B000001410806000000009A38C4'), '$.width');
SELECT json_extract(img_dimensions(x'89504E470D0A1A0A0000000D494844520000007B000001410806000000009A38C4'), '$.height');
SELECT json_extract(img_dimensions(x'89504E470D0A1A0A0000000D494844520000007B000001410806000000009A38C4'), '$.format');

-- ---- img_byte_size: convenience length() ----
SELECT img_byte_size(x'89504E470D0A1A0A0000000D494844520000007B000001410806000000009A38C4');
SELECT img_byte_size(x'AAAA');

-- ---- NULL passthrough on every fn ----
SELECT img_format(NULL);
SELECT img_width(NULL);
SELECT img_height(NULL);
SELECT img_dimensions(NULL);
SELECT img_byte_size(NULL);

-- ---- Version sanity ----
SELECT img_version();
