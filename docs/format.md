# The format

What a payload looks like on disk, version by version. `codec/` writes version
1 and reads every version it has ever written; a released version never moves,
because somebody's file holds it. The module's version is a different number
and says nothing about these bytes.

There is no schema yet. When the store exists, its migrations belong here too.

## What the codec promises

It takes 1..240 strictly time-ordered samples and returns one payload. It
preserves signed timestamps and every IEEE-754 bit, including `-0`, NaN
payloads and infinities. It does not sort, deduplicate, round, choose a
retention policy or decide when to seal — a caller does all of that, and the
codec never sees why.

One encoder and one decoder worker are shared behind a mutex; an iterator holds
neither and may outlive the codec that produced it.

## Version 1

All multibyte fixed-width integers are little-endian. Payloads have this layout:

| offset | bytes    | meaning                                                    |
|--------|----------|------------------------------------------------------------|
| 0      | 2        | `TS` magic                                                 |
| 2      | 1        | version, currently 1                                       |
| 3      | 1        | timestamp encoding: fixed step / delta / delta-of-delta    |
| 4      | 1        | value encoding: raw / constant / integer / XOR             |
| 5      | 1        | body compression: none / zstd                              |
| 6      | 2        | sample count, 1..240                                       |
| 8      | 2        | decoded timestamp-stream length                            |
| 10     | 2        | decoded value-stream length                                |
| 12     | variable | timestamp stream followed by value stream, optionally zstd |
| end-4  | 4        | CRC32C of the preceding bytes                              |

The first timestamp occupies eight bytes. Fixed-step stores one unsigned delta;
delta stores positive unsigned distances; delta-of-delta stores the first
distance and signed changes thereafter. All arithmetic is checked when decoding,
including the full signed timestamp range. A single-sample stream has no delta.

Values use one of four representations:

- raw IEEE-754 bits, eight bytes each;
- one constant bit pattern;
- an exact signed integer followed by ZigZag deltas in Simple8b words;
- the first IEEE-754 value followed by Gorilla-style XOR windows.

The integer path rejects values that cannot reproduce the original float bits,
including negative zero. Oversized deltas fall back to another representation.
Simple8b uses the four high bits as selector, the lower sixty for packed values;
selectors 0 and 1 represent runs of 240 and 120 ones. Partial final words are
zero-padded. XOR uses a zero-difference bit, an existing/new-window bit, five
leading-zero bits and six width bits, where zero width means 64.

The selector compares the small candidates, optionally compresses with zstd, and
also checks raw-value compression when useful. It does not promise that every
result beats the old unversioned spike: the new envelope and checksum cost space,
particularly on tiny decimal-valued chunks. No legacy reader is needed because
the old prototype never shipped a persisted format.

The body and the zstd window are limited to 8 KiB, which bounds one decode and
not a query holding many payloads. A caller inspects `Iterator.Err()` after
iterating and enforces its own budgets for bytes, blocks and series. A block
builder enforces its own encoded-byte ceiling too: a codec result is not
automatically below 2 KiB.
