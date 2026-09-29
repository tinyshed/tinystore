// Package pipe serves and reaches a Windows named pipe the way Go's runtime
// polls it: each end is overlapped and given to os.NewFile.
//
// A synchronous handle would lack the completion port os.NewFile gives an
// overlapped one, and would hold every write behind a pending read. Elsewhere
// the package has nothing to serve.
package pipe

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"runtime"
	"sync"
	"syscall"
	"time"
	"unsafe"
)

var (
	kernel32                 = syscall.NewLazyDLL("kernel32.dll")
	advapi32                 = syscall.NewLazyDLL("advapi32.dll")
	createNamedPipe          = kernel32.NewProc("CreateNamedPipeW")
	connectNamedPipe         = kernel32.NewProc("ConnectNamedPipe")
	cancelIoEx               = kernel32.NewProc("CancelIoEx")
	getOverlappedResult      = kernel32.NewProc("GetOverlappedResult")
	createEvent              = kernel32.NewProc("CreateEventW")
	localFree                = kernel32.NewProc("LocalFree")
	stringSecurityDescriptor = advapi32.NewProc("ConvertStringSecurityDescriptorToSecurityDescriptorW")
)

// the errors syscall does not name
const (
	errorPipeBusy      = syscall.Errno(231)
	errorNoData        = syscall.Errno(232)
	errorPipeConnected = syscall.Errno(535)
	errorNotFound      = syscall.Errno(1168)
)

const (
	pipeAccessDuplex          = 0x00000003
	fileFlagFirstPipeInstance = 0x00080000
	pipeRejectRemoteClients   = 0x00000008
	pipeUnlimitedInstances    = 255
	pipeBuffer                = 64 << 10
	securitySQOSPresent       = 0x00100000
	securityIdentification    = 0x00010000
	sddlRevision              = 1
)

