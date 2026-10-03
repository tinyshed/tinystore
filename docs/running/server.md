# A remote server

Serve a store to programs on other machines over TLS, with tokens that decide
what each client may do. The server is the same `tinystore serve` that runs
as a sidecar, and clients use the same SDK calls.

## Start a server

```sh
tinystore serve /srv/data --listen tls://0.0.0.0:7443 \
  --tls-cert /etc/tinystore/cert.pem --tls-key /etc/tinystore/key.pem \
  --tokens /etc/tinystore/tokens.txt
```

```text title="tokens.txt"
admin 4dGQ6tR2kq0SxVZpLWn8E1yHf7cJmB3aUoN9Tz5Ki0E
data  Yq8wHn2xLz5Rb7Tc0Vm3Kj6Fd9Gs1Ap4Ue8Wo2Ni5Qt
```

The server checks its flags, its certificate and its tokens before it opens
the store, so a mistake leaves no files behind. A server with `--listen`
limits the memory its engines use to 1 GiB, unless `--memory` sets another
limit.

## Connect

```ts
import { connect } from '@tinyshed/tinystore'

await using store = await connect('tls://db.internal:7443', { token: process.env.TINYSTORE_TOKEN! })
```

```python
async with tinystore.connect("tls://db.internal:7443", token=os.environ["TINYSTORE_TOKEN"]) as store:
    ...
```

The client checks the server's certificate before it sends the token. For a
certificate from your own authority, pass it with `tls: { ca }` in Bun, or an
`ssl.SSLContext` in Python.

A plain `tcp://` address works too, but then the token travels unencrypted.
Use it only on a network you trust.

## Tokens

Each line of the tokens file is a capability and a token:

| Capability | May |
|---|---|
| `admin` | everything: read and write data, run migrations, repair damaged records, stop the server |
| `data` | read and write data, open databases whose migrations are already applied |

A token is 32 random bytes in base64url without padding. Generate one with
`python -c "import secrets; print(secrets.token_urlsafe(32))"`.

A `data` client can't change a schema. Its SQL must be a single statement
that starts with `SELECT`, `VALUES`, `WITH`, `INSERT`, `REPLACE`, `UPDATE` or
`DELETE`, and the server also checks the compiled statement, so a schema
change hidden in a comment or a trigger is refused too. A `data` client's
statement may run for at most 30 seconds.

Run migrations from your deploy step with an `admin` token. The application
itself can then run with a `data` token: it checks that its migrations are
applied, but can't apply them.

## Run it in Docker

Each release publishes an image that serves `/data` on `tls://` port 7443. It reads `cert.pem`, `key.pem` and `tokens` from
`/etc/tinystore`:

```sh
docker run -d -p 7443:7443 -v tinystore-data:/data -v ./secrets:/etc/tinystore:ro \
  ghcr.io/tinyshed/tinystore
```

> [!NOTE]
> **Release candidates**
> `latest` names the newest release that is not a release candidate, so until
> `v0.1.0` it names nothing. Pull a release candidate by its version instead,
> such as `ghcr.io/tinyshed/tinystore:0.1.0-rc.1`.

## When a connection drops

A client reconnects by itself, opens its buckets and queues again, and a
`work` loop continues. What was in flight when the connection dropped:

| In flight | After the drop |
|---|---|
| a read | may be sent again |
| a write | fails with an outcome unknown error: read what you wrote before you retry |
| a blob upload | is aborted and leaves nothing |
| a job in a worker's hands | its attempt fails, as if the worker crashed, and it is retried |

There is no transaction that stays open across the network, so a client that
disappears never blocks other writers.

## See also

- [The sidecar](sidecar.md): the local server.
- [Go, Bun and Python](../languages.md): how each language connects.
- [design/server.md](https://github.com/tinyshed/research/blob/main/tinystore/design/server.md)
  in the research repository: the server's design and limits.
