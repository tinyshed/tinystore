# Sessions and limits

The chat signs people in with a one-time code, keeps them signed in for a
month while they are active, limits how many messages a free plan sends, and
answers a request that arrives twice only once. All of it is the KV engine:
buckets of values with expiry, a rate limit, a quota and `once`. There are no
`expires_at` columns and no cleanup jobs.

## Open everything

```ts
import { createHash } from 'node:crypto'

interface Session {
	userId: number
	device: string
}

const codes = store.bucket<number>('sign-in-codes', { ttl: '15m' })
const sessions = store.bucket<Session>('sessions', { idle: '30d' })
const guesses = store.rateLimit('sign-in-guesses', { rate: '5/m', burst: 5 })
const messages = store.quota('messages', { session: '100/5h', weekly: '300/7d' })
const sends = store.once<{ id: number }>('sends', { keep: '1d' })

const digest = (secret: string) => createHash('sha256').update(secret).digest('hex')
```

```python
@dataclass
class Session:
    user_id: int
    device: str


@dataclass
class Sent:
    id: int


codes = store.bucket("sign-in-codes", int, ttl="15m")
sessions = store.bucket("sessions", Session, idle="30d")
guesses = store.rate_limit("sign-in-guesses", rate="5/m", burst=5)
messages = store.quota("messages", session="100/5h", weekly="300/7d")
sends = store.once("sends", Sent, keep="1d")


def digest(secret: str) -> str:
    return hashlib.sha256(secret.encode()).hexdigest()
```

```go
type Session struct {
	UserID int64  `json:"userId"`
	Device string `json:"device"`
}

type Sent struct {
	ID int64 `json:"id"`
}

// errors left out
codes, err := kv.OpenBucket[int64](store, "sign-in-codes", kv.TTL(15*time.Minute))
sessions, err := kv.OpenBucket[Session](store, "sessions", kv.Idle(30*24*time.Hour))
guesses, err := kv.OpenRateLimit(store, "sign-in-guesses", kv.Rate(5, time.Minute), kv.Burst(5))
messages, err := kv.OpenQuota(store, "messages",
	kv.Window("session", 100, 5*time.Hour), kv.Window("weekly", 300, 7*24*time.Hour))
sends, err := kv.OpenOnce[Sent](store, "sends", kv.Keep(24*time.Hour))

func digest(secret string) string {
	sum := sha256.Sum256([]byte(secret))
	return hex.EncodeToString(sum[:])
}
```

```rust
#[derive(Serialize, Deserialize)]
struct Session {
    user_id: i64,
    device: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct Sent {
    id: i64,
}

let codes = store.bucket::<i64>("sign-in-codes").ttl(Duration::from_mins(15)).open()?;
let sessions = store.bucket::<Session>("sessions").idle(Duration::from_hours(24 * 30)).open()?;
let guesses = store.rate_limit("sign-in-guesses").rate(5, Duration::from_mins(1)).burst(5).open()?;
let messages = store
    .quota("messages")
    .window("session", 100, Duration::from_hours(5))
    .window("weekly", 300, Duration::from_hours(24 * 7))
    .open()?;
let sends = store.once::<Sent>("sends").keep(Duration::from_hours(24)).open()?;

fn digest(secret: &str) -> String {
    format!("{:x}", Sha256::digest(secret)) // the sha2 crate
}
```

Each handle lives in the store's `kv.db`. The names say what each one is for,
and the options say how long its keys live:

| Handle            | Keys                          | Expire                               |
|-------------------|-------------------------------|--------------------------------------|
| `sign-in-codes`   | a code's digest → a user id   | 15 minutes after they are written    |
| `sessions`        | a user, then a token's digest | 30 days after the last read or write |
| `sign-in-guesses` | an IP address                 | on their own, as the rate refills    |
| `messages`        | a user id                     | each window resets on its own        |
| `sends`           | a request's idempotency key   | one day after the answer is kept     |

## Sign in with a code

The user types an email address, the chat emails a six-digit code, and the
user types the code back:

```ts
async function sendCode(email: string) {
	const user = await findOrCreateUser(email)
	const code = String(randomInt(100000, 1000000))
	await codes.set(digest(code), user.id)
	await mailer.send(email, `Your code is ${code}`)
}

async function checkCode(ip: string, code: string) {
	const guess = await guesses.allow(ip)
	if (!guess.ok) return { status: 429, retryAt: guess.retryAt }

	const userId = await codes.take(digest(code)) // undefined if wrong, used or expired
	if (userId === undefined) return { status: 401 }
	return startSession(userId)
}
```

