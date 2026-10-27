# Encrypted buckets

An encrypted bucket keeps its values sealed with the store's encryption key:
API tokens of integrations, passwords of the sources a program connects to.
The file, a backup or a stolen disk image shows none of them. Every call is
the same as on any bucket.

```ts
const passwords = store.bucket<string>('source-passwords', { encrypted: true })

await passwords.set(source.id, 'hunter2') // kv.db holds what only the key opens
await passwords.get(source.id) // 'hunter2'
```

```python
passwords = store.bucket("source-passwords", str, encrypted=True)

await passwords.set(source.id, "hunter2")
await passwords.get(source.id)  # "hunter2"
```

```go
passwords, err := kv.OpenBucket[string](store, "source-passwords", kv.Encrypted())

err = passwords.Set(ctx, source.ID, "hunter2")
password, found, err := passwords.Get(ctx, source.ID)
```

```rust
let passwords = store.bucket::<String>("source-passwords").encrypted().open()?;

passwords.set(&source.id, &"hunter2".to_string())?;
let password = passwords.get(&source.id)?;
```

## What is sealed

- **A value is encrypted; its key, its bucket's name and its size are not.**
  Keep secrets in values, never in keys.
- **A value opens only where it was written.** It is bound to its bucket, its
  branch and its key: a value copied to another place reads `corrupt`
  (contract SEC-03).
- **The store encrypts and decrypts.** A client of a server sends the value
  and gets it back as any other; so a token that writes reads every value,
  and a `read` token opens no encrypted bucket.
- **A bucket's name keeps whether it is encrypted.** Opening it the other way
  is `invalid`. To encrypt what a plain bucket holds, write it into a new
  bucket.

The cipher is XChaCha20-Poly1305 with a random nonce per value.

## The encryption key

| Where the key is                          | Who makes it                                                    |
|-------------------------------------------|-----------------------------------------------------------------|
| `encryption.key` in the store's directory | the store, when the first encrypted bucket opens                |
| the file `encryptionKeyFile` names        | you, `openssl rand -hex 32`: the store reads it, never makes it |

```ts
const store = await open('./data', { encryptionKeyFile: '/run/secrets/tinystore.key' })
```

```python
store = await tinystore.open("data", encryption_key_file="/run/secrets/tinystore.key")
```

```go
store, err := tinystore.Open(ctx, "data", tinystore.EncryptionKeyFile("/run/secrets/tinystore.key"))
```

```rust
let store = Store::open("data", Options { encryption_key_file: Some("/run/secrets/tinystore.key".into()), ..Options::default() })?;
```

The key the store makes lies beside the data, so a copy of the whole
directory reads everything. Keep the key apart with `encryptionKeyFile` when a
stolen disk or a copied directory is the worry.

The store never makes a new key in place of a lost one. If `encryption.key`
is gone while buckets still hold values sealed with it, opening an encrypted
bucket fails with an `io` error that names the missing key. This includes a
new, empty bucket, wherever the sealed values are: in `kv.db` or in any
database of the store's directory, whether the program has opened it yet or
not. Put the key back, and every bucket opens again. If the key is lost for
good, put a new one there yourself, as the next section shows.

## Older keys

Each value records which key sealed it. The store seals new values with one
key, and opens a value with whichever of its keys sealed it. The keys that
only open are its older keys:

```ts
const store = await open('./data', {
	encryptionKeyFile: '/run/secrets/tinystore-2026.key',
	olderEncryptionKeys: ['/run/secrets/tinystore-2025.key'],
})
const moved = await passwords.reseal() // 120: the values the 2025 key sealed, now sealed with the 2026 key
```

```python
store = await tinystore.open(
    "data",
    encryption_key_file="/run/secrets/tinystore-2026.key",
    older_encryption_keys=["/run/secrets/tinystore-2025.key"],
)
moved = await passwords.reseal()  # 120
```

```go
store, err := tinystore.Open(ctx, "data", tinystore.EncryptionKeyFile("/run/secrets/tinystore-2026.key"),
	tinystore.OlderEncryptionKeys("/run/secrets/tinystore-2025.key"))
moved, err := passwords.Reseal(ctx) // 120
```

```rust
let store = Store::open("data", Options {
    encryption_key_file: Some("/run/secrets/tinystore-2026.key".into()),
    older_encryption_keys: vec!["/run/secrets/tinystore-2025.key".into()],
    ..Options::default()
})?;
let moved = passwords.reseal()?; // 120
```

- **To rotate a key**, make a new one with `openssl rand -hex 32`, give it as
  the encryption key, and move the old one to the older keys. Call `reseal`
  on each encrypted bucket. Once every bucket is resealed, remove the old key
  from the options.
- **`reseal` writes a batch of values per commit.** Each value keeps its
  version and its expiry, so a conditional write after it still matches.
- **After a restore without the key**, put a new key where the store reads
  it. The old values stay in their buckets: a read of one fails with an
  `invalid` error that names the key that sealed it. New values are sealed
  with the new key in the same bucket, so you can enter the secrets again.
- **If the old key turns up later**, add it to the older keys. Its values
  open again, beside the ones the new key sealed, and `reseal` moves them to
  the new key.
- A value sealed with a key that the store doesn't hold stops `reseal` with
  an `invalid` error that names the key. The batches before it stay
  resealed, and a `reseal` after you add the key continues.

> [!CAUTION]
> **Lose the key and the values are gone**
> Nothing recovers them. Keep a copy of the key apart from the data. A backup
> leaves the key out unless asked, and says so with `keyLeftOut`. A backup
> takes the sealing key only, so it also says `keyLeftOut` while a value is
> sealed with an older key.

## See also

- [KV](README.md): the engine's overview.
- [Backup](../backup/README.md): a backup with or without the key.
