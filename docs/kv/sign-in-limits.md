# Sign-in limits

This page limits failed sign-ins with a KV quota: five failures per 15
minutes and fifty per day, counted for each login and for each address. A
correct password clears the login's failures. Every application with
passwords needs this, and it takes one quota and a few lines.

## Check, then count failures

```ts
const failures = store.kv.quota('signin-failures', { burst: '5/15m', day: '50/24h' })
const byLogin = failures.of('login')
const byAddress = failures.of('address')

export async function signIn(login: string, address: string, password: string) {
	for (const usage of [await byLogin.get(login), await byAddress.get(address)]) {
		if (!usage.ok) {
			return { status: 429, retryAfter: usage.retryAfter } // before the costly hash
		}
	}
	const user = await users.find(login)
	if (user === undefined || !(await verify(user.passwordHash, password))) {
		await byLogin.allow(login)
		await byAddress.allow(address)
		return { status: 401 }
	}
	await byLogin.delete(login) // a correct password clears the login's failures
	return { status: 200, user }
}
```

```python
failures = store.kv.quota("signin-failures", burst="5/15m", day="50/24h")
by_login = failures.of("login")
by_address = failures.of("address")


async def sign_in(login: str, address: str, password: str) -> tuple[int, float]:
    for usage in (await by_login.get(login), await by_address.get(address)):
        if not usage.ok:
            return 429, usage.retry_after  # before the costly hash
    user = await users.find(login)
    if user is None or not verify(user.password_hash, password):
        await by_login.allow(login)
        await by_address.allow(address)
        return 401, 0
    await by_login.delete(login)  # a correct password clears the login's failures
    return 200, 0
```

```go
failures, err := kv.OpenQuota(ctx, state, "signin-failures",
	kv.Window("burst", 5, 15*time.Minute), kv.Window("day", 50, 24*time.Hour))
byLogin, byAddress := failures.Of("login"), failures.Of("address")

func signIn(ctx context.Context, login, address, password string) (int, time.Duration, error) {
	for _, check := range []struct {
		quota *kv.Quota
		key   string
	}{{byLogin, login}, {byAddress, address}} {
		usage, err := check.quota.Get(ctx, check.key)
		if err != nil {
			return 0, 0, err
		}
		if !usage.OK {
			return http.StatusTooManyRequests, usage.RetryAfter, nil // before the costly hash
		}
	}
	user, found, err := users.Find(ctx, login)
	if err != nil {
		return 0, 0, err
	}
	if !found || !verify(user.PasswordHash, password) {
		if _, err = byLogin.Allow(ctx, login); err == nil {
			_, err = byAddress.Allow(ctx, address)
		}
		return http.StatusUnauthorized, 0, err
	}
	return http.StatusOK, 0, byLogin.Delete(ctx, login) // a correct password clears the login's failures
}
```

`get` reads a key's windows without counting anything, and `allow` counts one
failure in every window at once. A full window answers `ok: false` with the
time until it resets, which goes into a `Retry-After` header.

## Check before you hash the password

A password hash such as Argon2 is slow and takes memory on purpose. If you
hashed first and checked the limit afterwards, an attacker could make your
server hash as fast as they can send requests. Checking first with `get` costs
one read, so a refused attempt never reaches the hash.

## Count failures only

Only a wrong password calls `allow`. A user who signs in correctly every day
never comes near the limit, and a correct password deletes the login's
failures with `delete`. The address keeps its count, because one address that
guesses many accounts is the attack the address's limit stops.

## Why a login and an address

| Limit       | Stops                                    | Costs                                                 |
|-------------|------------------------------------------|-------------------------------------------------------|
| per login   | guessing one account from many addresses | the real user waits up to 15 minutes after five tries |
| per address | one address guessing many accounts       | users behind one shared address share the limit       |

`of` keeps the two counts apart inside one quota, so a login and an address
with the same text never share a count. The windows reset on their own, and
TinyStore deletes a key's data when all of its windows have reset, so the
quota needs no cleanup job.

## Limits and defaults

|              |                                                      |
|--------------|------------------------------------------------------|
| Burst window | 5 failures per 15 minutes, per login and per address |
| Daily window | 50 failures per 24 hours, per login and per address  |
| Each `get`   | one read                                             |
| Each `allow` | one write, saved to disk before it returns           |

Change the numbers to fit your application. See [Quotas](quotas.md) for every
option of a quota.

## See also

- [Quotas](quotas.md): windows, refunds and reading usage.
- [Sessions](sessions.md): what to create once the password is right.
- [kv/README.md](../../kv/README.md#quota): the full contract of quotas.
