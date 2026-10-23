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
new, empty bucket, when the buckets with sealed values are in the same file or
in a database that the program has open. A new key would seal values that the
old key cannot open once you put it back. Put the key back, and every bucket
opens again.

> [!CAUTION]
> **Lose the key and the values are gone**
> Nothing recovers them. Keep a copy of the key apart from the data. A backup
> leaves the key out unless asked, and says so with `keyLeftOut`.

## See also

- [KV](README.md): the engine's overview.
- [Backup](../backup/README.md): a backup with or without the key.
