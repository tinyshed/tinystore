package main

import (
	"fmt"
	"io"
	"os"
	"path/filepath"
)

// copiedReadFixture copies read/metrics.db into a directory of its own, so that
// a run can write to it and open it as a store
func copiedReadFixture(dir string) (string, func(), error) {
	source, err := os.Open(filepath.Join(dir, "read", "metrics.db"))
	if err != nil {
		return "", nil, fmt.Errorf("open read fixture: %w", err)
	}
	defer source.Close()
	runDir, err := os.MkdirTemp(dir, "read-run-")
	if err != nil {
		return "", nil, fmt.Errorf("create read fixture copy: %w", err)
	}
	cleanup := func() {
		if err := os.RemoveAll(runDir); err != nil {
			fmt.Fprintln(os.Stderr, "remove read fixture copy:", err)
		}
	}
	path := filepath.Join(runDir, "metrics.db")
	copy, err := os.Create(path)
	if err != nil {
		cleanup()
		return "", nil, fmt.Errorf("create read fixture copy: %w", err)
	}
	if _, err = io.Copy(copy, source); err != nil {
		copy.Close()
		cleanup()
		return "", nil, fmt.Errorf("copy read fixture: %w", err)
	}
	if err = copy.Close(); err != nil {
		cleanup()
		return "", nil, fmt.Errorf("close read fixture copy: %w", err)
	}
	return path, cleanup, nil
}
