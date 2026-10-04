# KV API for Bun and Node

Every public class, function and type of the KV engine in `@tinyshed/tinystore`, generated from its source. The [KV guide](../../kv/README.md) explains how to use them, and the [Python](../python/kv.md) and [Go](../go/kv.md) pages list the same API.

## Kind

```ts
type Kind = 'json' | 'string' | 'bytes' | 'int' | 'bigint' | 'float' | 'bool' | 'none'
```

How a bucket keeps its values in a row, as Go's codecFor keeps a type's,
so that Go, Python and this SDK read each other's buckets:

    'json'     JSON text, the default         a Go struct, a Python dataclass
    'string'   its UTF-8 bytes                string, str
    'bytes'    the bytes                       []byte, bytes
    'int'      an integer, a number exactly    int64, int
    'bigint'   an integer                      int64, int
    'float'    its eight bytes, big-endian     float64, float
    'bool'     1 or 0                          bool, bool
    'none'     nothing: a bucket of them is a set    struct{}, None

## BucketOptions

```ts
interface BucketOptions {
    /** the expiry a key gets when it is written without its own */
    defaultTtl?: Duration
    /** keeps a key this long from its last read; beside defaultTtl it is refused */
    sliding?: Duration
}
```

## CounterOptions

```ts
interface CounterOptions {
    defaultTtl?: Duration
    /**
     * keeps changes in the server's memory between flushes this far apart, so
     * a crash may lose them: attempts and rates, not money
     */
    loseAtMost?: Duration
}
```

## WriteOptions

```ts
interface WriteOptions {
    ttl?: Duration
    expireAt?: Time
    /** writes only while the key is live at this version: ConflictError otherwise */
    ifVersion?: string
}
```

## Entry

```ts
interface Entry<V> {
    value: V
    /** compared only for equality: give it back to ifVersion */
    version: string
    /** when the key expires; undefined for one that never does */
    expires: Date | undefined
}
```

## Scanned

```ts
interface Scanned<V> extends Entry<V> {
    /** the key's text; bytes when it is not UTF-8 */
    key: string | Uint8Array
}
```

## Kv

```ts
class Kv {}
```

### Kv.config

```ts
config<D extends object>(name: string, defaults: D, ...layers: ConfigLayer<ConfigValue<D>>[]): Promise<Config<ConfigValue<D>>>
```

The config name, shaped and typed as its defaults, then each layer over
the one before: a file's values, `fromEnv`, and what update kept over
them all. It resolves once the server's state is read, and follows every
change from then on, whoever makes it, until the store closes.

```ts
const cfg = await store.kv.config('app', {
  port: 8080,
  db: { url: secret('DATABASE_URL'), pool: 10 },
}, file, fromEnv('APP'))
cfg.value.port                    // APP_PORT=3000 makes it 3000
await cfg.update({ port: 4000 })  // kept: 4000 after a restart too
```

### Kv.limiter

```ts
limiter(name: string, options: LimiterOptions): Limiter
```

A limiter of requests by key: `store.kv.limiter('api', { rate: '100/s', burst: 20 })`.

### Kv.quota

```ts
quota<W extends string>(name: string, windows: Record<W, Rate>): Quota<W>
```

A quota of uses by key, its windows by name, each counted from a key's
first use and all of them together or none:
`store.kv.quota('ai', { session: '100/5h', weekly: '300/7d' })`.

### Kv.once

```ts
once<T = unknown>(name: string, options?: OnceOptions): Once<T>
once<S extends StandardSchemaV1>(name: string, schema: S, options?: OnceOptions): Once<StandardSchemaV1.InferOutput<S>>
```

The answers a function gives once a key, JSON values of type T kept a
day unless defaultTtl says: `store.kv.once<Receipt>('charges')`.

### Kv.stop

```ts
stop(): void
```

Stops following configs, as the store does when it closes.

### Kv.bucket

