# The `.wire` language

The files here are the wire protocol's schema: every message and every
method, a file an engine, and `connection.wire` and `server.wire` for what no
engine owns. The codecs of every language and the test vectors are written
from them, so a message is declared once, here.

This page is the whole language. The bytes a message becomes are
[docs/wire.md](../docs/wire.md); why the protocol has its shape is
[plan/protocol.md](../plan/protocol.md).

```wire
# What a call on a key carries. A comment above a message documents it.
message kv.Call {
  1 handle: uint
  2 under: [key]  # a comment after a field documents the field
  3 key: key
  4 value: value?
}

message kv.Entry {
  1 found: bool
  2 value: value?
}

message Empty {}

read  0x0102 kv.get(kv.Call) -> kv.Entry
# A comment above a method documents it.
write 0x0104 kv.set(kv.Call) -> Empty
```

## Lines

A file is read a line at a time, and a line is one of six things:

| Line    | Looks like                                | What it is                                                               |
|---------|-------------------------------------------|--------------------------------------------------------------------------|
| blank   |                                           | a separator; it ends the comment above it                                |
| comment | `# text`                                  | the doc of the message or the method right below; in a message, a note   |
| message | `message kv.Call {`                       | opens a message; `message Empty {}` is a whole message without fields    |
| field   | `4 value: value?  # doc`                  | a number, a name and a type, inside a message                            |
| end     | `}`                                       | closes the message                                                       |
| method  | `read 0x0102 kv.get(kv.Call) -> kv.Entry` | who may call it, its number, its name, what it takes and what it answers |

Only a field takes a comment after it. A message's or a method's goes above,
with no blank line between, and becomes its doc comment in the code written.

## Messages and fields

A message's name starts with its engine: `kv.Call` is `KvCall` in Rust and in
TypeScript, and its Rust code is behind the feature `kv`. A name without an
engine, `Hello`, `Failure`, `Handle`, `Empty`, is the connection's own, which
every engine may use.

A field is `number name: type`:

- **The number is what goes on the wire**, from 1 to 127, once a message. A
  name may change freely and a number may not, once a release has it.
- **The name** is camelCase, and Rust spells it in snake_case: `expiresAt` is
  `expires_at`.
- **The type** is one of the table below. With `?` the field is absent when
  not given. Without it the field is absent at its zero value and reads back
  as that value.

## Types

| Type       | On the wire                               | Rust                  | TypeScript             |
|------------|-------------------------------------------|-----------------------|------------------------|
| `bool`     | true or false                             | `bool`                | `boolean`              |
| `uint`     | an integer, not negative                  | `u64`                 | `number`               |
| `int`      | an integer                                | `i64`                 | `number`               |
| `int64`    | an integer past what a `number` holds     | `i64`                 | `bigint`               |
| `float`    | a 64-bit float                            | `f64`                 | `number`               |
| `str`      | text                                      | `String`              | `string`               |
| `bin`      | bytes                                     | `Vec<u8>`             | `Uint8Array`           |
| `duration` | a uint of milliseconds                    | `u64`                 | `number`               |
| `time`     | an int of Unix milliseconds               | `i64`                 | `number`               |
| `nanos`    | an int of Unix nanoseconds                | `i64`                 | `bigint`               |
| `key`      | str, or bin when not UTF-8, or an integer | `String`              | `string \| Uint8Array` |
| `value`    | nil, an integer or bin: a kv row          | `Row`                 | `Raw`                  |
| `cell`     | nil, an integer, a float, str or bin      | `Cell`                | `SqlValue \| boolean`  |
| `json`     | str holding JSON                          | `String`              | `string`               |
| `[T]`      | an array of `T`                           | `Vec<T>`              | `T[]`                  |
| `{T}`      | a map of names to `T`                     | `BTreeMap<String, T>` | `Record<string, T>`    |
| `kv.Call`  | the message of that name                  | `KvCall`              | what `KvCall` reads    |
| `T?`       | absent when not given                     | `Option<T>`           | `T \| undefined`       |

A value nests at most eight levels, which is what the MessagePack profile
reads; the schema is refused when a message could hold one deeper.

## Methods

