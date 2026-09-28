package spike

import (
	"errors"
	"io"
	"os"
	"sync/atomic"
	"syscall"
	"time"
	"unsafe"
)

var (
	rpcKernel32           = syscall.NewLazyDLL("kernel32.dll")
	rpcCreateNamedPipe    = rpcKernel32.NewProc("CreateNamedPipeW")
	rpcConnectNamedPipe   = rpcKernel32.NewProc("ConnectNamedPipe")
	rpcCreateEvent        = rpcKernel32.NewProc("CreateEventW")
	rpcGetOverlappedValue = rpcKernel32.NewProc("GetOverlappedResult")
	rpcCreateFile         = rpcKernel32.NewProc("CreateFileW")
)

const (
	rpcPipeAccessDuplex          = 0x00000003
	rpcFileFlagFirstPipeInstance = 0x00080000
	rpcPipeRejectRemoteClients   = 0x00000008
	rpcPipeUnlimitedInstances    = 255
	rpcPipeBuffer                = 64 << 10
	rpcErrorPipeConnected        = syscall.Errno(535)
	rpcErrorPipeBusy             = syscall.Errno(231)
)

// rpcPipeListener serves a named pipe the way Go's runtime can poll it: each
// instance is overlapped, and connected before os.NewFile hands it to the
// runtime's completion port, since a synchronous handle would let a pending
// read hold every write behind it
type rpcPipeListener struct {
	name   string
	first  bool
	closed atomic.Bool
}

func rpcListenPipe(name string) (rpcListener, error) {
	return &rpcPipeListener{name: name, first: true}, nil
}

func (l *rpcPipeListener) address() string { return l.name }

func (l *rpcPipeListener) Close() error {
	l.closed.Store(true)
	return nil
}

func (l *rpcPipeListener) accept() (io.ReadWriteCloser, error) {
	if l.closed.Load() {
		return nil, errors.New("closed")
	}
	pipe, err := l.instance()
	if err != nil {
		return nil, err
	}
	if err := rpcWaitForClient(pipe); err != nil {
		_ = syscall.CloseHandle(pipe)
		return nil, err
	}
	return os.NewFile(uintptr(pipe), l.name), nil
}

func (l *rpcPipeListener) instance() (syscall.Handle, error) {
	name, err := syscall.UTF16PtrFromString(l.name)
	if err != nil {
		return 0, err
	}
	mode := uintptr(rpcPipeAccessDuplex | syscall.FILE_FLAG_OVERLAPPED)
	if l.first {
		mode |= rpcFileFlagFirstPipeInstance
		l.first = false
	}
	pipe, _, err := rpcCreateNamedPipe.Call(uintptr(unsafe.Pointer(name)), mode, rpcPipeRejectRemoteClients,
		rpcPipeUnlimitedInstances, rpcPipeBuffer, rpcPipeBuffer, 0, 0)
	if syscall.Handle(pipe) == syscall.InvalidHandle {
		return 0, err
	}
	return syscall.Handle(pipe), nil
}

// rpcWaitForClient connects an overlapped instance through an event of its own
func rpcWaitForClient(pipe syscall.Handle) error {
	event, _, err := rpcCreateEvent.Call(0, 1, 0, 0)
	if event == 0 {
		return err
	}
	defer syscall.CloseHandle(syscall.Handle(event))
	var overlapped syscall.Overlapped
	overlapped.HEvent = syscall.Handle(event)
	connected, _, err := rpcConnectNamedPipe.Call(uintptr(pipe), uintptr(unsafe.Pointer(&overlapped)))
	switch {
	case connected != 0, errors.Is(err, rpcErrorPipeConnected):
		return nil
	case !errors.Is(err, syscall.ERROR_IO_PENDING):
		return err
	}
	var transferred uint32
	done, _, err := rpcGetOverlappedValue.Call(uintptr(pipe), uintptr(unsafe.Pointer(&overlapped)),
		uintptr(unsafe.Pointer(&transferred)), 1)
	if done == 0 {
		return err
	}
	return nil
}

// rpcDialPipe opens an overlapped client end, waiting while every instance is busy
func rpcDialPipe(name string) (io.ReadWriteCloser, error) {
	path, err := syscall.UTF16PtrFromString(name)
	if err != nil {
		return nil, err
	}
	for range 200 {
		handle, _, err := rpcCreateFile.Call(uintptr(unsafe.Pointer(path)),
			syscall.GENERIC_READ|syscall.GENERIC_WRITE, 0, 0, syscall.OPEN_EXISTING, syscall.FILE_FLAG_OVERLAPPED, 0)
		if syscall.Handle(handle) != syscall.InvalidHandle {
			return os.NewFile(handle, name), nil
		}
		if !errors.Is(err, rpcErrorPipeBusy) && !errors.Is(err, syscall.ERROR_FILE_NOT_FOUND) {
			return nil, err
		}
		time.Sleep(10 * time.Millisecond)
	}
	return nil, errors.New("every instance of " + name + " stayed busy")
}

func rpcPipeName(id string) string { return `\\.\pipe\tinystore-rpc-` + id }

const rpcHasPipes = true
