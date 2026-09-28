package spike

import (
	"bufio"
	"context"
	"crypto/rand"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	mrand "math/rand/v2"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"runtime"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/kv"
	"github.com/tinyshed/tinystore/server/internal/client"
	"github.com/tinyshed/tinystore/server/wire"
)

// the round's bucket and calls: 10,000 keys of 100 bytes, a get of a random
// key and a set of one, 1, 64 and 256 calls in flight through one connection
const (
	keys      = 10_000
	valueSize = 100
	bucket    = "b"
)

var depths = []int{1, 64, 256}

var value = make([]byte, valueSize)

func skip(t *testing.T) {
	t.Helper()
	if os.Getenv("TINYSTORE_SPIKE") != "1" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
}

// caseTime is a case's time, TINYSTORE_RPC_SECONDS as the round takes it
func caseTime() time.Duration {
	seconds, err := strconv.ParseFloat(os.Getenv("TINYSTORE_RPC_SECONDS"), 64)
	if err != nil || seconds <= 0 {
		seconds = 2
	}
	return time.Duration(seconds * float64(time.Second))
}

func key(i int) string { return fmt.Sprintf("k%05d", i) }

// storeDir is a directory of the case's own, under TINYSTORE_RPC_DIR when it
// is set, so that a container keeps the store on a volume
func storeDir(t *testing.T) string {
	t.Helper()
	root := os.Getenv("TINYSTORE_RPC_DIR")
	if root == "" {
		return t.TempDir()
	}
	if err := os.MkdirAll(root, 0o750); err != nil {
		t.Fatal(err)
	}
	dir, err := os.MkdirTemp(root, "serve-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(dir) })
	return dir
}

// fill writes the round's bucket in dir, as a program embedding the store
// would, before a server opens it
func fill(t *testing.T, dir string) {
	t.Helper()
	ctx := context.Background()
	store, err := tinystore.Open(ctx, dir, tinystore.Options{})
	if err != nil {
		t.Fatal(err)
	}
	state, err := kv.Open(ctx, store, kv.Options{})
	if err != nil {
		t.Fatal(errors.Join(err, store.Close(ctx)))
	}
	values, err := kv.OpenBucket[[]byte](ctx, state, bucket)
	if err != nil {
		t.Fatal(errors.Join(err, store.Close(ctx)))
	}
	var filling sync.WaitGroup
	var failed atomic.Pointer[error]
	for w := range 64 {
		filling.Go(func() {
			random := make([]byte, valueSize)
			for i := w; i < keys; i += 64 {
				_, _ = rand.Read(random)
				if setErr := values.Set(ctx, key(i), random); setErr != nil {
					failed.Store(&setErr)
				}
			}
		})
	}
	filling.Wait()
	if err := failed.Load(); err != nil {
		t.Fatal(errors.Join(*err, store.Close(ctx)))
	}
	if err = store.Close(ctx); err != nil {
		t.Fatal(err)
	}
}

// binary builds tinystore from cmd/tinystore, as a release builds it
func binary(t *testing.T) string {
	t.Helper()
	name := "tinystore"
	if runtime.GOOS == "windows" {
		name += ".exe"
	}
	out := filepath.Join(t.TempDir(), name)
	build := exec.Command("go", "build", "-trimpath", "-o", out, ".")
	build.Dir = filepath.Join("..", "..", "cmd", "tinystore")
	build.Env = append(os.Environ(), "CGO_ENABLED=0")
	if text, err := build.CombinedOutput(); err != nil {
		t.Fatalf("build tinystore: %v\n%s", err, text)
	}
	return out
}

// transports are the ways a sidecar is reached: the directory's own, a named
// pipe on Windows and a Unix socket elsewhere; a private child's stdio; TCP
func transports() []string {
	local := "unix"
	if runtime.GOOS == "windows" {
		local = "pipe"
	}
	return []string{local, "stdio", "tcp"}
}

// sidecar is `tinystore serve` of a filled store
type sidecar struct {
	transport string
	dir       string
	cmd       *exec.Cmd
	logs      *logs
	endpoint  string // what a client dials, but over stdio
	token     string
	stdin     io.WriteCloser
	stdout    io.ReadCloser
}

// logs keeps what the server writes on stderr, and says when a line appears
type logs struct {
	mu    sync.Mutex
	text  strings.Builder
	lines chan string
}

func (l *logs) read(from io.Reader) {
	scanner := bufio.NewScanner(from)
	for scanner.Scan() {
		l.mu.Lock()
		l.text.WriteString(scanner.Text() + "\n")
		l.mu.Unlock()
		select {
		case l.lines <- scanner.Text():
		default:
		}
	}
}

func (l *logs) String() string {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.text.String()
}

func startServe(t *testing.T, bin, dir, transport string) *sidecar {
	t.Helper()
	s := &sidecar{transport: transport, dir: dir, logs: &logs{lines: make(chan string, 64)}}
	args := []string{"serve", "--dir", dir}
	switch transport {
	case "stdio":
		args = append(args, "--stdio")
	case "tcp":
		s.token = base64.RawURLEncoding.EncodeToString(randomBytes(t, 32))
		tokens := filepath.Join(t.TempDir(), "tokens")
		if err := os.WriteFile(tokens, []byte("admin "+s.token+"\n"), 0o600); err != nil {
			t.Fatal(err)
		}
		args = append(args, "--listen", "tcp://127.0.0.1:0", "--tokens", tokens)
	default:
		args = append(args, "--local", "--idle", "0")
	}
	s.cmd = exec.Command(bin, args...)
	stderr, err := s.cmd.StderrPipe()
	if err != nil {
		t.Fatal(err)
	}
	if transport == "stdio" {
		if s.stdin, err = s.cmd.StdinPipe(); err != nil {
			t.Fatal(err)
		}
		if s.stdout, err = s.cmd.StdoutPipe(); err != nil {
			t.Fatal(err)
		}
	}
	if err = s.cmd.Start(); err != nil {
		t.Fatal(err)
	}
	go s.logs.read(stderr)
	t.Cleanup(func() {
		s.stop()
		if t.Failed() {
			t.Logf("the server's logs:\n%s", s.logs)
		}
	})
	switch transport {
	case "tcp":
		s.endpoint = s.waitServing(t)
	case "stdio":
	default:
		s.endpoint = s.waitServe(t).Endpoints[0]
	}
	return s
}

var servingAt = regexp.MustCompile(`endpoints=(\S+)`)

// waitServing reads the endpoint from the line the server logs once it listens
func (s *sidecar) waitServing(t *testing.T) string {
	t.Helper()
	deadline := time.After(20 * time.Second)
	for {
		select {
		case line := <-s.logs.lines:
			if found := servingAt.FindStringSubmatch(line); found != nil {
				return found[1]
			}
		case <-deadline:
			t.Fatal("the server logged no endpoint")
		}
	}
}

func (s *sidecar) waitServe(t *testing.T) wire.Published {
	t.Helper()
	for deadline := time.Now().Add(20 * time.Second); time.Now().Before(deadline); time.Sleep(10 * time.Millisecond) {
		text, err := os.ReadFile(filepath.Join(s.dir, "server", "SERVE"))
		var published wire.Published
		if err == nil && json.Unmarshal(text, &published) == nil {
			return published
		}
	}
	t.Fatal("no SERVE was written")
	return wire.Published{}
}

// stop ends the server: a private child by the end of its stdin, as its parent
// would; the others by force, since what they hold is the case's to throw away
func (s *sidecar) stop() {
	if s.cmd.ProcessState != nil {
		return
	}
	if s.stdin != nil {
		_ = s.stdin.Close()
		done := make(chan struct{})
		go func() {
			_ = s.cmd.Wait()
			close(done)
		}()
		select {
		case <-done:
			return
		case <-time.After(20 * time.Second):
		}
	}
	_ = s.cmd.Process.Kill()
	_ = s.cmd.Wait()
}

// stdio is the Go client's side of a private child's pipes
type stdio struct {
	io.Reader
	io.WriteCloser
}

func (s *sidecar) connect(t *testing.T) *client.Conn {
	t.Helper()
	ctx := context.Background()
	hello := wire.Hello{Client: "tinystore-go-bench"}
	var conn *client.Conn
	var err error
	switch s.transport {
	case "stdio":
		conn, err = client.New(stdio{s.stdout, s.stdin}, hello)
	case "tcp":
		hello.Token = s.token
		conn, err = client.Dial(ctx, s.endpoint, hello)
	default:
		conn, err = client.Found(ctx, s.dir, hello)
	}
	if err != nil {
		t.Fatal(err)
	}
	return conn
}

func openBucket(t *testing.T, conn *client.Conn) uint64 {
	t.Helper()
	answer, err := conn.Call(context.Background(), wire.KVOpen, wire.KVBucket{Name: bucket})
	if err != nil {
		t.Fatal(err)
	}
	var handle wire.Handle
	if err = handle.Decode(answer); err != nil {
		t.Fatal(err)
	}
	return handle.Handle
}

var errNotFound = errors.New("a key of the bucket was not found")

// found says a get's answer is an entry found: a map whose first field, 1,
// is true
func found(body []byte) bool {
	return len(body) > 2 && body[1] == 1 && body[2] == 0xc3
}

func getOp(conn *client.Conn, handle uint64) func(*mrand.Rand) error {
	return func(r *mrand.Rand) error {
		body, err := conn.Call(context.Background(), wire.KVGet, wire.KVCall{Handle: handle, Key: key(r.IntN(keys))})
		if err == nil && !found(body) {
			err = errNotFound
		}
		return err
	}
}

func setOp(conn *client.Conn, handle uint64) func(*mrand.Rand) error {
	return func(r *mrand.Rand) error {
		set := wire.KVCall{Handle: handle, Key: key(r.IntN(keys)), Value: wire.KVValue{Kind: wire.KVBytes, Bytes: value}}
		_, err := conn.Call(context.Background(), wire.KVSet, set)
		return err
	}
}

// run keeps depth calls in flight for d and says how many a second finished
func run(depth int, d time.Duration, op func(*mrand.Rand) error) (float64, error) {
	var done atomic.Bool
	var count atomic.Int64
	var failed atomic.Pointer[error]
	var running sync.WaitGroup
	began := time.Now()
	for w := range depth {
		running.Go(func() {
			r := mrand.New(mrand.NewPCG(uint64(w), 7))
			for !done.Load() {
				if err := op(r); err != nil {
					failed.Store(&err)
					return
				}
				count.Add(1)
			}
		})
	}
	time.Sleep(d)
	done.Store(true)
	running.Wait()
	if err := failed.Load(); err != nil {
		return 0, *err
	}
	return float64(count.Load()) / time.Since(began).Seconds(), nil
}

// report prints a case as the round's harness does, so that both read alike
func report(t *testing.T, who, transport, op string, depth int, ops float64) {
	t.Helper()
	t.Logf("%-14s %-6s %-9s %-5s %4d %11.0f ops/s  mean %9.1f µs", who, transport, "server", op, depth, ops,
		float64(depth)/ops*1e6)
}

func randomBytes(t *testing.T, n int) []byte {
	t.Helper()
	b := make([]byte, n)
	if _, err := rand.Read(b); err != nil {
		t.Fatal(err)
	}
	return b
}

// each transport from Go, Bun and Python, through `tinystore serve`
func TestServeTransports(t *testing.T) {
	skip(t)
	bin := binary(t)
	for i := range value {
		value[i] = 7
	}
	for _, transport := range transports() {
		dir := storeDir(t)
		fill(t, dir)
		s := startServe(t, bin, dir, transport)
		conn := s.connect(t)
		handle := openBucket(t, conn)
		for _, op := range []struct {
			name string
			run  func(*mrand.Rand) error
		}{{"get", getOp(conn, handle)}, {"set", setOp(conn, handle)}} {
			for _, depth := range depths {
				ops, err := run(depth, caseTime(), op.run)
				if err != nil {
					t.Fatal(err)
				}
				report(t, "go", transport, op.name, depth, ops)
			}
		}
		if err := conn.Close(); err != nil {
			t.Fatal(err)
		}
		if transport == "stdio" {
			s.stop()
		}
		runForeignClients(t, bin, s)
		s.stop()
	}
}

// frames are the bytes a foreign client sends, written by Go's wire so that
// no codec of theirs is measured: its HELLO, kv.open of the bucket on stream
// 1 and the handle that answers it, and a kv.get on stream 0 whose key's five
// digits begin at key
type frames struct {
	Hello  string `json:"hello"`
	Open   string `json:"open"`
	Handle string `json:"handle"`
	Get    string `json:"get"`
	Key    int    `json:"key"`
}

func framesFor(s *sidecar) (frames, error) {
	hello := wire.Hello{Protocol: wire.Protocol, Client: "tinystore-foreign-bench", Token: s.token}
	open := wire.Header{Kind: wire.KindRequest, Flags: wire.FlagEnd, Method: wire.KVOpen, Stream: 1}
	get := wire.AppendFrame(nil, wire.Header{Kind: wire.KindRequest, Flags: wire.FlagEnd, Method: wire.KVGet},
		wire.KVCall{Handle: 1, Key: key(0)}.Append(nil))
	at := strings.Index(string(get), key(0))
	if at < 0 {
		return frames{}, errors.New("no key in a kv.get")
	}
	return frames{
		Hello:  hex.EncodeToString(wire.AppendFrame(nil, wire.Header{Kind: wire.KindHello}, hello.Append(nil))),
		Open:   hex.EncodeToString(wire.AppendFrame(nil, open, wire.KVBucket{Name: bucket}.Append(nil))),
		Handle: hex.EncodeToString(wire.Handle{Handle: 1}.Append(nil)),
		Get:    hex.EncodeToString(get), Key: at + 1,
	}, nil
}

// foreign is how a client in another language is run
type foreign struct {
	name string
	argv []string
}

func foreignClients() []foreign {
	var clients []foreign
	if bun, err := exec.LookPath("bun"); err == nil {
		clients = append(clients, foreign{"bun", []string{bun, "run", "testdata/client.ts"}})
	}
	for _, name := range []string{"python3", "python"} {
		python, err := exec.LookPath(name)
		if err != nil || exec.Command(python, "-c", "pass").Run() != nil { // Windows' store alias only says it is not there
			continue
		}
		clients = append(clients, foreign{"python asyncio", []string{python, "-X", "utf8", "testdata/client.py"}})
		break
	}
	return clients
}

// runForeignClients runs the Bun and Python clients against the sidecar, or,
// over stdio, against a sidecar each starts as its own child
func runForeignClients(t *testing.T, bin string, s *sidecar) {
	t.Helper()
	given, err := framesFor(s)
	if err != nil {
		t.Fatal(err)
	}
	text, err := json.Marshal(given)
	if err != nil {
		t.Fatal(err)
	}
	command, err := json.Marshal([]string{bin, "serve", "--dir", s.dir, "--stdio"})
	if err != nil {
		t.Fatal(err)
	}
	transport, address := s.transport, s.endpoint
	switch {
	case strings.HasPrefix(address, "pipe:"):
		address = `\\.\pipe\` + strings.TrimPrefix(address, "pipe:")
	case strings.HasPrefix(address, "unix://"), strings.HasPrefix(address, "tcp://"):
		_, address, _ = strings.Cut(address, "://")
	}
	env := append(os.Environ(),
		"TINYSTORE_BENCH_TRANSPORT="+transport,
		"TINYSTORE_BENCH_ADDRESS="+address,
		"TINYSTORE_BENCH_COMMAND="+string(command),
		"TINYSTORE_BENCH_FRAMES="+string(text),
		fmt.Sprintf("TINYSTORE_RPC_SECONDS=%g", caseTime().Seconds()),
	)
	for _, client := range foreignClients() {
		runForeignClient(t, client, transport, env)
	}
}

// result is a line a foreign client prints
type result struct {
	Op      string  `json:"op"`
	Depth   int     `json:"depth"`
	Ops     float64 `json:"ops"`
	Skipped string  `json:"skipped"`
}

func runForeignClient(t *testing.T, client foreign, transport string, env []string) {
	t.Helper()
	cmd := exec.Command(client.argv[0], client.argv[1:]...)
	cmd.Env = env
	cmd.Stderr = os.Stderr
	out, err := cmd.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	if err = cmd.Start(); err != nil {
		t.Fatal(err)
	}
	lines := bufio.NewScanner(out)
	for lines.Scan() {
		var line result
		if json.Unmarshal(lines.Bytes(), &line) != nil {
			t.Logf("%s: %s", client.name, lines.Text())
			continue
		}
		if line.Skipped != "" {
			t.Logf("%-14s %-6s skipped: %s", client.name, transport, line.Skipped)
			continue
		}
		report(t, client.name, transport, line.Op, line.Depth, line.Ops)
	}
	if err = cmd.Wait(); err != nil {
		t.Errorf("%s over %s: %v", client.name, transport, err)
	}
}