```ts
bucket<T = unknown>(name: string, options?: BucketOptions): Bucket<T>
bucket<K extends Kind>(name: string, kind: K, options?: BucketOptions): Bucket<KindValue[K]>
bucket<S extends StandardSchemaV1>(name: string, schema: S, options?: BucketOptions): Bucket<StandardSchemaV1.InferOutput<S>>
```

A bucket of JSON values of type T.

### Kv.counters

```ts
counters(name: string, options?: CounterOptions): Counters<number>
counters(name: string, kind: 'bigint', options?: CounterOptions): Counters<bigint>
```

Counters, an int64 a key read as a number, 0 when absent.

### Kv.batch

```ts
batch<const T = void>(fn: (tx: Batch) => T): Promise<Settled<T>>
```

Runs the calls fn asks for in one transaction, all or none: the calls of
buckets it takes through withTx, whose promises settle once the batch
has. fn returns before anything is sent, since no transaction is held
across the network: a call cannot wait for another's answer inside it.
What fn returns comes back answered, an array's promises each:

    const [code] = await store.kv.batch(tx => [codes.withTx(tx).take(k), used.withTx(tx).set(k, 1)])

### Kv.view

```ts
view<const T = void>(fn: (tx: Batch) => T): Promise<Settled<T>>
```

Reads the gets and hases fn asks for from one snapshot, and gives back
what fn returns, answered as batch answers it:

    const [profile, prefs] = await store.kv.view(tx => [a.withTx(tx).get(id), b.withTx(tx).get(id)])

### Kv.tx

```ts
tx<T>(fn: (tx: Tx) => T | Promise<T>): Promise<T>
```

Runs fn as one transaction that reads before it decides what to write,
as Go's Tx does, without holding the writer across the network: a read
goes to the server at once, a write waits, and when fn returns the writes
commit in one batch that first checks every key fn read is still as it
read it. When another write changed one meanwhile, fn runs again, five
times at most before ConflictError, so it does nothing else that must
happen once. It gives back what fn returns.

    const userId = await store.kv.tx(async tx => {
      const userId = await codes.withTx(tx).take(digest(code))
      if (userId === undefined) throw new InvalidCode()
      sessions.withTx(tx).of(userId).set(digest(token), { device })
      return userId
    })

## Batch

```ts
class Batch {
    readonly kind: 'batch' | 'view'
    readonly calls: Recorded[]
}
```

The calls a batch or a view gathered, in their order.

### Batch.made

```ts
made(promise: Promise<unknown>): boolean
```

Whether the promise is one of this batch's calls', rather than an async function's.

### Batch.record

```ts
record<T>(call: Omit<Recorded, 'settle'>, settle: (entry: Entry<Raw> & {
        found: boolean
    }) => Promise<T>): Promise<T>
```

### Batch.close

```ts
close(): void
```

## Tx

```ts
class Tx implements Recorder {}
```

A transaction of store.kv.tx: a read goes to the server at once and is
remembered with its version, so that a key read again answers the same; a
write waits for the commit, and a key read after it answers what it wrote.

### Tx.record

```ts
record<T>(call: Omit<Recorded, 'settle'>, settle: (entry: Found) => Promise<T>): Promise<T>
```

### Tx.commit

```ts
commit(): Promise<void>
```

Sends what fn wrote in one batch, after a check of each key it read: a
key it found at the version it found, a key it found absent still
absent. A tx that only read checks its reads from one snapshot.

### Tx.stale

```ts
stale(err: unknown): boolean
```

Whether the commit failed for a key fn read that changed since, which another run reads anew.

## Bucket

```ts
class Bucket<V> {
    readonly name: string
}
```

A bucket's values under one branch of owners, the root when there are
none: `sessions.of(user.id).get(token)`.

### Bucket.of

```ts
of(...owners: Key[]): Bucket<V>
```

The branch below this one that the owners name.

### Bucket.withTx

```ts
withTx(tx: Batch | Tx): BucketTx<V>
```

