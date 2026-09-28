//go:build !windows

package spike

import (
	"errors"
	"io"
)

var errRPCNoPipes = errors.New("named pipes are Windows'")

func rpcListenPipe(string) (rpcListener, error) { return nil, errRPCNoPipes }

func rpcDialPipe(string) (io.ReadWriteCloser, error) { return nil, errRPCNoPipes }

func rpcPipeName(id string) string { return id }

const rpcHasPipes = false
