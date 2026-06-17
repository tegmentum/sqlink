# Canonical test samples

Known-good inputs for algorithms-with-check-digit and similar
validators. Use these in smoke.sql files for new extensions
that port one of these algorithms  saves re-Googling, and the
algorithm gets exercised against a real example instead of a
"this kinda looks right" sample.

Each entry lists:
- The sample
- What it is (so the next reader knows it's not random)
- Where it's documented (so you can verify if a check fails)

## Identification numbers

| Type | Sample | Notes |
|---|---|---|
| ISBN-10 | `0-19-852663-6` | Oxford English Dictionary, Wikipedia |
| ISBN-13 | `9780198526636` | same book, ISBN-13 form |
| ISBN registration-group example (English) | `9780198526636` | maps to "English language" |
| ISBN registration-group example (Brazil) | `9788578205614` | maps to "Brazil" |
| VIN | `1M8GDM9AXKP042788` | Wikipedia algorithm worked example; check digit X |
| ULID | `01JZ7E5XYZK4VPS9TQM03R2HBN` | example valid Crockford base32 |

## Cards

ISO 8583 publicly-published test cards (Luhn-passing, not real):

| Brand | Test number |
|---|---|
| Visa (16 digit) | `4111111111111111` |
| Visa (alt) | `4012888888881881` |
| Mastercard | `5555555555554444` |
| Mastercard (2-series) | `2223003122003222` |
| Amex (15 digit) | `378282246310005` |
| Discover | `6011111111111117` |
| Diners | `30569309025904` |
| JCB | `3530111333300000` |
| UnionPay | `6200000000000005` |

## Encodings

| Algorithm | Input  Output | Notes |
|---|---|---|
| base32 (RFC 4648 std, no pad) | `Hello`  `JBSWY3DP` | classic example |
| base58 (Bitcoin) | `0001020304`  `12VfUX` | leading zero bytes become leading `1`s |
| Punycode (xn--) | `mnchen.de`  `xn--mnchen-3ya.de` | RFC 3492 |
| Punycode CJK | ``  `xn--kpry57d` | Taiwan |
| Bencode integer | `42`  `i42e` | BEP 0003 |
| Bencode string | `"hello"`  `5:hello` | length-prefix |
| Bencode list | `[1,2,3]`  `li1ei2ei3ee` | nested |
| Bencode dict (sorted) | `{"foo":1,"bar":"x"}`  `d3:bar1:x3:fooi1ee` | keys sorted! |

## Crypto / hashes

| Hash | Input  Output | Notes |
|---|---|---|
| SHA-256 (empty) | `""`  `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` | RFC 6234 |
| BPE/tiktoken cl100k_base | `"hello world"`  `[15339, 1917]` | OpenAI public token IDs |

## Networking

| Type | Sample | Notes |
|---|---|---|
| MAC (colon) | `AA:BB:CC:11:22:33` | classic form |
| MAC (Cisco dot) | `AABB.CC11.2233` | Cisco gear's preferred |
| Multicast MAC | `01:00:5E:00:00:01` | bit 0 of first byte set |
| Locally-administered MAC | `02:00:00:11:22:33` | bit 1 of first byte set |
| RFC 5321 / 5322 email | `alice+tag@example.com` | with subaddressing |

## Time / dates

| Source | Sample | Notes |
|---|---|---|
| Unix timestamp anchor | `1750000000` | 2025-06-15 16:26:40 UTC (rounded to a nice digit base) |
| Next-midnight cron | `0 0 * * *` | for relative-fire-time testing |

## Geography

| Type | Sample | Notes |
|---|---|---|
| ISO 3166-1 alpha-2 | `US` | United States |
| ISO 3166-1 alpha-3 | `USA` | maps to alpha-2 = US |
| ISO 4217 currency | `EUR` | numeric = 978, symbol =  |
| ISO 4217 currency (no minor units) | `JPY` | exponent = 0 (yen has no minor unit) |
| ISO 639-1 language | `en` | maps to 639-3 = `eng` |

## SQL / GraphQL

| Type | Sample | Notes |
|---|---|---|
| SQL (with JOIN) | `SELECT a.x FROM users a JOIN orders o ON a.id = o.user_id` | exercises table-collection visitor |
| GraphQL (named query) | `query Foo { user { id name } }` | named operation + nested selection |
| GraphQL (mutation) | `mutation Bar { delete }` | distinct from query |

## Sentiment

| Polarity | Sample | Expected (compound, VADER) |
|---|---|---|
| strong positive | `I love this! It is amazing!` | ~0.86 |
| strong negative | `This is terrible. I hate it.` | ~-0.78 |
| neutral | `The weather is okay today.` | ~0.23 (mildly positive  "okay" has slight valence) |

---

When adding an entry: anchor it to a public spec, RFC, or
algorithm-author worked example. Don't add "I made this up and
it passes my code"  the whole point is referenced verifiability.