```python
async def send_code(email: str) -> None:
    user = await find_or_create_user(email)
    code = str(secrets.randbelow(900000) + 100000)
    await codes.set(digest(code), user.id)
    await mailer.send(email, f"Your code is {code}")


async def check_code(ip: str, code: str):
    guess = await guesses.allow(ip)
    if not guess.ok:
        return Response(429, retry_at=guess.retry_at)

    user_id = await codes.take(digest(code))  # None if wrong, used or expired
    if user_id is None:
        return Response(401)
    return await start_session(user_id)
```

```go
func sendCode(ctx context.Context, email string) error {
	user, err := findOrCreateUser(ctx, email)
	if err != nil {
		return err
	}
	code := fmt.Sprintf("%06d", 100000+mathrand.IntN(900000))
	if err := codes.Set(ctx, digest(code), user.ID); err != nil {
		return err
	}
	return mailer.Send(ctx, email, "Your code is "+code)
}

func checkCode(ctx context.Context, ip, code string) (Response, error) {
	guess, err := guesses.Allow(ctx, ip)
	if err != nil {
		return Response{}, err
	}
	if !guess.OK {
		return Response{Status: 429, RetryAt: guess.RetryAt}, nil
	}
	userID, found, err := codes.Take(ctx, digest(code)) // found is false if wrong, used or expired
	if err != nil || !found {
		return Response{Status: 401}, err
	}
	return startSession(ctx, userID)
}
```

```rust
fn send_code(email: &str) -> Result<(), AppError> {
    let user = find_or_create_user(email)?;
    let code = format!("{:06}", rand::random_range(100000..1000000));
    codes.set(digest(&code), &user.id)?;
    mailer.send(email, &format!("Your code is {code}"))
}

fn check_code(ip: &str, code: &str) -> Result<Response, AppError> {
    let guess = guesses.allow(ip)?;
    if !guess.ok {
        return Ok(Response::too_many(guess.retry_at));
    }
    match codes.take(digest(code))? {
        Some(user_id) => start_session(user_id),
        None => Ok(Response::unauthorized()), // wrong, used or expired
    }
}
```

`take` reads the key and deletes it in one write. If the user clicks "Sign in"
twice, or an attacker races the user with the same code, only one of the
calls gets the user id. The other gets nothing. A code that nobody uses
expires 15 minutes after it was written, and nothing has to clean it up.

The rate limit lets one IP address try 5 codes per minute, with at most 5 at
once. A six-digit code has a million values, so guessing one at that rate
takes years. `allow` answers instead of throwing: `ok`, how many tries are
`left`, and `retryAt`, the time when the next try passes.

The bucket stores the code's digest, never the code. The same goes for
session tokens below. If someone reads `kv.db` or a backup of it, they find
nothing they can sign in with, and an error that names a key shows the digest.

## Sessions

```ts
async function startSession(userId: number, device = 'web') {
	const token = randomBytes(32).toString('base64url')
	await sessions.under(userId).set(digest(token), { userId, device })
	return { status: 200, cookie: `${userId}.${token}` }
}

async function currentUser(cookie: string) {
	const [userId, token] = cookie.split('.')
	const session = await sessions.under(userId).get(digest(token)) // renews the 30 days
	return session?.userId
}

await sessions.under(userId).delete(digest(token)) // sign out here
await sessions.under(userId).clear() // sign out everywhere
```

```python
async def start_session(user_id: int, device: str = "web"):
    token = secrets.token_urlsafe(32)
    await sessions.under(user_id).set(digest(token), Session(user_id=user_id, device=device))
    return Response(200, cookie=f"{user_id}.{token}")


async def current_user(cookie: str) -> int | None:
    user_id, token = cookie.split(".", 1)
    session = await sessions.under(user_id).get(digest(token))  # renews the 30 days
    return session.user_id if session else None


await sessions.under(user_id).delete(digest(token))  # sign out here
await sessions.under(user_id).clear()  # sign out everywhere
```

```go
func startSession(ctx context.Context, userID int64) (Response, error) {
	token := rand.Text()
	err := sessions.Under(userID).Set(ctx, digest(token), Session{UserID: userID, Device: "web"})
	return Response{Status: 200, Cookie: fmt.Sprint(userID, ".", token)}, err
}

func currentUser(ctx context.Context, cookie string) (int64, bool, error) {
	userID, token, _ := strings.Cut(cookie, ".")
	session, found, err := sessions.Under(userID).Get(ctx, digest(token)) // renews the 30 days
	return session.UserID, found, err
}

_, err = sessions.Under(userID).Delete(ctx, digest(token)) // sign out here
err = sessions.Under(userID).Clear(ctx)                    // sign out everywhere
```

