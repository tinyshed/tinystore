# Once

`once` runs a function only once per key and keeps its result. If the same
request arrives again with the same idempotency key, it gets the stored result
instead of running the function a second time. Use it for payments, orders and
any other action that a retried request must not repeat.

## Charge a card once

```ts
const charges = store.kv.once<Receipt>('charges')

const key = request.headers.get('Idempotency-Key')!
const receipt = await charges.run(key, () => payments.charge(order, key))
```

```python
charges = store.kv.once("charges", Receipt)

key = request.headers["Idempotency-Key"]
receipt = await charges.run(key, lambda: payments.charge(order, key))
```

```go
charges, err := kv.OpenOnce[Receipt](ctx, state, "charges")

key := r.Header.Get("Idempotency-Key")
receipt, err := charges.Run(ctx, key, func(ctx context.Context) (Receipt, error) {
	return payments.Charge(ctx, order, key)
})
```

The first `run` of a key calls the function and stores its result. Every later
`run` of the same key returns the stored result without calling the function.
A result is kept for one day by default. Change this with the bucket's default
TTL (`kv.DefaultTTL`, `defaultTtl`, `default_ttl`).

## Requests that arrive together

If a client sends the same request twice at the same moment, the second `run`
waits for the first one to finish and returns its result. Only one `run` of a
key runs at a time, across all processes and clients of the store.

A waiting `run` stops waiting when its own context, task or signal is
cancelled. The first `run` keeps going.

## Errors are not stored

If the function throws or returns an error, nothing is stored, and the next
`run` of the key calls the function again. This is what you want for a
network error.

If you want a failure to be repeated, return it as a value instead. For
example, return a receipt with `declined: true` for a declined card, and the
next request with the same key gets the same answer.

## Pass the key along

A claim on a key lives in memory while its function runs. If the process
crashes during the function, the claim is gone, and the next `run` calls the
function again. If the function has already charged the card, the card is
charged twice.

To prevent this, pass the same key to the other service as its own
idempotency key, as the examples do with `payments.charge(order, key)`. Most
payment providers accept one.

## Read or forget a result

```ts
const stored = await charges.get(key) // the result, or undefined
await charges.delete(key)             // the next run calls the function again
```

```python
stored = await charges.get(key)  # the result, or None
await charges.delete(key)        # the next run calls the function again
```

```go
receipt, found, err := charges.Get(ctx, key)
err = charges.Delete(ctx, key) // the next Run calls the function again
```

## In Bun and Python

The function runs in your process, not in the server. The server hands the
key's turn to your process, and your process sends the result back to be
stored. If the connection breaks after the function returned but before the
result was stored, `run` fails with an outcome unknown error
(`OutcomeUnknownError`).

## Limits and defaults

|                  |                                              |
|------------------|----------------------------------------------|
| A result is kept | 1 day, unless the default TTL says otherwise |
| A result         | the same limits as a KV value, at most 1 MiB |

## See also

- [Versions](versions.md): claim a key yourself with `setEntryIfAbsent`.
- [kv/README.md](../../kv/README.md#once): the full contract of `once`.
