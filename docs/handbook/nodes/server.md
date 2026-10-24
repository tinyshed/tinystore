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

| Capability | May                                                                                  |
|------------|--------------------------------------------------------------------------------------|
| `admin`    | anything, backups included                                                           |
| `data`     | read and write the store                                                             |
| `read`     | call only the methods the schema marks as reads; SQL only on a reader, no migrations |

`--listen tls://` refuses to start without a certificate, a key and a tokens
file. The same server still serves programs on its own machine through its
local socket or named pipe, which need no token: their permission is the file
system's.

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

Every call after `connect` is the same as on a store opened locally. The
client checks the server's certificate before the token leaves, so a token is
never sent to a machine that only claims to be the server. A connection that
drops is made again by the SDK; a subscription comes back with `onMissed`,
and a worker's jobs return to the queue as a dead worker's would.

## In a container

The release publishes an image, `ghcr.io/tinyshed/tinystore`, with the
`tinystore` executable for amd64 and arm64:

```sh
docker run -v /srv/chat:/data -p 7443:7443 ghcr.io/tinyshed/tinystore:0.1 \
  serve /data --listen tls://0.0.0.0:7443 --tls-cert /data/cert.pem --tls-key /data/key.pem --tokens /data/tokens.txt
```

On a terminal the server prints a banner and one line per event; anywhere
else, in a container or a pipe, each line is a JSON object on stderr, ready
for any log collector.

## Look at it while it runs

The command line reads a store beside the server that holds it, without
stopping it and without writing a byte:

```sh
tinystore status ./data                        # what the store holds and who serves it
tinystore logs ./data -f                       # the application's logs as they arrive
tinystore sql ./data chat "select count(*) from messages"
tinystore backup ./data backups/now.zip
tinystore stop ./data                          # stop the server once its work is finished
```

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
