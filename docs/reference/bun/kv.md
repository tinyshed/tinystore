# KV API for Bun and Node

Every public class, function and type of the KV engine in `@tinyshed/tinystore`, generated from its source. The [KV guide](../../chat/sessions-and-limits.md) explains how to use them, and the [Python](../python/kv.md), [Go](../go/kv.md) and [Rust](../rust/kv.md) pages list the same API.

## ValueType

```ts
type ValueType = 'string' | 'bytes' | 'int' | 'bigint' | 'float' | 'bool'
```

How a bucket keeps a value that is not JSON, as the core keeps a Rust type,
so that every language reads the others' rows:

    'string'   its UTF-8 bytes
    'bytes'    the bytes
    'int'      an integer a number holds exactly
    'bigint'   an integer past what a number holds
    'float'    its eight bytes, big-endian, every bit kept
    'bool'     1 or 0

## BucketOptions

```ts
interface BucketOptions {
    /** a key expires this long after it was written, unless its write gives its own */
    ttl?: Duration | undefined
    /** a key expires this long after it was last read or written; not beside ttl */
    idle?: Duration | undefined
    /** how values are kept when they are not JSON */
    type?: ValueType | undefined
    /**
     * Keeps every value sealed with the store's encryption key, as a password or a
     * token is kept: the file holds what nobody reads without the key. The
     * store seals and opens, so this program never holds the key. A key is not
     * sealed, so a secret is a value and never a key, and a name keeps whether
     * it is encrypted.
     */
    encrypted?: boolean | undefined
}
```

## WriteOptions

```ts
interface WriteOptions {
    /** the key expires this long from now */
    ttl?: Duration | undefined
    /** the key expires at this time */
    expiresAt?: Time | undefined
    /** writes only while the key is at this version: ConflictError otherwise */
    ifVersion?: string
}
```

## Entry

```ts
interface Entry<T> {
    key: string
    value: T
    /** compared only for equality: give it back to ifVersion */
    version: string
    /** undefined for a key that never expires */
    expiresAt: Date | undefined
}
```

## Page

```ts
interface Page<T> {
    entries: Entry<T>[]
    next: string | undefined
}
```

A page of a branch's keys, and where the next starts when there is one.

## CountersOptions

```ts
interface CountersOptions {
    /** a counter lasts this long from its first add */
    ttl?: Duration | undefined
    /**
     * keeps counts in memory and writes them this often, so that an add costs
     * no commit and a crash loses at most this span: attempts and hits, not money
     */
    flushEvery?: Duration | undefined
}
```

## Bucket

```ts
class Bucket<T> {
    readonly name: string
}
```

A bucket's values under one branch of owners, the bucket's own when there are none.

### Bucket.under

```ts
under(...owners: Key[]): Bucket<T>
```

The branch the owners name below this one: whose keys these are.

### Bucket.get

```ts
get(key: Key): Promise<T | undefined>
```

The key's value, undefined when it holds none.

### Bucket.entry

```ts
entry(key: Key): Promise<Entry<T> | undefined>
```

The key's value with its version and expiry, undefined when it holds none.

### Bucket.has

```ts
has(key: Key): Promise<boolean>
```

### Bucket.set

```ts
set(key: Key, value: T, options?: WriteOptions): Promise<void>
```

Writes the value; a key that exists keeps its expiry unless the options give one.

### Bucket.create

```ts
create(key: Key, value: T, options?: Omit<WriteOptions, 'ifVersion'>): Promise<boolean>
```

Writes the value only where no live key is, and says whether it did.

### Bucket.add

```ts
add(key: Key, options?: Omit<WriteOptions, 'ifVersion'>): Promise<boolean>
```

Puts the key in a bucket used as a set; false when it was there.

### Bucket.take

```ts
take(key: Key, options?: Pick<WriteOptions, 'ifVersion'>): Promise<T | undefined>
```

Reads the value and removes the key in one commit: of two callers taking
a one-time code at once, one gets it.

### Bucket.delete

```ts
delete(key: Key, options?: Pick<WriteOptions, 'ifVersion'>): Promise<boolean>
```