The bucket's calls inside a batch or a view, whose promises settle with
it, or inside a tx, whose reads answer at once and writes wait for it.

### Bucket.get

```ts
get(key: Key): Promise<V | undefined>
```

The key's value, undefined when it holds none.

### Bucket.getEntry

```ts
getEntry(key: Key): Promise<Entry<V> | undefined>
```

The key's value with its version and expiry, undefined when it holds none.

### Bucket.has

```ts
has(key: Key): Promise<boolean>
```

### Bucket.set

```ts
set(key: Key, value: V, options?: WriteOptions): Promise<void>
```

Writes the value; a live key keeps its expiry unless the options give one.

### Bucket.setEntry

```ts
setEntry(key: Key, value: V, options?: WriteOptions): Promise<Entry<V>>
```

Writes the value and gives its new version and expiry.

### Bucket.setIfAbsent

```ts
setIfAbsent(key: Key, value: V, options?: Omit<WriteOptions, 'ifVersion'>): Promise<boolean>
```

Writes the value only where no live key is, and says whether it did.

### Bucket.setEntryIfAbsent

```ts
setEntryIfAbsent(key: Key, value: V, options?: Omit<WriteOptions, 'ifVersion'>): Promise<{
        created: boolean
        entry: Entry<V>
    }>
```

Writes the value only where no live key is: created says whether it did,
and entry is the new key's, or the live one's that was there.

### Bucket.delete

```ts
delete(key: Key, options?: Pick<WriteOptions, 'ifVersion'>): Promise<void>
```

### Bucket.take

```ts
take(key: Key, options?: Pick<WriteOptions, 'ifVersion'>): Promise<V | undefined>
```

Reads the value and deletes the key in one write: a one-time code read
and burned. A value that no longer decodes has been taken all the same.

### Bucket.touch

```ts
touch(key: Key, options: WriteOptions): Promise<boolean>
```

Gives a live key a new expiry, keeping its value and version; false when it holds none.

### Bucket.clear

```ts
clear(): Promise<void>
```

Removes this branch's keys and every branch under it, at once however many.

### Bucket.scan

```ts
scan(options?: {
        after?: Key
        limit?: number
    }): Promise<Page<Scanned<V>, string | Uint8Array>>
```

One page of this branch's own keys, in the byte order of their text.

### Bucket.all

```ts
all(options?: {
        limit?: number
    }): AsyncGenerator<Scanned<V>>
```

Walks this branch's own keys a page at a time, holding no snapshot
between pages: a key written during the walk may or may not be met.

## BucketTx

```ts
class BucketTx<V> {}
```

A bucket's calls inside a batch or a view.

### BucketTx.of

```ts
of(...owners: Key[]): BucketTx<V>
```

### BucketTx.get

```ts
get(key: Key): Promise<V | undefined>
```

### BucketTx.has

```ts
has(key: Key): Promise<boolean>
```

### BucketTx.set

```ts
set(key: Key, value: V, options?: WriteOptions): Promise<void>
```

### BucketTx.delete

```ts
delete(key: Key, options?: Pick<WriteOptions, 'ifVersion'>): Promise<void>
```

### BucketTx.take

```ts
take(key: Key): Promise<V | undefined>
```

## Counters

```ts
class Counters<N extends number | bigint> {
    readonly name: string
}
```

Counters under one branch: an int64 a key, 0 when absent, which add and
max change in one write; a sum past the int64 range is refused.

### Counters.of

```ts
of(...owners: Key[]): Counters<N>
```

### Counters.get

```ts
get(key: Key): Promise<N>
```

### Counters.add

```ts
add(key: Key, n?: number | bigint): Promise<N>
```

Adds n, 1 unless given, and gives the new value.

### Counters.max

```ts
max(key: Key, n: number | bigint): Promise<N>
```

Keeps the larger of the counter and n, and gives it.

### Counters.delete

```ts
delete(key: Key): Promise<void>
```

### Counters.clear

```ts
clear(): Promise<void>
```

## Rate

