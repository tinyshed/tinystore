// Command tinystore is the tinystore command of this SDK's release, for a Go
// program's author who installed nothing else: it fetches the release's
// binary once, as a store's first open does, and runs it with its arguments.
//
//	go run tinyshed.org/tinystore/cmd/tinystore@v0.1.0 mcp ./data
//	go install tinyshed.org/tinystore/cmd/tinystore@v0.1.0
package main

import (
	"context"
	"fmt"
	"os"

	"tinyshed.org/tinystore/internal/core"
)

func main() {
	binary, err := core.ReleaseBinary(context.Background())
	if err != nil {
		fmt.Fprintf(os.Stderr, "tinystore: %v\n", err)
		os.Exit(1)
	}
	os.Exit(run(binary, os.Args[1:]))
}
