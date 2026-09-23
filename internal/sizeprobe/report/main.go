package main

import (
	"fmt"
	"os"
	"runtime"
)

func main() {
	probe, err := os.Stat("bin/sizeprobe")
	if err != nil {
		fmt.Fprintln(os.Stderr, "stat probe:", err)
		os.Exit(1)
	}
	baseline, err := os.Stat("bin/sizeprobe-baseline")
	if err != nil {
		fmt.Fprintln(os.Stderr, "stat baseline:", err)
		os.Exit(1)
	}
	fmt.Printf("%s/%s probe: %d KiB, of which the import added %d KiB\n", runtime.GOOS, runtime.GOARCH, probe.Size()/1024, (probe.Size()-baseline.Size())/1024)
}