```ts
type Rate = `${number}/${Unit}` | `${number}/${number}${Unit}`
```

How many pass a span: '100/s', '5/10s', '1000/h', '300/7d', a span of one
unit leaving out its 1. The type checks it as it is written.

## LimiterOptions

```ts
interface LimiterOptions {
    /** how many requests pass a span, each key on its own */
    rate: Rate
    /** how many may pass at once before the rate holds the next; the rate's count when absent */
    burst?: number
}
```

## Allowance

```ts
interface Allowance {
    ok: boolean
    /** how many more requests would pass now */
    left: number
    /** milliseconds until the request would pass; 0 when ok */
    retryAfter: number
}
```

What a limiter answers a request.

## Limiter

```ts
class Limiter {
    readonly name: string
}
```

A limiter: open it with `store.kv.limiter(name, { rate })`.

### Limiter.of

```ts
of(...owners: Key[]): Limiter
```

The limiter of a branch, each key apart from the same key elsewhere.

### Limiter.allow

```ts
allow(key: Key, n?: number): Promise<Allowance>
```

Asks for n requests of key, one unless given: all pass or none does; past the burst is InvalidError.

## WindowUsage

```ts
interface WindowUsage {
    used: number
    limit: number
    left: number
    /** when the window ends and the next use starts another; undefined before it starts */
    resetAt: Date | undefined
}
```

One window of a key: what it used of its limit, and when it resets.

## QuotaUsage

```ts
interface QuotaUsage<W extends string = string> {
    ok: boolean
    /** how many more uses would pass now */
    left: number
    /** milliseconds until a refused use would pass; 0 when ok */
    retryAfter: number
    windows: Record<W, WindowUsage>
}
```

What a quota answers a key: what a limiter answers, and each window by its name.

## Quota

```ts
class Quota<W extends string = string> {
    readonly name: string
}
```

A quota: open it with `store.kv.quota(name, { session: '100/5h', weekly: '300/7d' })`.

### Quota.of

```ts
of(...owners: Key[]): Quota<W>
```

The quota of a branch, each key apart from the same key elsewhere.

### Quota.allow

```ts
allow(key: Key, n?: number): Promise<QuotaUsage<W>>
```

Uses n of key's every window, one unless given, or none when one has no
room for them; more than a window's limit is InvalidError.

    const { ok, retryAfter, windows } = await ai.allow(user.id)

### Quota.get

```ts
get(key: Key): Promise<QuotaUsage<W>>
```

Key's windows without using them: ok says whether one more use would pass now.

### Quota.refund

```ts
refund(key: Key, n?: number): Promise<void>
```

Gives n uses back, one unless given, to each of key's windows that has not reset since.

### Quota.delete

```ts
delete(key: Key): Promise<void>
```

Forgets key's windows, so that its next use starts each anew.

## OnceOptions

```ts
interface OnceOptions {
    /** how long an answer is kept: a day when absent */
    defaultTtl?: Duration
}
```

## Once

```ts
class Once<V> {
    readonly name: string
}
```

Answers kept once a key: open them with `store.kv.once(name)`.

### Once.of

```ts
of(...owners: Key[]): Once<V>
```

The answers of a branch, each key apart from the same key elsewhere.

### Once.run

```ts
run(key: Key, fn: () => V | Promise<V>): Promise<V>
```

The answer kept under key, or fn's, which is kept. A call of a key
another call is running waits for it and gets its answer; a throw of fn
keeps nothing and is thrown, so the next call runs fn again. A
connection lost after fn returned and before its answer was kept is
OutcomeUnknownError.

    const receipt = await charges.run(requestId, () => pay.charge(order, requestId))

### Once.get

```ts
get(key: Key): Promise<V | undefined>
```

The answer kept under key, undefined when none is.

### Once.delete

```ts
delete(key: Key): Promise<void>
```

Forgets the answer kept under key, so that the next run runs again.

## DeepPartial