// Path is the file system's name of a pipe.
func Path(name string) string {
	return `\\.\pipe\` + name
}

// Listener serves a named pipe that only its owner may open and that refuses
// remote clients. It creates the pipe's first instance itself, so that no other
// process held the name before it. An instance always waits for the next
// client.
type Listener struct {
	name       string
	descriptor uintptr // the owner-only DACL every instance carries
	mu         sync.Mutex
	waiting    syscall.Handle // the instance the next client connects to
	closed     bool
	// connecting is the kernel's while a client is awaited: it lives in the
	// listener, on the heap, since a goroutine's stack may move meanwhile
	connecting syscall.Overlapped
}

func Listen(name string) (*Listener, error) {
	descriptor, err := ownerOnly()
	if err != nil {
		return nil, fmt.Errorf("pipe %s: %w", name, err)
	}
	l := &Listener{name: Path(name), descriptor: descriptor}
	if l.waiting, err = l.instance(true); err != nil {
		return nil, errors.Join(fmt.Errorf("pipe %s: %w", name, err), l.free())
	}
	return l, nil
}

// ownerOnly is a security descriptor whose protected DACL grants the process's
// user everything and nobody else anything
func ownerOnly() (uintptr, error) {
	token, err := syscall.OpenCurrentProcessToken()
	if err != nil {
		return 0, err
	}
	defer token.Close()
	user, err := token.GetTokenUser()
	if err != nil {
		return 0, err
	}
	sid, err := user.User.Sid.String()
	if err != nil {
		return 0, err
	}
	sddl, err := syscall.UTF16PtrFromString("D:P(A;;GA;;;" + sid + ")")
	if err != nil {
		return 0, err
	}
	var descriptor uintptr
	made, _, err := stringSecurityDescriptor.Call(
		uintptr(unsafe.Pointer(sddl)), //nolint:gosec // converted in the call's arguments, as Call requires
		sddlRevision,
		uintptr(unsafe.Pointer(&descriptor)), //nolint:gosec // converted in the call's arguments, as Call requires
		0)
	if made == 0 {
		return 0, err
	}
	return descriptor, nil
}

func (l *Listener) instance(first bool) (syscall.Handle, error) {
	name, err := syscall.UTF16PtrFromString(l.name)
	if err != nil {
		return 0, err
	}
	mode := uintptr(pipeAccessDuplex | syscall.FILE_FLAG_OVERLAPPED)
	if first {
		mode |= fileFlagFirstPipeInstance
	}
	attributes := syscall.SecurityAttributes{SecurityDescriptor: l.descriptor}
	attributes.Length = uint32(unsafe.Sizeof(attributes))
	pipe, _, err := createNamedPipe.Call(
		uintptr(unsafe.Pointer(name)), //nolint:gosec // converted in the call's arguments, as Call requires
		mode, pipeRejectRemoteClients, pipeUnlimitedInstances, pipeBuffer, pipeBuffer, 0,
		uintptr(unsafe.Pointer(&attributes))) //nolint:gosec // converted in the call's arguments, as Call requires
	if syscall.Handle(pipe) == syscall.InvalidHandle {
		return 0, err
	}
	return syscall.Handle(pipe), nil
}

// errClosed is net.ErrClosed, as a closed socket's listener says it
var errClosed = fmt.Errorf("%w: the pipe's listener is closed", net.ErrClosed)

// Accept waits for a client on the waiting instance, and puts the next one in
// its place before it returns, so that a client rarely finds every instance
// busy. One Accept runs at a time.
func (l *Listener) Accept() (io.ReadWriteCloser, error) {
	l.mu.Lock()
	pipe, closed := l.waiting, l.closed
	l.mu.Unlock()
	if closed {
		return nil, errClosed
	}
	if err := l.waitForClient(pipe); err != nil {
		if l.isClosed() {
			return nil, errClosed
		}
		return nil, err
	}
	next, err := l.instance(false)
	l.mu.Lock()
	defer l.mu.Unlock()
	if err != nil || l.closed {
		return nil, errors.Join(err, errClosed, syscall.CloseHandle(pipe))
	}
	l.waiting = next
	return os.NewFile(uintptr(pipe), l.name), nil
}

func (l *Listener) isClosed() bool {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.closed
}

// waitForClient connects an overlapped instance through an event of its own
func (l *Listener) waitForClient(pipe syscall.Handle) error {
	made, _, err := createEvent.Call(0, 1, 0, 0)
	if made == 0 {
		return err
	}
	event := syscall.Handle(made)
	defer syscall.CloseHandle(event) //nolint:errcheck // an event of this call's own
	defer runtime.KeepAlive(l)
	l.connecting = syscall.Overlapped{HEvent: event}
	connected, _, err := connectNamedPipe.Call(uintptr(pipe),
		uintptr(unsafe.Pointer(&l.connecting))) //nolint:gosec // on the heap, as the kernel keeps it
	switch {
	case connected != 0, errors.Is(err, errorPipeConnected), errors.Is(err, errorNoData):
		return nil
	case !errors.Is(err, syscall.ERROR_IO_PENDING):
		return err
	}
	var transferred uint32
	done, _, err := getOverlappedResult.Call(uintptr(pipe),
		uintptr(unsafe.Pointer(&l.connecting)), //nolint:gosec // on the heap, as the kernel keeps it
		uintptr(unsafe.Pointer(&transferred)),  //nolint:gosec // converted in the call's arguments, as Call requires
		1)
	if done == 0 {
		return err
	}
	return nil
}

// Close ends the waiting instance, so that an Accept waiting on it returns.
func (l *Listener) Close() error {
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.closed {
		return nil
	}
	l.closed = true
	return errors.Join(cancelWaiting(l.waiting), syscall.CloseHandle(l.waiting), l.free())
}

// cancelWaiting ends the wait for a client on an instance; one that waits for
// nothing is not an error
func cancelWaiting(pipe syscall.Handle) error {
	cancelled, _, err := cancelIoEx.Call(uintptr(pipe), 0)
	if cancelled == 0 && !errors.Is(err, errorNotFound) {
		return err
	}
	return nil
}

func (l *Listener) free() error {
	if l.descriptor == 0 {
		return nil
	}
	left, _, err := localFree.Call(l.descriptor)
	l.descriptor = 0
	if left != 0 {
		return err
	}
	return nil
}

// Dial opens a client's end of the pipe, waiting while every instance is busy
// until ctx ends. It lets the server identify it and no more, so that a
// process that took the name first cannot act as its client.
func Dial(ctx context.Context, name string) (io.ReadWriteCloser, error) {
	path, err := syscall.UTF16PtrFromString(Path(name))
	if err != nil {
		return nil, err
	}
	for {
		handle, err := syscall.CreateFile(path, syscall.GENERIC_READ|syscall.GENERIC_WRITE, 0, nil,
			syscall.OPEN_EXISTING, syscall.FILE_FLAG_OVERLAPPED|securitySQOSPresent|securityIdentification, 0)
		if err == nil {
			return os.NewFile(uintptr(handle), Path(name)), nil
		}
		if !errors.Is(err, errorPipeBusy) {
			return nil, fmt.Errorf("pipe %s: %w", name, err)
		}
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		case <-time.After(5 * time.Millisecond):
		}
	}
}

const Supported = true
