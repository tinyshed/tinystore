# Learn TinyStore

The Handbook builds one application, the backend of an AI chat, chapter by
chapter: from a first store on your laptop to copies of it in another region.
Each chapter adds one capability with the code that does it, in TypeScript,
Python, Go and Rust, and ends with why TinyStore works that way.

| Part               | What you learn                                                                  |
|--------------------|---------------------------------------------------------------------------------|
| Start              | what TinyStore is, a first program, the directory, and the chat we build        |
| Building the chat  | one engine per chapter: messages, sessions, jobs, files, search, settings…      |
| Under the hood     | how a write reaches the disk, how SQLite is run, how jobs, logs and search work |
| Beyond one machine | a server, copies in another region, and what to do when a home dies             |
| Production         | what TinyStore promises, every limit, and running it                            |

Read Start in order; it takes about twenty minutes. After that, every chapter
of Building the chat stands on the ones before it, and you can stop after any
of them with a program that runs.

Already know what you need? The [Guides](../guides/README.md) cover each
engine a feature at a time, and the [API reference](../reference/api.md)
lists every call.

## Start here

- [Why TinyStore](start/why.md)
- [Your first store](start/first-store.md)