```rust
fn start_session(user_id: i64) -> Result<Response, AppError> {
    let token = new_token();
    sessions.under(user_id).set(digest(&token), &Session { user_id, device: "web".into() })?;
    Ok(Response::signed_in(format!("{user_id}.{token}")))
}

fn current_user(cookie: &str) -> tinystore::Result<Option<i64>> {
    let Some((user_id, token)) = cookie.split_once('.') else { return Ok(None) };
    let session = sessions.under(user_id).get(digest(token))?; // renews the 30 days
    Ok(session.map(|session| session.user_id))
}

sessions.under(user_id).delete(digest(&token))?; // sign out here
sessions.under(user_id).clear()?; // sign out everywhere
```

`under(userId)` is a branch of the bucket: the keys that belong to one user.
"Sign out everywhere" is `clear()` on the branch. It removes every session of
that user in one call, for every reader at once, however many devices they
used.

The bucket's `idle: '30d'` means a session expires 30 days after it was last
read or written. Every `get` keeps an active user signed in, and a user who
leaves for a month is signed out without any code of yours.

## Limits per plan

A free plan sends 100 messages per 5 hours and 300 per week:

```ts
const used = await messages.allow(userId)
if (!used.ok) {
	return { status: 429, retryAt: used.retryAt, left: used.windows.weekly.left }
}
```

```python
used = await messages.allow(user_id)
if not used.ok:
    return Response(429, retry_at=used.retry_at, left=used.windows["weekly"].left)
```

```go
used, err := messages.Allow(ctx, strconv.FormatInt(userID, 10))
if err != nil {
	return err
}
if !used.OK {
	return tooMany(used.RetryAt, used.Windows["weekly"].Left)
}
```

```rust
let used = messages.allow(user_id)?;
if !used.ok {
    return Ok(Response::too_many(used.retry_at));
}
```

`allow` counts the message in both windows, or in neither. A message that is
refused because the weekly window is full does not use up the 5-hour window.
Each window starts at the user's first message and resets on its own, and the
answer says when: `windows.weekly.resetsAt` (`resets_at`, `ResetsAt`).

If the model fails and the user gets no answer, give the use back with
`messages.refund(userId)`. To look without counting, call `peek` instead of
`allow`.

## Requests sent twice

A phone sends a message, the network drops the answer, and the phone sends
the same request again. Without protection, the chat saves the message twice
and asks the model twice. The client sends an idempotency key with each
request, the same one on a retry, and `once` runs the work once per key:

```ts
const sent = await sends.run(request.headers['idempotency-key'], async () => {
	const { lastInsertRowid } = await db.exec`insert into messages (chat_id, role, text, created_at)
		values (${chatId}, 'user', ${text}, ${Date.now()})`
	return { id: Number(lastInsertRowid) }
})
```

```python
async def save() -> Sent:
    done = await db.exec(
        "insert into messages (chat_id, role, text, created_at) values (?, 'user', ?, ?)",
        chat_id, text, time.time_ns() // 1_000_000,
    )
    return Sent(id=done.last_insert_rowid)


sent = await sends.run(request.headers["idempotency-key"], save)
```

```go
sent, err := sends.Run(ctx, r.Header.Get("Idempotency-Key"), func(ctx context.Context) (Sent, error) {
	done, err := db.Exec(ctx, `insert into messages (chat_id, role, text, created_at) values (?, 'user', ?, ?)`,
		chatID, text, time.Now().UnixMilli())
	return Sent{ID: done.LastInsertRowid}, err
})
```

```rust
let sent = sends.run(&idempotency_key, || -> tinystore::Result<Sent> {
    let done = db.exec(sql!(
        "insert into messages (chat_id, role, text, created_at) values (?, 'user', ?, ?)",
        chat_id,
        text,
        unix_millis(SystemTime::now())
    ))?;
    Ok(Sent { id: done.last_insert_rowid })
})?;
```

The retry can even arrive while the first request is still running:

