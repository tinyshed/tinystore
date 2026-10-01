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

A payload is the body of a block, and a block is a row with a body. What the
row holds, the body does not repeat:

```text
head       start, end, count, first
body       everything else
```

`Encode` returns the head beside the body; `Decode` is given it back. All
multibyte fixed-width integers are little-endian.

| offset | bytes    | meaning                                                         |
|--------|----------|-----------------------------------------------------------------|
| 0      | 1        | version, currently 1                                            |
| 1      | 1        | bits 0-1 the timestamp encoding, 2-4 the value encoding, 5 zstd |
| 2      | 2        | length of the decoded timestamp stream                          |
| 4      | variable | timestamp stream then value stream, optionally zstd             |
| end-4  | 4        | CRC32C of the head's bytes followed by everything above         |

The checksum covers the head because the head is no longer inside the body: a
row whose `first` was corrupted would otherwise change every sample silently.
A decoder also requires the last timestamp it produces to equal the head's
`end`, which catches the same class of damage a second way.

`EncodeValues` writes values alone for a caller that keeps the timestamps
itself, as the metrics engine keeps them in its clocks: byte 1 above followed by
the stream of evenly spaced samples, with no version, no timestamp length and
no checksum. `DecodeValues` is given the first value and the count, and the
caller checks the bytes it stored. The metrics engine's ordinary value body is
its representation byte followed by exactly this.

An evenly spaced block writes **no timestamps at all**: the step is
`(end - start) / (count - 1)`, and a decoder refuses a head where that division
is not exact. Otherwise the stream holds either the positive distances between
samples or the first distance and the signed changes after it, and all of that
arithmetic is checked when decoding, over the full signed range.

Values use one of five representations:

| byte | representation                                                |
|------|---------------------------------------------------------------|
| 0    | raw IEEE-754 bits, eight bytes each                           |
| 1    | one constant bit pattern                                      |
| 2    | an exact signed integer, then ZigZag deltas in Simple8b words |
| 3    | the first IEEE-754 value, then Gorilla-style XOR windows      |
| 4    | a decimal scale, then the same integers as 2                  |

The integer path rejects values that cannot reproduce the original float bits,
including negative zero. Oversized deltas fall back to another representation.

Representations 2 and 4 both end in ZigZag deltas, and one byte ahead of them
says how those were written: `0` for Simple8b words, `1` for a `huff0` Huffman
block over their varint bytes, table included. The encoder writes both and
keeps the smaller. A Huffman block never borrows another block's table, because
a block that cannot be read on its own is not a block.

Version 1 changed once, on 21 September 2026, to add that byte. It was
permissible because nothing had ever been written to a disk: no release, no
tag, no store. From the first tag the rule holds and the next change is a
version 2.

The scaled path is for numbers that were written as decimals. Its stream starts
with one byte, the power of ten, and continues exactly as the integer path; the
decoder divides by that power. It divides rather than multiplying by a
reciprocal, because `10^-k` is not representable while `10^k` is, and one
correctly rounded division is one chance to differ instead of two. The scale is
at most nine, which is how far the encoder searches and not a statement about
what a metric may contain: past nine, our fixtures gained no bytes and cost
five times the encode. A corpus may move that number. The encoder accepts a
scale only when the decoder's own expression returns the original bits for
every sample in the block, so negative zero, NaN, the infinities and anything
that does not divide back exactly fall out into another representation without
a rule of their own.
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

## Metrics group directories, versions 2 and 3

These versions belong to the metrics directory, independently of the codec
version above. Version 2 was the format for newly sealed groups. Version 3
was written when publication merged adjacent groups, retaining the payload ids
already owned by their live slots. Both keep the same header:

| offset | bytes | meaning |
|---:|---:|---|
| 0 | 1 | directory version |
| 1 | 1 | block slots, 1..32 |
| 2 | 4 | live mask |
| 6 | 4 | external payload mask |
| 10 | 8 | first payload id in version 2; zero in version 3 |
| 18 | 8 | shared clock id |
| 26 | variable | compression mode and descriptor stream |
| end-4 | 4 | IEEE CRC32 bound to series id, start, end and preceding bytes |

The stream stores each block's first value, summary prediction flags, explicit
summary values where needed, reset count and body length. Version 2 addresses
external bodies as `first_id + popcount(earlier allocation bits)`. Version 3
instead follows each external body's length with its absolute payload id as an
unsigned varint; ids are positive, below MaxInt64 and unique within a directory.
Inline bodies follow their length directly in either version. Expired external
slots retain their address until the directory is merged or removed.

The descriptor stream is bounded to 8 KiB before and after compression. Group
clocks, value representations and payload checksums are unchanged. Version 3
does not cause raw values or summaries to be re-encoded. Readers accept both
versions; earlier binaries cannot read version 3. Golden vectors cover both.

## Metrics Group Directories, Version 4

New sealing and directory merges write version 4. It retains version 3's
explicit payload addresses, so merging does not relocate or decode old bodies.
The header and checksum binding stay the same; `first payload id` is zero.

Bit 6 of a block's summary flags says that exact sum and increase follow the
reset count, before body length. Bit 7 is reserved. Bit 6 is forbidden in
earlier versions. A carried old block has no exact fields and still reads raw.

Each exact value is an integer in units of `2^-1074`: an unsigned varint
magnitude length, then (for nonzero lengths) an unsigned varint containing
`exponent << 1 | negative`, followed by big-endian magnitude bytes. Zero is
one zero byte. A nonzero magnitude has no leading zero byte and is odd; the
decoder requires the complete field to match its canonical re-encoding.
Magnitude length is at most 264 bytes, and magnitude bit length plus exponent
is at most 2,106. A stored increase is nonnegative.

These fields exist only for finite gauges and finite, nonnegative counters.
They store exact internal arithmetic, not rounded float64 totals. Counter
first/last values and reset count supply transitions between adjacent blocks;
the query rounds once after combining every contribution to its bucket.

Sealing and merging keep expanded descriptors below 8 KiB with header and
compression overhead reserved. Extreme exponent mixtures may therefore
produce fewer than 32 blocks in a group. Raw payload and clock formats stay
unchanged. Golden vectors cover all three supported directory versions.
