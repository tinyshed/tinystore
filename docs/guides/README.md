# Guides

The Guides cover TinyStore one engine at a time, for when you know what you
need: how keys expire, how a job retries, how a search filters. Each engine
has an overview, then a page per feature, every example in TypeScript,
Python, Go and Rust. The [Handbook](../handbook/README.md) explains the same
engines as parts of one application, and why they work as they do.

| Engine                         | For                                                                  | In the Handbook                                                                 |
|--------------------------------|----------------------------------------------------------------------|---------------------------------------------------------------------------------|
| [SQL](sql/README.md)           | your own SQLite databases: migrations, queries, transactions         | [Messages](../handbook/chat/messages.md)                                        |
| [KV](kv/README.md)             | values with expiry and versions; counters, rate limits, quotas, once | [Sessions and limits](../handbook/chat/sessions-and-limits.md)                  |
| [Jobs](jobs/README.md)         | work later or on a schedule, retried, with steps                     | [Replies in the background](../handbook/chat/replies.md)                        |
| [Blobs](blobs/README.md)       | files by path, written whole, checked when read                      | [Attachments](../handbook/chat/attachments.md)                                  |
| [Search](search/README.md)     | words in any language, typos, vectors, filters                       | [Finding old messages](../handbook/chat/finding-old-messages.md)                |
| [Channels](channels/README.md) | messages to whoever listens now                                      | [Live updates](../handbook/chat/live-updates.md)                                |
| [Locks](locks/README.md)       | one holder of a key at a time, with fences                           | [Live updates](../handbook/chat/live-updates.md#one-summary-per-chat-at-a-time) |
| [Config](config/README.md)     | settings from files and the environment, some changed live           | [Settings](../handbook/chat/settings.md)                                        |
| [Records](records/README.md)   | logs and events, kept and read back                                  | [What it is doing](../handbook/chat/what-it-is-doing.md)                        |
| [Metrics](metrics/README.md)   | counters, gauges, timers and histograms, read exactly                | [How fast it is](../handbook/chat/how-fast-it-is.md)                            |
| [Backup](backup/README.md)     | the whole store as one checked zip, taken while it runs              | [Keeping it](../handbook/chat/keeping-it.md)                                    |

The [command line](cli/README.md) has a guide of its own: serving a store,
reading it while it runs, backups and nodes.

Every engine opens from the store, which [Your first
store](../handbook/start/first-store.md) opens in each language.
