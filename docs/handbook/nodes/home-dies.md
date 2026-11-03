# When the home dies

A file's home is the one machine that writes it. While it runs, its copies
read and pass their writes to it. This chapter covers what happens when it
stops: moving a home on purpose, promoting a copy by hand when the home is
gone, and the epochs that keep a file from ever having two writers. As in
[Readers in another region](copies.md), the names of these options may
still change before `1.0`.

## Why nothing happens by itself

No node can tell a dead home from a cut cable: both are silence. A copy that
guessed wrong and started writing while the old home lived would make a
second home, and the file would split into two histories for good.
Automatic failover is safe only with a judge outside the two nodes that
keeps its decision durably. TinyStore's judge, the
[witness](../../guides/cli/nodes.md#witness), is a draft, so a person
decides, once they know the old home is stopped, and the command they run
refuses while the old home still answers.

## Epochs

Every change of a file's home raises the file's epoch. A node refuses pages
of an older epoch, and a home refuses writes once a later epoch reaches it
(contract REPL-08). An old home that wakes up after its copy was promoted
cannot write over the new history: the first link it makes tells it that it
is no longer the home. The epoch only grows, and every page and every write
carries it, so the older side loses every time, including writes it took
while it was cut off.

## Move a home that is alive

```rust
tinystore::replication::move_home(&store, "sql/app.db")?; // on the node that takes the file over
```

A move is a call of the Rust core alone: no SDK or command offers it yet. The
old home stops taking writes, ships its last commits, and the new home opens
its writer at the next epoch, through the catalog. No commit is lost
(contract REPL-10). Until the new home's claim reaches the catalog, the old
home can call the move off; after that it never can.

## Promote a copy when the home is gone

America's data centre is down and will not come back soon. Europe holds the
freshest copy:

```sh
# stop Europe's server first
tinystore node promote ./eu --from us.internal:7444
tinystore serve ./eu --node-listen 0.0.0.0:7444 --node-config node.toml   # without --from: it is the home now
```

`promote` refuses while America still serves the file, as [promote and
demote](../../guides/cli/nodes.md#promote-and-demote) says. The promoted
files continue at the next epoch, and nodes that copied from America try the
next address in their `from` and find Europe.

```scene
code ts
  await sessions.under(userId).set(digest(token), session)
end
actor us   "America home" at 12,50 icon=globe
actor eu   "Europe copy"  at 52,50 icon=database
actor app  "Chat"         at 90,50 icon=server
link us eu
link app eu
mark us home
mark eu copy
set us "commit 500"
set eu "commit 498"

step "America commits 499 and 500, and goes down before their pages reach Europe." line=1
  mark us down
  cut us eu
  say eu "at commit 498"

step "A person checks that America is stopped, and runs node promote on Europe. Europe becomes the home at the next epoch." line=1
  mark eu home
  set eu "commit 498, epoch 2"

step "Writes go on in Europe. Commits 499 and 500 were never shipped: they are lost, and the promotion says so." line=1
  send app -> eu "set" color=ok
  set eu "commit 499 (epoch 2)"
  say us "499, 500 lost" color=bad

step "America comes back. Its epoch is older, so it takes no writes, and node demote makes its files copies of Europe." line=1
  link us eu
  set us "copy of Europe"
  mark us copy
```

Commits 499 and 500 are lost because copies trail their home by default. A
[synchronous home](copies.md#when-a-homes-disk-must-not-lose-a-commit) loses
nothing it answered (contract REPL-09).

## Bring the old home back

```sh
tinystore node demote ./us
tinystore serve ./us --from eu.internal:7444 --node-listen 0.0.0.0:7444 --node-config node.toml
```

`demote` makes the old home's files copies again, and drops the commits it
had not shipped, as [promote and
demote](../../guides/cli/nodes.md#promote-and-demote) says.

## Why this design?

**Why the loss is counted and logged, not hidden.** A failover that loses
commits silently leaves the program believing writes that no longer exist.
TinyStore names how many were lost, and offers the synchronous mode for files
where none may be.

## See also

- [Readers in another region](copies.md): homes, copies and writes passed
  home.
- [What TinyStore promises](../production/promises.md): every contract,
  replication's among them.