Removes the key, and says whether it was there.

### Bucket.expire

```ts
expire(key: Key, when: Duration | Date, options?: Pick<WriteOptions, 'ifVersion'>): Promise<boolean>
```

Gives a live key a new expiry, a span from now or a time, keeping its
value; false when it holds none.

### Bucket.clear

```ts
clear(): Promise<void>
```

Removes this branch's keys and every branch under it, at once however many.

### Bucket.list

```ts
list(options?: {
        limit?: number | undefined
        after?: Key | undefined
    }): Promise<Page<T>>
```

One page of this branch's own keys, in the byte order of their text: `"10"` before `"9"`.

### Bucket.all

```ts
all(options?: {
        limit?: number | undefined
    }): AsyncGenerator<Entry<T>>
```

Walks this branch's own keys a page at a time, holding nothing between
pages: a key written during the walk may or may not be met. It renews no
idle key.

## Counters

```ts
class Counters {
    readonly name: string
}
```

Numbers by key that only add up: 0 for one never added to or expired.

### Counters.under

```ts
under(...owners: Key[]): Counters
```

### Counters.add

```ts
add(key: Key, n?: number | bigint): Promise<number>
```

Adds n, 1 unless given, and gives the new count.

### Counters.get

```ts
get(key: Key): Promise<number>
```

### Counters.delete

```ts
delete(key: Key): Promise<boolean>
```

Removes the counter, and says whether it was there.

### Counters.clear

```ts
clear(): Promise<void>
```

Removes this branch's counters and every branch's under it.

## Rate

```ts
type Rate = `${number}/${Unit}` | `${number}/${number}${Unit}`
```

How many pass a span: '100/s', '5/10s', '1000/h', '300/7d', a span of one
unit leaving out its 1. The type checks it as it is written.

## RateLimitOptions

```ts
interface RateLimitOptions {
    /** how many requests a key passes a span */
    rate: Rate
    /** how many may pass at once before the rate holds the next; the rate's count when absent */
    burst?: number | undefined
}
```

## WindowUse

```ts
interface WindowUse {
    used: number
    limit: number
    left: number
    /** when the window resets; undefined for one not started */
    resetsAt: Date | undefined
}
```

One window of a quota as a key stands in it.

## Allowance

```ts
interface Allowance<W extends string = never> {
    ok: boolean
    /** how many more would pass now */
    left: number
    /** when a refused request may try again; undefined when it passed */
    retryAt: Date | undefined
    /** a quota's windows by name; none for a rate limit */
    windows: Record<W, WindowUse>
}
```

What a rate limit or a quota answers a request.

## RateLimit

```ts
class RateLimit {
    readonly name: string
}
```

A rate of requests a key: open it with `store.rateLimit(name, { rate })`.

### RateLimit.under

```ts
under(...owners: Key[]): RateLimit
```

The rate limit of a branch, each key apart from the same key elsewhere.

### RateLimit.allow

```ts
allow(key: Key, n?: number): Promise<Allowance>
```

Asks for n requests of key, one unless given: all pass or none; more than the burst is InvalidError.

### RateLimit.peek

```ts
peek(key: Key): Promise<Allowance>
```

The answer one request would get, using nothing.

### RateLimit.reset

```ts
reset(key: Key): Promise<void>
```

Forgets key, so that its next request starts afresh.

## Quota

```ts
class Quota<W extends string = string> {
    readonly name: string
}
```

Uses by key in named windows: open it with `store.quota(name, { daily: '100/1d' })`.

### Quota.under

```ts
under(...owners: Key[]): Quota<W>
```

### Quota.allow

```ts
allow(key: Key, n?: number): Promise<Allowance<W>>
```

Uses n of key's every window, one unless given, or none when one has no
room; more than a window's limit is InvalidError.

    const { ok, retryAt, windows } = await ai.allow(user.id)

### Quota.peek

```ts
peek(key: Key): Promise<Allowance<W>>
```

### Quota.reset

```ts
reset(key: Key): Promise<void>
```

### Quota.refund

```ts
refund(key: Key, n?: number): Promise<void>
```

