# serve

`tinystore serve` shares a store with programs on this machine and, with
`--listen`, on others. It is the same program an SDK starts as a sidecar or a
private child; only its listeners differ. It runs until Ctrl+C or
`tinystore stop`.

```sh
tinystore serve ./data
tinystore serve ./data --listen tls://0.0.0.0:7443 --tls-cert cert.pem --tls-key key.pem --tokens tokens.txt
tinystore stop ./data
```

## Local

Without flags, programs on this machine find the server through
`<dir>/server/SERVE`: a Unix socket, or a named pipe on Windows, and the
secret of a proof. On every platform only the user the server runs as can
read `SERVE` or connect, whoever else may read the store's directory. No token
is needed: their permission is the file system's. If another user created
`<dir>/server/` first, `serve` refuses to start and names the folder: remove
it and start again.

| Flag            | Does                                                                        |
|-----------------|-----------------------------------------------------------------------------|
| `--stdio`       | one client on stdin and stdout, for a program that starts the server itself |
| `--idle <span>` | stop once no client has been connected this long; never by default          |

An SDK's sidecar is `serve` with `--idle 30s`, and its private child is
`serve --stdio`.

## Remote

```sh
tinystore token new admin >> tokens.txt
tinystore token new data >> tokens.txt
tinystore token new read >> tokens.txt
tinystore serve ./data --listen tls://0.0.0.0:7443 --tls-cert cert.pem --tls-key key.pem --tokens tokens.txt
```

| Flag                           | Does                                  |
|--------------------------------|---------------------------------------|
| `--listen tls://<host>:<port>` | serve other machines over TLS         |
| `--tls-cert <pem>`             | the certificate chain it answers with |
| `--tls-key <pem>`              | its private key                       |
| `--tokens <file>`              | who may connect: a token a line       |

`token new` prints a capability and a random token:

```text
read fx-o4leY8IHwDw-A-Yr-QwtChi8vDIoF94CANd9XSlk
```

| Capability | May                                                      |
|------------|----------------------------------------------------------|
| `admin`    | anything, backups included                               |
| `data`     | read and write the store                                 |
| `read`     | only reads: SQL on a reader, no migrations, no publishes |

`--listen tls://` refuses to start without a certificate, a key and a tokens
file. `--listen tcp://` sends tokens in the clear, for a private network its
operator trusts. The same server still serves this machine's programs through
its local socket.

## The store

| Flag                           | Does                                                                 |
|--------------------------------|----------------------------------------------------------------------|
| `--memory <size>`              | what the engines may hold at once, `512MiB`; 1 GiB with `--listen`   |
| `--durability full\|os`        | how far a commit goes before it returns                              |
| `--keep-free <size>`           | the disk the store keeps free; 1 GiB by default                      |
| `--encryption-key-file <file>` | the store's key, kept apart from its data                            |
| `--self-metrics`               | measure the store into its own [metrics](../metrics/self-metrics.md) |
| `--no-system-metrics`          | with `--self-metrics`, leave out the machine                         |
| `--max-series <count>`         | the metrics series the store holds at most; 100,000 by default       |

## Output

On a terminal the server prints a banner and a line per event; anywhere
else, in a container or a pipe, each line is a JSON object on stderr, ready
for a log collector.

| Flag                | Does                                 |
|---------------------|--------------------------------------|
| `--log-file <file>` | JSON lines to a file as well         |
| `--quiet`           | the banner, warnings and errors only |

`LOG_LEVEL` sets the least level that the server logs, `info` unless set.
`LOG_CONSOLE_LEVEL` sets the least level on stderr alone, and wins over
`--quiet`. For example, `LOG_CONSOLE_LEVEL=error` with `--log-file` shows
only errors on stderr and keeps every line from `info` in the file.

## In a container

```sh
docker run -v /srv/chat:/data -p 7443:7443 ghcr.io/tinyshed/tinystore:0.3 \
  serve /data --listen tls://0.0.0.0:7443 --tls-cert /data/cert.pem --tls-key /data/key.pem --tokens /data/tokens.txt
```

## Nodes

`--node-listen`, `--from` and `--node-config` make the store a node that
copies files from another or serves its own to others. See [Nodes](nodes.md).

## Stop

```sh
tinystore stop ./data
```

The clients are told to leave and the streams running finish, then the store
closes. With no server it says so and exits 1.

## See also

- [The command line](README.md).
- [Connecting](../connecting.md): how programs reach a server.
- [The server](../../handbook/nodes/server.md) in the Handbook.