```scene
code ts
  const sent = await sends.run(request.headers['idempotency-key'], async () => {
    const { lastInsertRowid } = await db.exec`insert into messages …`
    return { id: Number(lastInsertRowid) }
  })
end
actor phone "Phone"     at 8,50  icon=smartphone
actor one   "Request 1" at 38,22 icon=server
actor two   "Request 2" at 38,80 icon=server
actor kv    "kv.db"     at 66,50 icon=database
actor db    "chat.db"   at 92,22 icon=database
link phone one
link phone two
link one kv
link two kv
link one db
set db "messages: 40"

step "The phone sends a message with the key k7q2. The answer is lost on the way back." line=1
  send phone -> one "POST k7q2"
  send one -> kv "run k7q2"
  set kv "k7q2: running"

step "The phone retries with the same key while Request 1 is still saving. Request 2 finds k7q2 running and waits for it." line=1
  send phone -> two "POST k7q2"
  send two -> kv "run k7q2"
  say two "waiting…"

step "Request 1 inserts the message once, and once keeps its answer under the key for a day." line=2-3
  send one -> db "insert"
  set db "messages: 41"
  send one -> kv "keep id 41"
  set kv "k7q2: id 41"

step "Request 2 gets the same answer. The phone sees message 41, and the chat saved it once." line=1
  send kv -> two "id 41" color=ok
  send two -> phone "200 id 41" color=ok
```

`run` returns the answer kept for the key, or runs the function and keeps
what it returns. A second call with the same key waits for the first one,
in this process or in another, and returns the same answer (contract KV-11).
If the function throws, nothing is kept, and the next `run` with that key
runs it again. That is what you want when the database was briefly
unavailable.

## Why this design?

**Why the keys are digests.** A sign-in code or a session token in a file is a
password in a file. A backup, a log line or an error message would carry it.
The chat stores `sha256(token)` instead: it can still find the session when
the browser sends the token, but nobody can sign in with what `kv.db`
contains. TinyStore does not hash keys for you, because most keys, such as a
user id or an IP address, are not secrets, and a digest makes them unreadable
in `tinystore kv list`.

**Why `take` instead of `get` and `delete`.** Between a `get` and a `delete`,
another request can `get` the same code. With two requests in flight, both
would sign in, and a code meant for one use would work twice. `take` reads
and deletes in one write, so of two callers exactly one gets the value.

**Why idle expiry does not cost a write per read.** If every `get` of an idle
session wrote a new expiry, every page view would be a disk sync. TinyStore
renews an idle key only after a thirtieth of its span has passed, about once
a day for 30 days. It writes the renewal together with other writes a second
later, so the `get` itself never waits for the disk. A sweep over sessions
with `list` or `all` renews nothing, so an admin page that lists sessions
does not keep them alive.

**Why a rate limit for guesses and a quota for plans.** They answer different
questions. A rate limit smooths traffic: 5 per minute, refilled
continuously, so there is no window edge where a script gets 10 tries in two
seconds. A quota counts uses against a plan, in windows that reset, because
"300 messages per week" is what a pricing page promises. Both answer with the
same shape, `ok`, `left` and `retryAt`.

**Why a rate limit is kept in memory.** Checking a rate limit on every request
must be cheap, so its state lives in memory and reaches `kv.db` every second.
A crash loses at most that last second, which can let one extra burst
through. For guesses at a sign-in code that is fine. A quota is written on
every `allow`, because a plan's limit is a promise to a customer.

**Why `once` keeps no errors, and is not "exactly once".** If the database
was down for a second, the retry must be allowed to try again, so an error
is never kept. And `once` cannot make an effect happen exactly once: if the
process dies after the insert but before the answer is kept, the next run
inserts again. It guarantees that a key runs at most once while its answer
is kept. For a payment, pass the same key to the payment provider too, so that
its own deduplication covers that gap.

## Limits and defaults

| What                               | Limit                                                 |
|------------------------------------|-------------------------------------------------------|
| a key, with its owners             | 1 KiB                                                 |
| a value                            | 1 MiB                                                 |
| a bucket's name                    | 64 bytes                                              |
| a page of `list`                   | 1,000 keys                                            |
| a quota                            | one to eight windows                                  |
| rate limits and counters in memory | 100,000 keys per name, written every second           |
| `ttl`, `idle`                      | none unless the bucket or the call sets one; not both |

## See also

- [Messages](messages.md): the database that `once` writes into.
- [Your first store](../start/first-store.md#look-inside): `tinystore kv`
  lists a bucket's keys while the chat runs.
- [KV API reference](../reference/bun/kv.md): every call of buckets, limits
  and `once`.
