# When the home dies

A file's home is the one machine that writes it. While it runs, its copies
read and pass their writes to it. This chapter covers what happens when it
stops: moving a home on purpose, promoting a copy by hand when the home is
gone, and the epochs that keep a file from ever having two writers.

> [!NOTE]
> **Built, with draft names**
> Moving a home, `node promote`, `node demote` and synchronous homes are
> built. A witness that promotes a copy by itself exists as a draft; this
> chapter describes the manual promotion.

## Why nothing happens by itself

No node can tell a dead home from a cut cable: both are silence. A copy that
guessed wrong and started writing while the old home lived would make a
second home, and the file would split into two histories for good. So in the
first version a person decides, once they know the old home is stopped.

## Epochs

Every change of a file's home raises the file's epoch. A node refuses pages
of an older epoch, and a home refuses writes once a later epoch reaches it
(contract REPL-08). An old home that wakes up after its copy was promoted
cannot write over the new history: the first link it makes tells it that it
is no longer the home.

## Move a home that is alive

```rust
tinystore::replication::move_home(&store, "sql/app.db")?; // on the node that takes the file over
```

The old home stops taking writes, ships its last commits, and the new home
opens its writer at the next epoch, through the catalog. No commit is lost
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

`promote` asks each node named by `--from` first. If America still answers
and serves the file, `promote` refuses, because promoting then would make a
second home; `--force` overrides that, for a person who knows better. The
promoted files continue at the next epoch, and nodes that copied from America
try the next address in their `from` and find Europe.

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

By default a home answers a write after its own commit, so the commits that
had not reached a copy when the home died are lost with it. That is the
trade-off of asynchronous copies. A synchronous home, from [Readers in another
region](copies.md#when-a-homes-disk-must-not-lose-a-commit), loses nothing
it answered: every success means a copy held the write (contract REPL-09).

## Bring the old home back

```sh
tinystore node demote ./us
tinystore serve ./us --from eu.internal:7444 --node-listen 0.0.0.0:7444 --node-config node.toml
```

`demote` makes the old home's files copies again. The commits it had not
shipped are dropped, counted and logged, and it follows the new home before
it opens any writer.

## Why this design?

**Why a person and not automatic failover.** Automatic failover is only safe
with something outside the two nodes that decides, and that keeps the
decision durably: a judge. Without one, a network cut that hides the home
from its copy looks exactly like a dead home. TinyStore's witness is that
judge, and it is a draft. Until it is promised, the safe default is a command
that a person runs, and that refuses when the old home still answers.

**Why epochs.** A promotion is a decision that must win over everything the
old home does afterwards, including writes it accepted while it was cut off.
A number that only grows, carried by every page and every write, makes the
older side lose every time, without asking anyone.

**Why the loss is counted and logged, not hidden.** A failover that loses
commits silently leaves the program believing writes that no longer exist.
TinyStore names how many were lost, and offers the synchronous mode for files
where none may be.

## See also

- [Readers in another region](copies.md): homes, copies and writes passed
  home.
- [What TinyStore promises](../production/promises.md): every contract,
  replication's among them.
