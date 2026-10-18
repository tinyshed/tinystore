//go:build !windows

package core

import (
	"context"
	"io"
)

func dialPipe(context.Context, string) (io.ReadWriteCloser, error) {
	return nil, Errorf(Invalid, "a named pipe is Windows' alone: a sidecar here serves a Unix socket")
}
