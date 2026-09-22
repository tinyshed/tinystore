package main

import (
	"fmt"
	"io"
	"os"
	"path/filepath"
)

func copiedReadFixture(dir string) (string, func(), error) {
	source, err := os.Open(filepath.Join(dir, "read.db"))
	if err != nil {
		return "", nil, fmt.Errorf("open read fixture: %w", err)
	}
	defer source.Close()
	copy, err := os.CreateTemp(dir, "read-run-*.db")
	if err != nil {
		return "", nil, fmt.Errorf("create read fixture copy: %w", err)
	}
	path := copy.Name()
	if _, err = io.Copy(copy, source); err != nil {
		copy.Close()
		os.Remove(path)
		return "", nil, fmt.Errorf("copy read fixture: %w", err)
	}
	if err = copy.Close(); err != nil {
		os.Remove(path)
		return "", nil, fmt.Errorf("close read fixture copy: %w", err)
	}
	cleanup := func() {
		for _, suffix := range []string{"-wal", "-shm", ""} {
			if err := os.Remove(path + suffix); err != nil && !os.IsNotExist(err) {
				fmt.Fprintln(os.Stderr, "remove read fixture copy:", err)
			}
		}
	}
	return path, cleanup, nil
}
