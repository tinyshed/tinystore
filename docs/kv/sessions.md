# Sessions

This page builds user sessions with the KV engine: sign in, check a session on
each request, sign out on one device or on all of them, and list a user's
devices. Sessions stay alive while users are active and expire 30 days after
their last visit.

## Create a session

```ts
import { createHash, randomBytes } from 'node:crypto'

type Session = { device: string; since: number }

const sessions = store.kv.bucket<Session>('sessions', { sliding: '30d' })
const digest = (token: string) => createHash('sha256').update(token).digest('hex')

export async function signIn(userId: number, device: string) {
	const token = randomBytes(32).toString('base64url')
	await sessions.of(userId).set(digest(token), { device, since: Date.now() })
	return { userId, token } // put both in the cookie
}
```

```python
import hashlib
import secrets
import time

sessions = store.kv.bucket("sessions", Session, sliding="30d")


def digest(token: str) -> str:
    return hashlib.sha256(token.encode()).hexdigest()


async def sign_in(user_id: int, device: str) -> tuple[int, str]:
    token = secrets.token_urlsafe(32)
    await sessions.of(user_id).set(digest(token), Session(device=device, since=int(time.time())))
    return user_id, token  # put both in the cookie
```

```go
type app struct {
	sessions *kv.Bucket[Session]
}

// at startup
sessions, err := kv.OpenBucket[Session](ctx, state, "sessions", kv.Sliding(30*24*time.Hour))
a := &app{sessions: sessions}

func digest(token string) string {
	sum := sha256.Sum256([]byte(token))
	return hex.EncodeToString(sum[:])
}

func (a *app) signIn(ctx context.Context, userID int64, device string) (string, error) {
	token := rand.Text()
	err := a.sessions.Of(userID).Set(ctx, digest(token), Session{Device: device, Since: time.Now()})
	return token, err // put the user id and the token in the cookie
}
```

The cookie holds the user's id and a random token. The bucket stores only a
digest of the token, so a copy of the database file can't be used to sign
in as anyone.

## Check a session

```ts
const session = await sessions.of(cookie.userId).get(digest(cookie.token))
if (!session) {
	return redirectToSignIn()
}
```

```python
session = await sessions.of(cookie.user_id).get(digest(cookie.token))
if session is None:
    return redirect_to_sign_in()
```

```go
session, found, err := sessions.Of(c.UserID).Get(ctx, digest(c.Token))
if err != nil {
	return err
}
if !found {
	return redirectToSignIn()
}
```

Each read extends the session to 30 days from now, because the bucket has a
[sliding expiry](expiry.md). The read doesn't wait for a disk write: the new
expiry is saved in the background. An expired session is not found.

## Sign out

```ts
await sessions.of(userId).delete(digest(token)) // on this device
await sessions.of(userId).clear()               // on every device
```

```python
await sessions.of(user_id).delete(digest(token))  # on this device
await sessions.of(user_id).clear()                # on every device
```

```go
err = sessions.Of(userID).Delete(ctx, digest(token)) // on this device
err = sessions.Of(userID).Clear(ctx)                 // on every device
```

## List a user's devices

```ts
const { items } = await sessions.of(userId).scan({ limit: 20 })
for (const { value } of items) {
	console.log(value.device, new Date(value.since))
}
```

```python
page = await sessions.of(user_id).scan(limit=20)
for entry in page.items:
    print(entry.value.device, entry.value.since)
```

```go
page, err := sessions.Of(userID).Scan(ctx, kv.Query{Limit: 20})
for _, entry := range page.Entries {
	fmt.Println(entry.Value.Device, entry.Value.Since)
}
```

## Replace the token after a password change

When a user changes their password, give the current device a new token and
make the old one stop working at the same moment:

```ts
const old = await sessions.of(userId).getEntry(digest(token))
if (!old) {
	return redirectToSignIn() // the session expired or was signed out
}
const fresh = randomBytes(32).toString('base64url')

await store.kv.batch(tx => {
	sessions.withTx(tx).of(userId).delete(digest(token), { ifVersion: old.version })
	sessions.withTx(tx).of(userId).set(digest(fresh), old.value)
})
```

```python
old = await sessions.of(user_id).get_entry(digest(token))
if old is None:
    return redirect_to_sign_in()  # the session expired or was signed out
fresh = secrets.token_urlsafe(32)

async with store.kv.batch() as tx:
    sessions.of(user_id).with_tx(tx).delete(digest(token), if_version=old.version)
    sessions.of(user_id).with_tx(tx).set(digest(fresh), old.value)
```

```go
fresh := rand.Text()
err = state.Tx(ctx, func(tx *kv.Tx) error {
	session, found, err := sessions.WithTx(tx).Of(userID).Take(ctx, digest(token))
	if err != nil || !found {
		return err
	}
	return sessions.WithTx(tx).Of(userID).Set(ctx, digest(fresh), session)
})
```

Both writes succeed together or fail together. If another request changed the
session in between, the version check fails and nothing changes. Then sign out
the other devices with `clear`.

## Why there is no sessions API

Users, passwords and sign-in providers belong to your application. KV only
remembers what one request has to pass to the next. A bucket with a sliding
expiry is all a session needs.

## See also

- [Expiry](expiry.md): how sliding expiry works.
- [Transactions](transactions.md): batches in Bun and Python, `Tx` in Go.
- [Rate limits](rate-limits.md): protect the sign-in form.
- [Sign-in limits](sign-in-limits.md): limit failed sign-ins per login and per address.