Gives n uses back to every window of key, one unless given.

## OnceOptions

```ts
interface OnceOptions {
    /** how long an answer is kept: a day when absent */
    keep?: Duration | undefined
}
```

## Once

```ts
class Once<T> {
    readonly name: string
}
```

Answers kept once a key: open them with `store.once(name)`.

### Once.under

```ts
under(...owners: Key[]): Once<T>
```

The answers of a branch, each key apart from the same key elsewhere.

### Once.run

```ts
run(key: Key, fn: () => T | Promise<T>): Promise<T>
```

The answer kept for key, or fn's, which is kept. A run of a key that
another runs, here or in another process, waits for it and gets its
answer; a throw of fn keeps nothing and is thrown, so the next run runs
fn again. A connection lost after fn returned and before its answer was
kept is OutcomeUnknownError.

    const receipt = await charges.run(requestId, () => pay.charge(order, requestId))

### Once.get

```ts
get(key: Key): Promise<T | undefined>
```

The answer kept for key, undefined when none is.

### Once.delete

```ts
delete(key: Key): Promise<boolean>
```

Forgets the answer kept for key, so that its next run runs fn again.

## Tx

```ts
class Tx {}
```

The calls of a transaction, each through `tx.with(handle)`.

### Tx.with

```ts
with<T>(bucket: Bucket<T>): TxBucket<T>
with(counters: Counters): TxCounters
with<T>(queue: Queue<T>): StoreTxQueue<T>
with<T>(channel: Channel<T>): TxChannel<T>
```

### Tx.read

```ts
read(parts: Parts, key: Key): Promise<Found>
```

What the key holds as this transaction sees it: its own writes over what the store held.

### Tx.write

```ts
write(method: Method, parts: Parts, fields: CallFields, found?: Found): void
```

Keeps a write for the commit; found is what a read of its key answers after it.

### Tx.clear

```ts
clear(parts: Parts): void
```

### Tx.commit

```ts
commit(): Promise<void>
```

Commits the writes, then hands over the messages published inside.

## TxBucket

```ts
class TxBucket<T> {}
```

A bucket's calls inside a transaction: reads answer at once, writes wait for its commit.

### TxBucket.under

```ts
under(...owners: Key[]): TxBucket<T>
```

### TxBucket.get

```ts
get(key: Key): Promise<T | undefined>
```

### TxBucket.entry

```ts
entry(key: Key): Promise<Entry<T> | undefined>
```

### TxBucket.has

```ts
has(key: Key): Promise<boolean>
```

### TxBucket.set

```ts
set(key: Key, value: T, options?: WriteOptions): Promise<void>
```

### TxBucket.create

```ts
create(key: Key, value: T, options?: Omit<WriteOptions, 'ifVersion'>): Promise<boolean>
```

Writes the value only where no live key is, and says whether it will.

### TxBucket.add

```ts
add(key: Key, options?: Omit<WriteOptions, 'ifVersion'>): Promise<boolean>
```

### TxBucket.take

```ts
take(key: Key, options?: Pick<WriteOptions, 'ifVersion'>): Promise<T | undefined>
```

### TxBucket.delete

```ts
delete(key: Key, options?: Pick<WriteOptions, 'ifVersion'>): Promise<boolean>
```

### TxBucket.expire

```ts
expire(key: Key, when: Duration | Date, options?: Pick<WriteOptions, 'ifVersion'>): Promise<boolean>
```

### TxBucket.clear

```ts
clear(): Promise<void>
```

Removes this branch's keys and every branch's under it, with the commit.

## TxCounters

```ts
class TxCounters {}
```

Counters inside a transaction: an add commits with its writes.

### TxCounters.under

```ts
under(...owners: Key[]): TxCounters
```

### TxCounters.add

```ts
add(key: Key, n?: number | bigint): Promise<void>
```

Adds n, 1 unless given, with the commit; the count is known once it has.

<!-- Generated by task reference from sdk/js/src/kv.ts, sdk/js/src/limits.ts, sdk/js/src/once.ts, sdk/js/src/tx.ts. Edit the doc comments there, not this file. -->
