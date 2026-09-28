package spike

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"math/rand/v2"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"runtime/pprof"
	"strconv"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

// The round docs/server.md asks for before server/wire is built: what a call
// through a sidecar costs from Go, Bun and Python over each transport, who
// should write a connection's frames, what a call costs the server, and the
// credit a stream needs. The sidecar is this test binary, run again with its
// configuration in rpcSidecarVariable; see rpc_sidecar_spike_test.go.

var rpcDepths = []int{1, 64, 256}

var rpcSidecars atomic.Int64

func rpcSkip(t *testing.T) {
	t.Helper()
	if os.Getenv("TINYSTORE_SPIKE") != "1" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
}

func rpcDuration() time.Duration {
	seconds, err := strconv.ParseFloat(os.Getenv("TINYSTORE_RPC_SECONDS"), 64)
	if err != nil || seconds <= 0 {
		seconds = 2
	}
	return time.Duration(seconds * float64(time.Second))
}

func rpcTransports() []string {
	if rpcHasPipes {
		return []string{"unix", "tcp", "stdio", "pipe"}
	}
	return []string{"unix", "tcp", "stdio"}
}

// rpcConfigFor is a sidecar of its own directory, listening where transport says
func rpcConfigFor(t *testing.T, transport, writer string) rpcSidecarConfig {
	t.Helper()
	dir := rpcDir(t)
	config := rpcSidecarConfig{Transport: transport, Writer: writer, Dir: dir}
	switch transport {
	case "tcp":
		config.Address = "127.0.0.1:0"
	case "unix":
		config.Address = filepath.Join(dir, "s.sock")
	case "pipe":
		config.Address = rpcPipeName(fmt.Sprint(os.Getpid(), "-", rpcSidecars.Add(1)))
	}
	return config
}