```wire
read  0x0302 sql.query(sql.Query) -> download sql.Rows until sql.Rows
write 0x0131 kv.once.run(kv.Call) -> handover kv.Answer
write 0x0209 jobs.work(jobs.Work) -> exchange jobs.Held for jobs.Answer
admin 0x0001 server.stop(Empty)   -> Empty
```

**The first word says who may call it**, and there is no default, so that no
method is declared without saying:

| Word    | Who may call it                                                             |
|---------|-----------------------------------------------------------------------------|
| `read`  | any connection, one a server admitted to read only among them               |
| `write` | a connection that may write: it changes what a client wrote                 |
| `admin` | an admin's connection alone: the server's own calls, a stop or a clock move |

The server refuses by these words, so marking a method `read` is a promise
that it changes nothing a client wrote.

**The number** is `0x` and four hex digits, once in the schema. Its first
byte is the engine, `00` the server, `01` kv, `02` jobs, `03` sql and `04`
blobs, and its second the method.

**The name** is the call as the engine's book has it: `kv.bucket.open` is
`method::KV_BUCKET_OPEN` in Rust and `methods['kv.bucket.open']` in
TypeScript. **The request** is one message.

**The answer** says the shape of the method's stream:

| Answer               | Shape    | What goes on the stream                                                              |
|----------------------|----------|--------------------------------------------------------------------------------------|
| `T`                  | call     | one `T`, which ends it                                                               |
| `download T until U` | download | any number of `T`, then one `U` that ends it                                         |
| `handover T`         | handover | a `T` that ends it, or the run handed to the client, which sends the `T` to keep     |
| `exchange T for U`   | exchange | `T` from the server and `U` from the client, both ways, until each side ends its own |

`until` and `for` are words of their shapes and mean nothing alone. A test
holds each method to its line: the session must run it as the shape says.

## What is refused

`just protocol` stops at the first line that is not the language's, naming
the file and the line. It then refuses a schema in which:

- a message, a field's number in its message, or a method's number is
  declared twice;
- a field or a method names a message that is not declared;
- a field's number is not 1 to 127;
- a message holds itself, or nests a value past eight levels;
- there is no `Failure`, the message every failed stream ends with.

## The layout

`just protocol` writes every file back in one layout, and `just
protocol-check`, which CI runs, fails on a file that is not in it:

- a run of methods, the lines up to a blank one, has its calls in one column
  and its answers in the next;
- a file's field comments start in one column, two past its longest
  commented field;
- a field is indented two spaces, with one space between its parts.

Comments and blank lines stay where they were.

## What is written from it

| File                                    | What it is                                                       |
|-----------------------------------------|------------------------------------------------------------------|
| `crates/tinystore/src/wire/protocol.rs` | the core's messages, method numbers and who may call each        |
| `sdk/js/src/wire/protocol.ts`           | the Bun and Node SDK's messages and method numbers               |
| `testdata/wire/protocol.json`           | a vector of every message, which Rust and every SDK's tests read |

All three are committed. Never edit one: change the `.wire` file and run
`just protocol`.

## Where the language lives

| Path                                     | What it holds                                                      |
|------------------------------------------|--------------------------------------------------------------------|
| `crates/protocol/src/syntax.rs`          | the one parser, and the language's words                           |
| `crates/protocol/src/format.rs`          | the layout                                                         |
| `crates/protocol/src/schema.rs`          | the files joined, and what is refused                              |
| `crates/protocol/src/agreement_tests.rs` | the tests that hold this page and the editor to the parser's words |
| [editors/wire](../editors/wire)          | highlighting and snippets for VS Code and RustRover                |

A new word goes into `syntax.rs` first. The tests then fail until the editor's
grammar, its snippets and this page have it too.

## Adding a method

1. Declare its messages and its line in the engine's file, under the next
   free number.
2. Run `just protocol`.
3. Answer it in `crates/tinystore/src/wire/<engine>.rs`, and say in that
   file's `route` how it runs when it is not a plain call on a worker.
4. Call it from the SDK, through `methods` and the message's codec.
5. Test it over the pipe; `every_method_of_the_schema_is_answered` fails
   until step 3 is done.
