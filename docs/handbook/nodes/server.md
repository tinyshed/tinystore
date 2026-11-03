# The server

When the chat runs on more than one machine, the programs on the other
machines need to reach the store. `tinystore serve` serves a store's directory
over TLS to clients that connect with a token. The clients use the same SDK
and the same calls: only the `open` line changes.

## Serve a store

```sh
tinystore token new admin >> tokens.txt
tinystore token new data >> tokens.txt
tinystore serve ./data --listen tls://0.0.0.0:7443 --tls-cert cert.pem --tls-key key.pem --tokens tokens.txt
```

`tinystore token new` prints a line with a capability and a random token:

```text
data hBVwqW1DHNHzhxCuwnzuCl6vSeta8REkKpp7M04IQ5k
```

A token's capability is `admin`, `data` or `read`. Programs on the server's
own machine need no token. [serve](../../guides/cli/serve.md#remote) says what
each capability may do, and lists every flag.

## Connect

```ts
import { connect } from '@tinyshed/tinystore'

await using store = await connect('tls://db.internal:7443', { token: process.env.TINYSTORE_TOKEN! })
```

```python
store = await tinystore.connect("tls://db.internal:7443", token=os.environ["TINYSTORE_TOKEN"])
```

```go
store, err := tinystore.Connect(ctx, "tls://db.internal:7443", os.Getenv("TINYSTORE_TOKEN"))
```

```rust
// A Rust program runs the store in its own process: Store::open.
```

[Connecting](../../guides/connecting.md#a-remote-server) says what the SDK
does when a connection drops.

## In a container

The release publishes an image, `ghcr.io/tinyshed/tinystore`, with the
`tinystore` executable for amd64 and arm64: [serve](../../guides/cli/serve.md#in-a-container)
shows the command that runs it, and [Output](../../guides/cli/serve.md#output)
what the server prints there.

## Look at it while it runs

The command line reads a store beside the server that holds it, as
[Running it](../production/running-it.md) shows, and `tinystore stop ./data`
stops the server once its work is finished.

## Why this design?

**Why a server is the same program as the sidecar.** The sidecar that a Bun
program starts on one machine and the server that other machines reach are
the same `tinystore serve` with different listeners. A program tested against
a private child, moved to a sidecar and then to a remote server runs the same
core and the same protocol all the way.

**Why tokens with three capabilities and nothing finer.** Rules per bucket or
per table become a policy language that nobody reads and everyone gets
wrong. A store serves one application, so the useful lines are few: the
application itself, a read-only dashboard or analyst, and the operator.

**Why TLS by default.** A token in clear text on a network is a token for
anyone on that network. `--listen tcp://` exists for a private network its
operator trusts, and it sends the token as it is; everywhere else, use
`tls://`, where the client checks the certificate before the token leaves.

## See also

- [One core, four languages](../inside/one-core.md): the protocol a server
  speaks.
- [Readers in another region](copies.md): nodes that copy a store's files.
