# Config API for Bun and Node

Every public class, function and type of the Config engine in `@tinyshed/tinystore`, generated from its source. The [Config guide](../../start/why.md#the-engines) explains how to use them, and the [Python](../python/config.md), [Go](../go/config.md) and [Rust](../rust/config.md) pages list the same API.

## Setting

```ts
interface Setting<V, Live extends boolean = false, Given extends boolean = true, IsSecret extends boolean = false> {
    readonly [mark]: Described
    /** what the program reads it as; nothing at run time */
    readonly '~value'?: V
    readonly '~live'?: Live
    readonly '~given'?: Given
    readonly '~secret'?: IsSecret
}
```

A setting its declaration says more of than a default would: its rules, a
secret, one a layer must give, or one that may change while the program
runs. `V` is what the program reads it as, `Live` whether it may change,
and `Given` whether it has a default or is required, as a declaration needs.

## Secret

```ts
class Secret {}
```

A setting's text that prints as *** wherever it goes; `reveal` gives it.

### Secret.reveal

```ts
reveal(): string
```

The secret's text, where the program hands it to what needs it.

### Secret.toString

```ts
toString(): string
```

### Secret.toJSON

```ts
toJSON(): string
```

## Value

```ts
type Value<D> = D extends Setting<infer V, boolean, boolean, boolean> ? V : D extends string ? string : D extends number ? number : D extends boolean ? boolean : D extends null ? null : D extends readonly (infer I)[] ? readonly Value<I>[] : D extends object ? {
    readonly [K in keyof D]: Value<D[K]>
} : never
```

What a program reads of a declaration: a setting as its value, a group as its settings.

## Changes

```ts
type Changes<D> = {
    readonly [K in keyof D as LiveIn<D[K]> extends true ? K : never]?: D[K] extends Setting<infer V, true, boolean, boolean> ? V : Changes<D[K]>
}
```

The live settings a change may name, each as the program reads it, at any depth.

## Overlay

```ts
type Overlay<D> = {
    readonly [K in keyof D]?: D[K] extends Setting<infer V, boolean, boolean, boolean> ? V extends Secret ? string : V : D[K] extends readonly unknown[] ? Value<D[K]> : D[K] extends object ? Overlay<D[K]> : Value<D[K]>
}
```

An object in code over the declaration: any of its settings, at any depth, a secret as text.

## FileLayer

```ts
interface FileLayer {
    readonly [layerMark]: 'file'
    readonly path: string
    readonly optional: boolean
}
```

A file a config reads: TOML, YAML or JSON by its extension.

## EnvLayer

```ts
interface EnvLayer {
    readonly [layerMark]: 'env'
    readonly prefix: string
    /** a setting's own variable, by its path */
    readonly vars: Readonly<Record<string, string>>
    readonly files: readonly string[]
    readonly from: Readonly<Record<string, string | undefined>> | undefined
}
```

The environment a config reads.

## Layer

```ts
type Layer<D> = FileLayer | EnvLayer | Overlay<D> | false | null | undefined
```

A layer of a config's list: an object in code, a file, the environment, or nothing.

## Vars

```ts
interface Vars {
    readonly [key: string]: string | Vars
}
```

Settings' own variables by their paths: `{ port: 'PORT', db: { url: 'DATABASE_URL' } }`.

## EnvOptions

```ts
interface EnvOptions {
    /** a setting's variable of its own, in place of its prefixed one */
    vars?: Vars | undefined
    /** .env files, whose variables count where the process has none; Bun reads .env itself */
    files?: readonly string[] | undefined
    /** these variables in place of the process's, the only environment read, as a test gives them */
    from?: Readonly<Record<string, string | undefined>> | undefined
}
```

## ConfigOptions

```ts
interface ConfigOptions<V> {
    /**
     * the program's own check of the whole value, at open and before a change
     * is kept: a Standard Schema such as zod's or valibot's, or a function
     * that throws
     */
    validate?: StandardSchemaV1 | ((value: V) => unknown) | undefined
    /** refuses to open while a kept change no longer fits, rather than ignoring it and saying so */
    strict?: boolean | undefined
}
```

## UpdateOptions

```ts
interface UpdateOptions {
    /** keeps the change only while the config is at this version, else ConflictError */
    ifVersion?: number | undefined
}
```

## ExplainLine

```ts
interface ExplainLine {
    readonly path: string
    /** as JSON, a duration spelled, a secret as *** */
    readonly value: string
    /** the layer that gave it: APP_PORT, app.yaml, layer 2, the declaration, a change */
    readonly from: string
    /** the values it went over, newest first */
    readonly over: string
}
```

A setting's line of explain.

## Explained

```ts
type Explained = readonly ExplainLine[] & {
    readonly notes: readonly string[]
}
```

Every setting's line, for console.table, and what was read and not used.

## Bounds

```ts
interface Bounds {
    min?: number | undefined
    max?: number | undefined
}
```

The bounds of a number: the smallest and the largest it may be.

## DurationBounds

```ts
interface DurationBounds {
    min?: DurationText | undefined
    max?: DurationText | undefined
}
```

The bounds of a duration, each a duration's text.

## secret

```ts
function secret(given: StringConstructor | string): Setting<Secret, false, true, true>
```

A setting that prints as *** wherever it goes and that only a layer gives:
`secret(String)` must be given, `secret('')` has a default.

## required

```ts
function required<T extends Token>(type: T): Setting<Of<T>>
function required<V>(rule: Setting<V, false, false>): Setting<V>
```

A setting with no default: a layer must give it, or the config does not open.

## live

```ts
function live<V>(rule: Setting<V, false, true>): Setting<V, true>
function live<const V extends Primitive | readonly Primitive[]>(value: V): Setting<Widened<V>, true>
```

A setting that may change while the program runs: its change is kept in
the store, goes over every layer until it is reset, and reaches every
process. `live(100)`, `live(int(100, { min: 0 }))`, `live(required(String))`.

## int

```ts
function int(rules?: Bounds): Setting<number, false, false>
function int(fallback: number, rules?: Bounds): Setting<number>
```

A whole number in its bounds: `int(8080, { min: 1, max: 65535 })`; `int({ min: 1 })` goes in required.

## oneOf

```ts
function oneOf<const O extends readonly [string, ...string[]]>(values: O): Setting<O[number], false, false>
function oneOf<const T extends string, const O extends readonly string[]>(fallback: T, others: O): Setting<T | O[number]>
```

One of a set of texts, its default first: `oneOf('info', ['debug', 'warn', 'error'])`
reads as 'info' | 'debug' | 'warn' | 'error'; `oneOf(['eu', 'us'])` goes in required.

## duration

```ts
function duration(rules?: DurationBounds): Setting<DurationText, false, false>
function duration(fallback: DurationText, rules?: DurationBounds): Setting<DurationText>
```

A duration, read as one is written: `duration('30s')`, `duration({ max: '1h' })` in required.

## list

```ts
function list<T extends Token>(item: T, options?: ListOptions<Of<T>>): Setting<readonly Of<T>[]>
function list<V>(item: Setting<V, false, boolean>, options?: ListOptions<V>): Setting<readonly V[]>
function list<G extends object>(item: G, options?: ListOptions<Value<G>>): Setting<readonly Value<G>[]>
```

A list: of `String`, `Number` or `Boolean`, of a rule, or of groups, each
item checked; its `min` and `max` count its items. A list written as a
value, `['localhost']`, needs no `list`.

## file

```ts
function file(path: string, options?: FileOptions): FileLayer
```

A file a config reads: TOML, YAML or JSON by its extension, a relative path the program's.

## env

```ts
function env(prefix: string, options?: EnvOptions): EnvLayer
```

The environment a config reads: `env('APP')` reads APP_ and each setting's
path in upper snake case, `APP_LIMITS_RPS` for limits.rps.

## Config

```ts
class Config<D> {
    readonly name: string
}
```

A program's settings as one typed value, read from memory: open one with
`store.config(name, [declaration, ...layers])`.

### Config.open

```ts
static open<D>(link: Link, name: string, layers: readonly unknown[], options: ConfigOptions<Value<D>>): Promise<Config<D>>
```

Opens a config from its list: the declaration first, then its layers in order.

### Config.value

```ts
get value(): Value<D>
```

The settings as they are now: a read of memory, so read it where it is used.

### Config.version

```ts
get version(): number
```

The version of the changes the value holds, which `ifVersion` takes.

### Config.update

```ts
update(changes: Changes<D>, options?: UpdateOptions): Promise<void>
```

Changes live settings: the ones `changes` names are kept in the store, go
over every layer until they are reset, and reach every handle of the
config. The value they make passes validate first; all are kept or none.

### Config.reset

```ts
reset(path?: string): Promise<void>
```

Drops the change kept of the setting or the group at `path`, or of every setting.

### Config.watch

```ts
watch(watcher: (next: Value<D>, previous: Value<D> | undefined) => void): () => void
```

Calls `watcher` with the settings now, then with each change and the value
before it, made in this process or another. It returns the call that stops it.

### Config.explain

```ts
explain(): Explained
```

Every setting with its value, the layer that gave it and what it went over.

### Config.stop

```ts
stop(): Promise<void>
```

Stops following the config: the store closes. It resolves once the
stream that follows it is cancelled, so that the store's connection
ends after its CANCEL: a stream the server still ran would keep a
private child, or a sidecar's connection, draining for ten seconds.

<!-- Generated by task reference from sdk/js/src/config.ts. Edit the doc comments there, not this file. -->