// rpcDir is a directory of the case's own, under TINYSTORE_RPC_DIR when it is
// set, so that a container keeps its files on a volume rather than its overlay
func rpcDir(t *testing.T) string {
	t.Helper()
	root := os.Getenv("TINYSTORE_RPC_DIR")
	if root == "" {
		return t.TempDir()
	}
	if err := os.MkdirAll(root, 0o750); err != nil {
		t.Fatal(err)
	}
	dir, err := os.MkdirTemp(root, "rpc-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(dir) })
	return dir
}

func rpcLog(t *testing.T, client, transport, writer, op string, depth int, ops float64) {
	t.Helper()
	t.Logf("%-14s %-6s %-9s %-5s %4d %11.0f ops/s  mean %9.1f µs", client, transport, writer, op, depth, ops,
		float64(depth)/ops*1e6)
}

var rpcValue = bytes.Repeat([]byte{7}, rpcValueSize)

func rpcEchoOp(c *rpcClient) func(*rand.Rand) error {
	return func(*rand.Rand) error {
		_, err := c.call(rpcEcho, rpcValue)
		return err
	}
}

func rpcGetOp(c *rpcClient) func(*rand.Rand) error {
	return func(r *rand.Rand) error {
		_, err := c.call(rpcGet, []byte(rpcKey(r.IntN(rpcKeys))))
		return err
	}
}

func rpcSetOp(c *rpcClient) func(*rand.Rand) error {
	return func(r *rand.Rand) error {
		key := rpcKey(r.IntN(rpcKeys))
		body := append(append([]byte{byte(len(key))}, key...), rpcValue...)
		_, err := c.call(rpcSet, body)
		return err
	}
}

// the same bucket without a sidecar: what a call costs a Go program in-process
func TestRPCEmbedded(t *testing.T) {
	rpcSkip(t)
	ctx := context.Background()
	store, bucket, err := openRPCBucket(ctx, rpcDir(t))
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close(ctx)
	get := func(r *rand.Rand) error {
		_, _, err := bucket.Get(ctx, rpcKey(r.IntN(rpcKeys)))
		return err
	}
	set := func(r *rand.Rand) error { return bucket.Set(ctx, rpcKey(r.IntN(rpcKeys)), rpcValue) }
	for _, op := range []struct {
		name string
		run  func(*rand.Rand) error
	}{{"get", get}, {"set", set}} {
		for _, depth := range rpcDepths {
			ops, err := rpcRun(depth, rpcDuration(), op.run)
			if err != nil {
				t.Fatal(err)
			}
			rpcLog(t, "go embedded", "-", "-", op.name, depth, ops)
		}
	}
}

// who writes a connection's frames: each sender for itself, a goroutine of its
// own, or the first sender to find nobody writing
func TestRPCWriters(t *testing.T) {
	rpcSkip(t)
	for _, transport := range []string{"unix", "tcp"} {
		for _, writer := range []string{"naive", "goroutine", "leader"} {
			config := rpcConfigFor(t, transport, writer)
			c := startRPCSidecar(t, config).connect(t, config)
			for _, op := range []struct {
				name string
				run  func(*rand.Rand) error
			}{{"echo", rpcEchoOp(c)}, {"get", rpcGetOp(c)}} {
				for _, depth := range rpcDepths {
					ops, err := rpcRun(depth, rpcDuration(), op.run)
					if err != nil {
						t.Fatal(err)
					}
					rpcLog(t, "go", transport, writer, op.name, depth, ops)
				}
			}
			c.close()
		}
	}
}

// each transport from Go, Bun and Python, frames written by the first sender
func TestRPCTransports(t *testing.T) {
	rpcSkip(t)
	for _, transport := range rpcTransports() {
		config := rpcConfigFor(t, transport, "leader")
		sidecar := startRPCSidecar(t, config)
		c := sidecar.connect(t, config)
		for _, op := range []struct {
			name string
			run  func(*rand.Rand) error
		}{{"echo", rpcEchoOp(c)}, {"get", rpcGetOp(c)}, {"set", rpcSetOp(c)}} {
			for _, depth := range rpcDepths {
				ops, err := rpcRun(depth, rpcDuration(), op.run)
				if err != nil {
					t.Fatal(err)
				}
				rpcLog(t, "go", transport, "leader", op.name, depth, ops)
			}
		}
		c.close()
		if transport == "stdio" {
			sidecar.stop()
		}
		runRPCForeignClients(t, config, sidecar.address)
	}
}

// runRPCForeignClients runs the Bun and Python clients against the sidecar at
// address, or, over stdio, against a sidecar each starts as its own child
func runRPCForeignClients(t *testing.T, config rpcSidecarConfig, address string) {
	t.Helper()
	if config.Transport == "stdio" {
		config = rpcConfigFor(t, "stdio", "leader")
	}
	argv, text := rpcSidecarCommand(config)
	command, _ := json.Marshal(argv)
	env := append(os.Environ(),
		"TINYSTORE_RPC_TRANSPORT="+config.Transport,
		"TINYSTORE_RPC_ADDRESS="+address,
		"TINYSTORE_RPC_SIDECAR_COMMAND="+string(command),
		"TINYSTORE_RPC_SIDECAR_CONFIG="+text,
		"TINYSTORE_RPC_SIDECAR_VARIABLE="+rpcSidecarVariable,
		fmt.Sprintf("TINYSTORE_RPC_SECONDS=%g", rpcDuration().Seconds()),
	)
	for _, client := range rpcForeignClients() {
		runRPCForeignClient(t, client, config.Transport, env)
	}
}

// rpcForeignClient is how a client in another language is run
type rpcForeignClient struct {
	name string
	argv []string
}

func rpcForeignClients() []rpcForeignClient {
	var clients []rpcForeignClient
	if bun, err := exec.LookPath("bun"); err == nil {
		clients = append(clients, rpcForeignClient{"bun", []string{bun, "run", "testdata/rpc/client.ts"}})
	}
	for _, name := range []string{"python3", "python"} {
		python, err := exec.LookPath(name)
		if err != nil || exec.Command(python, "-c", "pass").Run() != nil { // Windows' store alias only says it is not there
			continue
		}
		clients = append(clients,
			rpcForeignClient{"python asyncio", []string{python, "-X", "utf8", "testdata/rpc/client.py", "asyncio"}},
			rpcForeignClient{"python threads", []string{python, "-X", "utf8", "testdata/rpc/client.py", "threads"}},
		)
		break
	}
	return clients
}

// rpcForeignResult is a line a foreign client prints
type rpcForeignResult struct {
	Op      string  `json:"op"`
	Depth   int     `json:"depth"`
	Ops     float64 `json:"ops"`
	Skipped string  `json:"skipped"`
}

func runRPCForeignClient(t *testing.T, client rpcForeignClient, transport string, env []string) {
	t.Helper()
	cmd := exec.Command(client.argv[0], client.argv[1:]...)
	cmd.Env = env
	cmd.Stderr = os.Stderr
	out, err := cmd.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	lines := bufio.NewScanner(out)
	for lines.Scan() {
		var result rpcForeignResult
		if err := json.Unmarshal(lines.Bytes(), &result); err != nil {
			t.Logf("%s: %s", client.name, lines.Text())
			continue
		}
		if result.Skipped != "" {
			t.Logf("%-14s %-6s skipped: %s", client.name, transport, result.Skipped)
			continue
		}
		rpcLog(t, client.name, transport, "leader", result.Op, result.Depth, result.Ops)
	}
	if err := cmd.Wait(); err != nil {
		t.Errorf("%s over %s: %v", client.name, transport, err)
	}
}

// the credit a stream needs: 64 MiB transfers, again and again for the
// case's time, through windows from one chunk to sixty-four
func TestRPCCredit(t *testing.T) {
	rpcSkip(t)
	const transfer = 64 << 20
	for _, run := range rpcCreditRuns() {
		transport, writer := run[0], run[1]
		config := rpcConfigFor(t, transport, writer)
		c := startRPCSidecar(t, config).connect(t, config)
		for _, direction := range []string{"upload", "download", "download, credit aside"} {
			c.creditAside = direction == "download, credit aside"
			for _, window := range []int64{64 << 10, 256 << 10, 1 << 20, 4 << 20} {
				moved, took, err := rpcTransferFor(rpcDuration(), func() error {
					if direction == "upload" {
						return c.upload(transfer, window)
					}
					return c.download(transfer, window)
				})
				if err != nil {
					t.Fatal(err)
				}
				t.Logf("go %-6s %-11s %-22s window %5d KiB  %7.0f MiB/s", transport, writer, direction, window>>10,
					float64(moved*transfer)/(1<<20)/took.Seconds())
			}
		}
		c.close()
	}
}

// rpcCreditRuns is every transport with the leader writing, and TCP again
// with no write over 256 KiB
func rpcCreditRuns() [][2]string {
	var runs [][2]string
	for _, transport := range rpcTransports() {
		runs = append(runs, [2]string{transport, "leader"})
	}
	return append(runs, [2]string{"tcp", "leader-256k"})
}

func rpcTransferFor(d time.Duration, transfer func() error) (int64, time.Duration, error) {
	began := time.Now()
	var moved int64
	for time.Since(began) < d {
		if err := transfer(); err != nil {
			return 0, 0, err
		}
		moved++
	}
	return moved, time.Since(began), nil
}

// what a call costs the server at 256 in flight: a goroutine started for each
// call or workers that keep theirs, bodies and answers allocated or pooled,
// the sidecar's GOGC at its default or 400; the allocations each get made, and
// a profile of the sidecar and its client, the first variant's and the last's
func TestRPCServerCost(t *testing.T) {
	rpcSkip(t)
	variants := []struct {
		pooled             bool
		workers, gcPercent int
	}{{false, 0, 0}, {true, 0, 0}, {false, 64, 0}, {false, 256, 0}, {false, 0, 400}, {false, 256, 400}}
	for i, variant := range variants {
		config := rpcConfigFor(t, "unix", "leader")
		config.Pooled, config.Workers, config.GCPercent = variant.pooled, variant.workers, variant.gcPercent
		config.Profile = filepath.Join(config.Dir, "sidecar.pprof")
		c := startRPCSidecar(t, config).connect(t, config)
		if _, err := rpcRun(256, rpcDuration()/2, rpcGetOp(c)); err != nil { // the first run after a start is slower
			t.Fatal(err)
		}
		before, err := c.stats()
		if err != nil {
			t.Fatal(err)
		}
		gets, err := rpcRun(256, rpcDuration(), rpcGetOp(c))
		if err != nil {
			t.Fatal(err)
		}
		after, err := c.stats()
		if err != nil {
			t.Fatal(err)
		}
		sets, err := rpcRun(256, rpcDuration(), rpcSetOp(c))
		if err != nil {
			t.Fatal(err)
		}
		requests := float64(after.Requests - before.Requests)
		t.Logf("pooled %-5v workers %3d gogc %3d at 256: %7.0f gets/s, %7.0f sets/s; a get %5.2f allocations and "+
			"%6.0f bytes, %d GCs", variant.pooled, variant.workers, variant.gcPercent, gets, sets,
			float64(after.Mallocs-before.Mallocs)/requests, float64(after.Bytes-before.Bytes)/requests,
			after.GC-before.GC)
		if i == 0 || i == len(variants)-1 {
			profileRPCGets(t, c, config)
		}
		c.close()
	}
}

// the latency of one call in flight, for comparing a machine idle between
// calls with one kept busy
func TestRPCOneInFlight(t *testing.T) {
	rpcSkip(t)
	for _, transport := range []string{"unix", "tcp", "stdio"} {
		config := rpcConfigFor(t, transport, "leader")
		c := startRPCSidecar(t, config).connect(t, config)
		for _, op := range []struct {
			name string
			run  func(*rand.Rand) error
		}{{"echo", rpcEchoOp(c)}, {"get", rpcGetOp(c)}} {
			ops, err := rpcRun(1, rpcDuration(), op.run)
			if err != nil {
				t.Fatal(err)
			}
			rpcLog(t, "go", transport, "leader", op.name, 1, ops)
		}
		c.close()
	}
}

// keepRPCProfiles copies the profiles into TINYSTORE_RPC_PROFILES, when it is
// set, for views the round's own output does not print
func keepRPCProfiles(t *testing.T, profiles ...string) {
	t.Helper()
	keep := os.Getenv("TINYSTORE_RPC_PROFILES")
	if keep == "" {
		return
	}
	if err := os.MkdirAll(keep, 0o750); err != nil {
		t.Fatal(err)
	}
	stamp := time.Now().UTC().Format("150405")
	for _, profile := range profiles {
		data, err := os.ReadFile(profile)
		if err != nil {
			t.Fatal(err)
		}
		kept := filepath.Join(keep, stamp+"-"+filepath.Base(profile))
		if err := os.WriteFile(kept, data, 0o600); err != nil {
			t.Fatal(err)
		}
	}
}

// profileRPCGets profiles the sidecar and this client through one run of gets
func profileRPCGets(t *testing.T, c *rpcClient, config rpcSidecarConfig) {
	t.Helper()
	clientProfile := filepath.Join(config.Dir, "client.pprof")
	file, err := os.Create(clientProfile)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = c.call(rpcProfileStart, nil); err != nil {
		t.Fatal(err)
	}
	if err = pprof.StartCPUProfile(file); err != nil {
		t.Fatal(err)
	}
	ops, err := rpcRun(256, rpcDuration(), rpcGetOp(c))
	pprof.StopCPUProfile()
	_ = file.Close()
	if err != nil {
		t.Fatal(err)
	}
	if _, err := c.call(rpcProfileStop, nil); err != nil {
		t.Fatal(err)
	}
	t.Logf("profiled get at 256: %.0f ops/s on %d processors", ops, runtime.NumCPU())
	keepRPCProfiles(t, config.Profile, clientProfile)
	for _, profile := range []string{config.Profile, clientProfile} {
		// the flat top, then who calls into the operating system: on Windows a
		// socket's syscalls and SQLite's file reads both land in cgocall
		for _, view := range [][]string{{"-top", "-nodecount=35"}, {"-peek", `runtime\.cgocall$|runtime\.semasleep$`}} {
			args := append(append([]string{"tool", "pprof"}, view...), profile)
			out, err := exec.Command("go", args...).CombinedOutput()
			if err != nil {
				t.Logf("pprof %s: %v", profile, err)
			}
			t.Logf("%s %s:\n%s", filepath.Base(profile), view[0], strings.TrimSpace(string(out)))
		}
	}
}
