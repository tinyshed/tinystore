# Sign-in limits

Use a KV quota to limit sign-in attempts before checking a password. This
example allows five attempts per 15 minutes and fifty per day, per login
and per address. Every attempt counts, including successful sign-ins.

## Reserve an attempt before checking

```ts
const attempts = store.kv.quota('signin-attempts', { burst: '5/15m', day: '50/24h' })
const byLogin = attempts.of('login')
const byAddress = attempts.of('address')

export async function signIn(login: string, address: string, password: string) {
	for (const check of [{ quota: byLogin, key: login }, { quota: byAddress, key: address }]) {
		const usage = await check.quota.allow(check.key)
		if (!usage.ok) {
			return { status: 429, retryAfter: usage.retryAfter }
		}
	}
	const user = await users.find(login)
	if (user === undefined || !(await verify(user.passwordHash, password))) {
		return { status: 401 }
	}
	await byLogin.delete(login) // a correct password clears this login's attempts
	return { status: 200, user }
}
```

```python
attempts = store.kv.quota("signin-attempts", burst="5/15m", day="50/24h")
by_login = attempts.of("login")
by_address = attempts.of("address")


async def sign_in(login: str, address: str, password: str) -> tuple[int, float]:
    for quota, key in ((by_login, login), (by_address, address)):
        usage = await quota.allow(key)
        if not usage.ok:
            return 429, usage.retry_after
    user = await users.find(login)
    if user is None or not verify(user.password_hash, password):
        return 401, 0
    await by_login.delete(login)  # a correct password clears this login's attempts
    return 200, 0
```

```go
attempts, err := kv.OpenQuota(ctx, state, "signin-attempts",
	kv.Window("burst", 5, 15*time.Minute), kv.Window("day", 50, 24*time.Hour))
byLogin, byAddress := attempts.Of("login"), attempts.Of("address")

func signIn(ctx context.Context, login, address, password string) (int, time.Duration, error) {
	for _, check := range []struct {
		quota *kv.Quota
		key   string
	}{{byLogin, login}, {byAddress, address}} {
		usage, err := check.quota.Allow(ctx, check.key)
		if err != nil {
			return 0, 0, err
		}
		if !usage.OK {
			return http.StatusTooManyRequests, usage.RetryAfter, nil
		}
	}
	user, found, err := users.Find(ctx, login)
	if err != nil {
		return 0, 0, err
	}
	if !found || !verify(user.PasswordHash, password) {
		return http.StatusUnauthorized, 0, nil
	}
	return http.StatusOK, 0, byLogin.Delete(ctx, login)
}
```

Each `allow` checks and counts an attempt atomically. A full window returns
`ok: false` with the time until it resets. Use that duration in a
`Retry-After` header.

The login and address are separate writes. If the address refuses an attempt,
the login's count is kept. This can block a login sooner, but cannot admit
more attempts than either limit allows.

## Check before you hash the password

A password hash such as Argon2 is slow and takes memory on purpose. Call
`allow` before looking up the user or verifying the password. A refused
attempt then performs neither operation.

Do not replace `allow` with `get` followed by a later write. Several
concurrent requests can all read an unused quota before any of them updates
it, then all start checking passwords.

## Count every attempt

Wrong passwords, successful checks and errors after admission all count.
A correct password clears the login's count with `delete`. The address
keeps its count, including successful attempts, so one address cannot reset
its limit by signing into an account that it owns.

Choose limits that fit legitimate sign-ins too. Users behind one shared
address share its limit.

## Why a login and an address

| Limit       | Stops                                    | Shared by                 |
|-------------|------------------------------------------|---------------------------|
| per login   | guessing one account from many addresses | attempts for that login   |
| per address | one address guessing many accounts       | users behind that address |

`of` keeps the two counts apart inside one quota. A login and an address
with the same text never share a count. The windows reset independently for
each key, and TinyStore deletes a key after all of its windows reset.

## Limits and defaults

|              |                                                      |
|--------------|------------------------------------------------------|
| Burst window | 5 attempts per 15 minutes, per login and per address |
| Daily window | 50 attempts per 24 hours, per login and per address  |
| Each allow   | one write, saved to disk before it returns           |
| One attempt  | up to two allow calls, checked in order              |

Change the numbers to fit your application. See [Quotas](quotas.md) for
every option.

## See also

- [Quotas](quotas.md): windows, refunds and reading usage.
- [Sessions](sessions.md): what to create once the password is right.
- [kv/README.md](../../kv/README.md#quota): the full contract of quotas.
