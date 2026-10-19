# Nodes

A store becomes a node when `serve` is given `--node-listen` or `--from`: it
keeps a log of each file it writes, serves those files to other nodes, and
copies the files of the node it takes from. Every file has one home that
writes it; copies read locally and pass their writes home. The commands here
start nodes, show how they stand, and move a home when one is gone.

```sh
# the home
tinystore serve ./us --node-listen 0.0.0.0:7444 --node-config node.toml
# a copy of every file of the home
tinystore serve ./eu --from us.internal:7444 --node-listen 0.0.0.0:7444 --node-config node.toml
```

```toml title="node.toml"
token-file = "/etc/tinystore/node-token" # the token nodes show each other
advertise = "eu.internal:7444"           # where other nodes reach this one

[tls]
cert = "/etc/tinystore/node.pem"
key = "/etc/tinystore/node.key"
roots = "/etc/tinystore/ca.pem"
```

| Flag                        | Does                                             |
|-----------------------------|--------------------------------------------------|
| `--node-listen <host:port>` | where other nodes copy from this one             |
| `--from <host:port>`        | a node it copies from; again for the next to try |
| `--node-config <file.toml>` | the rest of what the node is                     |

The config's rarer keys: `files`, `token-file`, `catalog`, `advertise`,
`synchronous`, `synchronous-with`, `failover`, `insecure` and `[tls]`'s
`cert`, `key` and `roots`. A link beyond its own machine takes TLS unless
`insecure = true`.

## node status

```sh
tinystore node status ./eu
tinystore node status ./eu --json
```

It shows the node's files, which it writes and which it copies, its links,
and how far behind its home each copy is. It reads beside the running node.

## promote and demote

When a home is gone and will not come back soon, a person promotes a copy:

```sh
# stop the copy's server first
tinystore node promote ./eu --from us.internal:7444
tinystore serve ./eu --node-listen 0.0.0.0:7444 --node-config node.toml # without --from: it is the home now
```

| Command                     | Does                                                     |
|-----------------------------|----------------------------------------------------------|
| `node promote <dir> [file]` | makes a stopped node's copies its own, at the next epoch |
| `node demote <dir> [file]`  | makes an old home's files copies again, before it serves |

- The node must be stopped.
- `promote` asks each node named by `--from` first. One that still serves the
  file would make a second home, so the promotion is refused unless
  `--force`.
- Commits the old home had not shipped are lost with it, unless the home was
  synchronous. `demote` drops, counts and logs the ones the old home still
  holds, and follows the new home before it opens a writer.

```sh
tinystore node demote ./us
tinystore serve ./us --from eu.internal:7444 --node-listen 0.0.0.0:7444 --node-config node.toml
```

## witness

A draft: a witness keeps each file's lease and no data, and gives the lease to
the freshest copy when a home goes quiet. Nodes name it as `failover` in their
config.

```sh
tinystore witness ./witness --listen 0.0.0.0:7445 --token-file token
```

| Flag                      | Does                          |
|---------------------------|-------------------------------|
| `--listen <host:port>`    | where nodes reach it          |
| `--token-file <file>`     | the token nodes say to it     |
| `--tls-cert`, `--tls-key` | its certificate and key       |
| `--insecure`              | plain TCP beyond this machine |

## See also

- [The command line](README.md).
- [Readers in another region](../../handbook/nodes/copies.md) and [When a
  home dies](../../handbook/nodes/home-dies.md) in the Handbook.
