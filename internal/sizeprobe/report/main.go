// Command report prints what task size measured: the probe's target, its size
// and what the import added over the baseline.
package main

import (
	"debug/buildinfo"
	"fmt"
	"os"
)

const (
	probePath    = "bin/sizeprobe"
	baselinePath = "bin/sizeprobe-baseline"
)

func main() {
	target, err := sameTarget(probePath, baselinePath)
	if err != nil {
		fail(err)
	}

	probe, err := os.Stat(probePath)
	if err != nil {
		fail(err)
	}
	baseline, err := os.Stat(baselinePath)
	if err != nil {
		fail(err)
	}

	added := probe.Size() - baseline.Size()
	fmt.Printf("%s probe: %d KiB, of which the import added %d KiB\n", target, probe.Size()/1024, added/1024)
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
