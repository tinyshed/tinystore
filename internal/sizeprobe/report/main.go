// Command report prints what task size measured: each probe's target, its
// size and what its imports added over the baseline, and fails when the
// linker kept the canary's methods, which a reflect.Value.MethodByName with a
// name it cannot see makes it keep.
package main

import (
	"bytes"
	"context"
	"debug/buildinfo"
	"fmt"
	"os"
	"os/exec"
)

const (
	baselinePath = "bin/sizeprobe-baseline"
	symbolsPath  = "bin/sizeprobe-symbols" // the probe, its symbols kept for go tool nm
	canary       = "linkaudit.Canary.Unreached"
)

// probes are what task size builds, each beside the baseline
var probes = []struct{ path, what string }{
	{"bin/sizeprobe", "every engine"},
	{"bin/sizeprobe-sql", "one SQL database"},
	{"bin/sizeprobe-search", "one SQL database with FTS5 and R*Tree"},
	{"bin/sizeprobe-console", "the logger without the store"},
}

func main() {
	baseline, err := os.Stat(baselinePath)
	if err != nil {
		fail(err)
	}
	for _, probe := range probes {
		target, targetErr := sameTarget(probe.path, baselinePath)
		if targetErr != nil {
			fail(targetErr)
		}
		built, statErr := os.Stat(probe.path)
		if statErr != nil {
			fail(statErr)
		}
		added := built.Size() - baseline.Size()
		fmt.Printf("%s probe of %s: %d KiB, of which the import added %d KiB\n",
			target, probe.what, built.Size()/1024, added/1024)
	}
	if err = checkCanary(); err != nil {
		fail(err)
	}
	fmt.Println("method pruning: the canary's methods are gone")
}

// checkCanary fails when the probe's symbols still hold the canary's method
func checkCanary() error {
	symbols, err := exec.CommandContext(context.Background(), "go", "tool", "nm", symbolsPath).Output()
	if err != nil {
		return fmt.Errorf("go tool nm %s: %w", symbolsPath, err)
	}
	if bytes.Contains(symbols, []byte(canary)) {
		return fmt.Errorf("the probe holds %s: something links reflect.Value.MethodByName with a name "+
			"the linker cannot see, which keeps every exported method of every program importing TinyStore", canary)
	}
	return nil
}

// sameTarget names the platform both binaries were built for. The promise is
// the size of a cgo-free build, and two targets' difference measures nothing.
func sameTarget(probe, baseline string) (string, error) {
	probeTarget, err := cgoFreeTarget(probe)
	if err != nil {
		return "", err
	}
	baselineTarget, err := cgoFreeTarget(baseline)
	if err != nil {
		return "", err
	}
	if probeTarget != baselineTarget {
		return "", fmt.Errorf("probe built for %s, baseline for %s", probeTarget, baselineTarget)
	}
	return probeTarget, nil
}

func cgoFreeTarget(path string) (string, error) {
	info, err := buildinfo.ReadFile(path)
	if err != nil {
		return "", fmt.Errorf("read build settings: %w", err)
	}
	settings := make(map[string]string, len(info.Settings))
	for _, setting := range info.Settings {
		settings[setting.Key] = setting.Value
	}
	if settings["CGO_ENABLED"] != "0" {
		return "", fmt.Errorf("%s built with CGO_ENABLED=%q, not 0", path, settings["CGO_ENABLED"])
	}
	return settings["GOOS"] + "/" + settings["GOARCH"], nil
}

func fail(err error) {
	fmt.Fprintln(os.Stderr, "size report:", err)
	os.Exit(1)
}
