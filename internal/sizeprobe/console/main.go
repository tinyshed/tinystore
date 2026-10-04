// Command console links the logger without the store, so task size can report what it costs.
package main

import (
	"log/slog"

	"github.com/tinyshed/tinystore/records/console"
)

func main() {
	slog.New(console.Handler("probe", console.Redact(console.Secrets...))).Info("probe", "password", "x")
}
