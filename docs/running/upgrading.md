# Upgrading

One version number covers everything TinyStore releases: the Go module, the
`tinystore` binary, the npm and PyPI packages and the server image. This page
explains how a store's files, a sidecar and a remote server move to a new
version.

> [!IMPORTANT]
> **Before the first release**
> Nothing is released yet. Until the first release, a file written by one
> version may not open in the next one.

## Upgrade a package

```sh for=bun
bun update tinystore
```

```sh for=python
pip install -U tinyshed-tinystore
```

```sh for=go
go get -u github.com/tinyshed/tinystore
```

An SDK package and its binary always move together, because the binary is
inside the package.

## The files move forward

When a new version opens a store, each engine applies its new migrations to
its file, once. An older version then refuses to open that file, instead of
reading data it doesn't understand. To go back to an older version, restore a
[backup](backups.md) taken before the upgrade.

## What happens to a running sidecar

A sidecar keeps running the binary that started it until it has been idle for
30 seconds. If your program restarts sooner after an upgrade, it finds the old
sidecar still running. The new SDK replaces it by itself. The old sidecar
finishes its running calls and stops. Then the SDK starts the new binary and
prints one line about it. Other programs that use the directory reconnect to
the new sidecar by themselves.

An SDK replaces only a sidecar that is older than the SDK, never a newer one.
So two programs with different SDK versions don't keep replacing each other's
sidecar. An SDK also doesn't replace a server that a person started with
`tinystore serve ./data`, or a server that a Go program runs with
`server.Share`. For those servers, it prints a warning, and you restart them
yourself.

## Upgrade a remote server first

A server and its clients can run different versions. The rules:

- A connection uses the older protocol of the two sides.
- A newer client that uses something the server doesn't have gets an
  "unimplemented" error that names the missing field and the server's
  version. It never gets a wrong answer.
- A server answers every older client.

So upgrade the server first, and the clients after it.

## See also

- [The wire protocol](../wire.md#versions): the exact compatibility rules.
- [Backups](backups.md): take one before you upgrade.