```ts
type DeepPartial<T> = {
    [K in keyof T]?: T[K] extends readonly unknown[] ? T[K] : T[K] extends object ? DeepPartial<T[K]> : T[K]
}
```

A value of T with any of its fields, at any depth, left out.

## Setting

```ts
class Setting<V> {
    readonly kind: Kind
    readonly fallback: V | undefined
    readonly secret: boolean
    readonly variable: string | undefined
}
```

A field of a config's defaults made by `secret` or `required`, in place of its value.

## secret

```ts
function secret(variable?: string, fallback?: string): Setting<string>
```

A secret: never kept by update, and shown as *** by sources(). It reads
its own variable when given one, and is required without a default.

```ts
db: { url: secret('DATABASE_URL'), pool: 10 }
```

## required

```ts
function required(): Setting<string>
function required(kind: StringConstructor): Setting<string>
function required(kind: NumberConstructor): Setting<number>
function required(kind: BooleanConstructor): Setting<boolean>
```

A field a layer has to give, or the config does not open: a string unless kind says.

## FromEnv

```ts
class FromEnv {
    readonly prefix: string
    readonly files: readonly string[]
}
```

The environment as a layer of a config, which `fromEnv` makes.

## fromEnv

```ts
function fromEnv(prefix?: string, ...files: string[]): FromEnv
```

The environment as a config's layer: `fromEnv('APP')` reads db.pool from
APP_DB_POOL, and `fromEnv()` from DB_POOL. The .env files given are read
first, the process's own variables over them, a missing file skipped.

## Validate

```ts
class Validate {
    readonly schema: StandardSchemaV1
}
```

A config's check, which `validate` makes.

## validate

```ts
function validate(schema: StandardSchemaV1): Validate
```

Checks a config each time it is made, with zod's, valibot's or another Standard Schema.

## ConfigValue

```ts
type ConfigValue<D extends object> = {
    [K in keyof D]: FieldValue<D[K]>
}
```

A config's value, as its defaults make it: each Setting its value's type.

## ConfigLayer

```ts
type ConfigLayer<V> = DeepPartial<V> | FromEnv | Validate
```

What a config takes after its defaults, each over the one before.

## Source

```ts
interface Source {
    path: string
    /** its JSON, or *** for a secret */
    value: string
    /** 'default', 'file', 'env NAME' or 'kept' */
    from: string
    /** why a kept value is left out, when it is */
    ignored?: string
}
```

Where a field's value came from.

## Config

```ts
class Config<T extends object> {
    readonly name: string
}
```

A config: open it with `store.kv.config(name, defaults, ...layers)`.

### Config.value

```ts
get value(): T
```

The config now, frozen: update changes it.

### Config.lay

```ts
lay(layers: readonly unknown[]): Promise<void>
```

Lays each layer over the defaults, the one before it under it, and checks what is required.

### Config.update

```ts
update(change: DeepPartial<T>): Promise<void>
```

Changes the fields change names, at any depth; each one that changed is
checked, kept, and seen by every store watching the config. A change the
schema refuses, one of a secret, or one leaving a required field empty, is
InvalidError and keeps nothing.

### Config.reset

```ts
reset(...paths: string[]): Promise<void>
```

Forgets what update kept for paths, each a field or a group of them; all of it without paths.

### Config.watch

```ts
watch(fn: (value: T) => void): () => void
```

Calls fn with the config now and after each change, whoever made it; the function it returns stops that.

### Config.sources

```ts
sources(): Source[]
```

Where each field's value came from, and why a kept value is left out.

### Config.start

```ts
start(): Promise<void>
```

Follows the config's changes until the store closes; resolves once the first state is read.

### Config.stop

```ts
stop(): void
```

Stops following the config, as the store does when it closes.

<!-- Generated by task reference from sdk/js/src/kv.ts, sdk/js/src/limiter.ts, sdk/js/src/quota.ts, sdk/js/src/once.ts, sdk/js/src/config.ts. Edit the doc comments there, not this file. -->
