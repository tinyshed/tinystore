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

## Metrics group directories

A directory belongs to the metrics engine, independently of the codec version
above, and lists the blocks of one group. Its version is 4: nothing earlier was
released, and no earlier directory is read.

| offset | bytes | meaning |
|---:|---:|---|
| 0 | 1 | directory version, 4 |
| 1 | 1 | block slots, 1..32 |
| 2 | 4 | live mask |
| 6 | 4 | external payload mask |
| 10 | 8 | shared clock id |
| 18 | variable | compression mode and descriptor stream |
| end-4 | 4 | IEEE CRC32 bound to series id, start, end and preceding bytes |

The stream holds, for each slot, the block's first value, its summary flags,
the summary values that differ from their prediction, its reset count, its
exact sum and increase when flag bit 6 says it has them, and its body length.
An inline body follows its length; an external one is named by its payload id,
an unsigned varint, positive, below MaxInt64 and unique within the directory.
An expired external slot keeps its address until the directory is merged or
removed, and a merge keeps every payload id it carries, so it rewrites the
directory without relocating or decoding a body. Bit 7 of the flags is
reserved.

An exact value is an integer in units of `2^-1074`: an unsigned varint
magnitude length, then, for a nonzero length, an unsigned varint holding
`exponent << 1 | negative` and the big-endian magnitude. Zero is one zero byte.
A nonzero magnitude has no leading zero byte and is odd, and the decoder
requires the whole field to equal its canonical encoding. A magnitude holds at
most 264 bytes, and its bit length plus its exponent at most 2,106. A stored
increase is never negative.

Only a block of finite gauge values, or of finite counter values none below
zero, has exact sums; any other reads raw. They are exact arithmetic, not
rounded float64 totals, and an aggregate rounds once, after every block and
sample of its bucket. A counter's first and last values and its reset count
carry the transitions between adjacent blocks.

The stream is bounded to 8 KiB before and after compression. Sealing and
merging reserve room for the header and the compression, so a mixture of
extreme exponents may hold fewer than 32 blocks in a group. Clocks, value
representations and payload checksums do not depend on the directory.
`TestDirectoriesWrittenBeforeStillRead` holds a vector of each kind of block,
inline and external, and `FuzzNewFormats` reads what it is given.
