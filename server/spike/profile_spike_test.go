package spike

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"runtime/debug"
	"runtime/pprof"
	"strings"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/server"
	"github.com/tinyshed/tinystore/server/internal/client"
	"github.com/tinyshed/tinystore/server/wire"
)

// What the server's time goes to, as the round profiled its prototype: this
// test binary, run again with sidecarVariable set, serves a directory as
// `tinystore serve --local` does, and profiles itself between the lines
// "start" and "stop" its parent writes on its stdin, answering each "stop"
// with what it allocated meanwhile.
const sidecarVariable = "TINYSTORE_SERVE_SIDECAR"

// sidecarConfig is the directory a profiled server serves and where its
// profile goes
type sidecarConfig struct {
	Dir     string `json:"dir"`
	Profile string `json:"profile"`
}

// allocated is what a profiled server allocated between a start and a stop
type allocated struct {
	Mallocs uint64 `json:"mallocs"`
	Bytes   uint64 `json:"bytes"`
	GC      uint32 `json:"gc"`
}

func TestMain(m *testing.M) {
	if text := os.Getenv(sidecarVariable); text != "" {
		if err := runSidecar(text); err != nil {
			fmt.Fprintln(os.Stderr, "sidecar:", err)
			os.Exit(1)
		}
		os.Exit(0)
	}
	os.Exit(m.Run())
}

// runSidecar serves the directory's sidecar with the collector's target
// `tinystore serve` sets, until its parent closes its stdin
func runSidecar(text string) (err error) {
	var config sidecarConfig
	if err = json.Unmarshal([]byte(text), &config); err != nil {
		return err
	}
	debug.SetGCPercent(400)
	ctx := context.Background()
	store, err := tinystore.Open(ctx, config.Dir, tinystore.Options{})
	if err != nil {
		return err
	}
	defer func() { err = errors.Join(err, store.Close(ctx)) }()
	srv, err := server.New(store, server.Options{Version: "spike"})
	if err != nil {
		return err
	}
	l, unpublish, err := srv.Publish(ctx)
	if err != nil {
		return err
	}
	go func() { _ = srv.Serve(ctx, l) }()
	err = obey(config, os.Stdin, os.Stdout)
	closing, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	return errors.Join(err, unpublish(), srv.Close(closing))
}

// obey profiles between each "start" and "stop" line, and answers a stop
// with what was allocated meanwhile
func obey(config sidecarConfig, commands io.Reader, answers io.Writer) error {
	var before runtime.MemStats
	var profile *os.File
	lines := bufio.NewScanner(commands)
	for lines.Scan() {
		switch lines.Text() {
		case "start":
			file, err := os.Create(config.Profile)
			if err != nil {
				return err
			}
			profile = file
			runtime.ReadMemStats(&before)
			if err = pprof.StartCPUProfile(profile); err != nil {
				return err
			}
		case "stop":
			pprof.StopCPUProfile()
			var after runtime.MemStats
			runtime.ReadMemStats(&after)
			if err := profile.Close(); err != nil {
				return err
			}
			line, err := json.Marshal(allocated{
				Mallocs: after.Mallocs - before.Mallocs, Bytes: after.TotalAlloc - before.TotalAlloc,
				GC: after.NumGC - before.NumGC,
			})
			if err != nil {
				return err
			}
			if _, err = fmt.Fprintf(answers, "%s\n", line); err != nil {
				return err
			}
		}
	}
	return lines.Err()
}

// the server's time and allocations at 256 gets in flight through its local
// transport, after a second of them to warm it, as the round's sidecar cost
func TestServeProfile(t *testing.T) {
	skip(t)
	for i := range value {
		value[i] = 7
	}
	dir := storeDir(t)
	fill(t, dir)
	config := sidecarConfig{Dir: dir, Profile: filepath.Join(t.TempDir(), "server.pprof")}
	text, err := json.Marshal(config)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(os.Args[0], "-test.run=^$")
	cmd.Env = append(os.Environ(), sidecarVariable+"="+string(text))
	cmd.Stderr = os.Stderr
	commands, err := cmd.StdinPipe()
	if err != nil {
		t.Fatal(err)
	}
	answers, err := cmd.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	if err = cmd.Start(); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		_ = commands.Close()
		_ = cmd.Wait()
	})
	s := &sidecar{transport: "local", dir: dir}
	s.waitServe(t)
	conn, err := client.Found(context.Background(), dir, wire.Hello{Client: "tinystore-go-bench"})
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	get := getOp(conn, openBucket(t, conn))
	if _, err = run(256, time.Second, get); err != nil { // the first run after a start is slower
		t.Fatal(err)
	}

	lines := bufio.NewScanner(answers)
	if _, err = fmt.Fprintln(commands, "start"); err != nil {
		t.Fatal(err)
	}
	began := time.Now()
	ops, err := run(256, caseTime(), get)
	took := time.Since(began)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = fmt.Fprintln(commands, "stop"); err != nil {
		t.Fatal(err)
	}
	var spent allocated
	if !lines.Scan() || json.Unmarshal(lines.Bytes(), &spent) != nil {
		t.Fatalf("the sidecar answered its stop with %q, %v", lines.Text(), lines.Err())
	}
	gets := ops * took.Seconds()
	t.Logf("server at 256: %7.0f gets/s; a get %5.2f allocations and %6.0f bytes, %d GCs, on %d processors",
		ops, float64(spent.Mallocs)/gets, float64(spent.Bytes)/gets, spent.GC, runtime.NumCPU())
	showProfile(t, config.Profile)
}

// showProfile prints the flat top and who calls into the operating system,
// the views the round printed of its sidecar, and keeps the profile in
// TINYSTORE_RPC_PROFILES when that is set
func showProfile(t *testing.T, profile string) {
	t.Helper()
	if keep := os.Getenv("TINYSTORE_RPC_PROFILES"); keep != "" {
		data, err := os.ReadFile(profile)
		if err != nil {
			t.Fatal(err)
		}
		if err = os.MkdirAll(keep, 0o750); err != nil {
			t.Fatal(err)
		}
		kept := filepath.Join(keep, time.Now().UTC().Format("150405")+"-server.pprof")
		if err = os.WriteFile(kept, data, 0o600); err != nil {
			t.Fatal(err)
		}
	}
	for _, view := range [][]string{
		{"-top", "-nodecount=35"},
		{"-top", "-cum", "-nodecount=45"},
		{"-peek", `runtime\.cgocall$|runtime\.semasleep$|runtime\.futex$`},
	} {
		args := append(append([]string{"tool", "pprof"}, view...), profile)
		out, err := exec.Command("go", args...).CombinedOutput()
		if err != nil {
			t.Logf("pprof %s: %v", profile, err)
		}
		t.Logf("server.pprof %s:\n%s", strings.Join(view, " "), strings.TrimSpace(string(out)))
	}
}
