//go:build !windows && cgo

package core

/*
#cgo linux LDFLAGS: -ldl
#include <dlfcn.h>
#include <stdint.h>
#include <stdlib.h>

typedef size_t (*open_fn)(const uint8_t *, size_t, const uint8_t *, size_t, void (*)(void *), void *, void **, uint8_t *, size_t);
typedef size_t (*send_fn)(void *, const uint8_t *, size_t, uint8_t *, size_t);
typedef size_t (*recv_fn)(void *, uint32_t, uint8_t *, size_t);
typedef void (*close_fn)(void *);

static size_t call_open(void *f, const uint8_t *dir, size_t dir_len, const uint8_t *options, size_t options_len,
		void **conn, uint8_t *error, size_t error_room) {
	return ((open_fn)f)(dir, dir_len, options, options_len, NULL, NULL, conn, error, error_room);
}
static size_t call_send(void *f, void *conn, const uint8_t *frames, size_t len) {
	return ((send_fn)f)(conn, frames, len, NULL, 0);
}
static size_t call_recv(void *f, void *conn, uint32_t wait_ms, uint8_t *into, size_t room) {
	return ((recv_fn)f)(conn, wait_ms, into, room);
}
static void call_close(void *f, void *conn) {
	((close_fn)f)(conn);
}
*/
import "C"

import (
	"strings"
	"unsafe"
)

// On Linux and macOS the library is loaded with dlopen, so that a program
// builds without it and the release's library is found at run time.
type cgoLibrary struct {
	openFn, sendFn, recvFn, closeFn unsafe.Pointer
}

func loadLibrary(path string) (coreLibrary, error) {
	name := C.CString(path)
	defer C.free(unsafe.Pointer(name))
	handle := C.dlopen(name, C.RTLD_NOW|C.RTLD_LOCAL)
	if handle == nil {
		return nil, Errorf(Closed, "dlopen %s: %s", path, C.GoString(C.dlerror()))
	}
	lib := &cgoLibrary{}
	for symbol, fn := range map[string]*unsafe.Pointer{
		"tinystore_open": &lib.openFn, "tinystore_send": &lib.sendFn,
		"tinystore_recv": &lib.recvFn, "tinystore_close": &lib.closeFn,
	} {
		cname := C.CString(symbol)
		*fn = C.dlsym(handle, cname)
		C.free(unsafe.Pointer(cname))
		if *fn == nil {
			return nil, Errorf(Closed, "%s has no %s", path, symbol)
		}
	}
	return lib, nil
}

func (l *cgoLibrary) open(dir, options []byte) (uintptr, error) {
	var conn unsafe.Pointer
	failure := make([]byte, 4096)
	cdir := C.CBytes(dir)
	defer C.free(cdir)
	var coptions unsafe.Pointer
	if len(options) > 0 {
		coptions = C.CBytes(options)
		defer C.free(coptions)
	}
	cfailure := C.malloc(C.size_t(len(failure)))
	defer C.free(cfailure)
	written := C.call_open(l.openFn, (*C.uint8_t)(cdir), C.size_t(len(dir)), (*C.uint8_t)(coptions),
		C.size_t(len(options)), &conn, (*C.uint8_t)(cfailure), C.size_t(len(failure)))
	if written > 0 {
		message := C.GoStringN((*C.char)(cfailure), C.int(written))
		if strings.Contains(message, "in use") {
			return 0, Errorf(InUse, "cannot open %s: %s", dir, message)
		}
		return 0, Errorf(Closed, "cannot open %s: %s", dir, message)
	}
	return uintptr(conn), nil
}

func (l *cgoLibrary) send(conn uintptr, frames []byte) {
	if len(frames) == 0 {
		return
	}
	cframes := C.CBytes(frames)
	defer C.free(cframes)
	C.call_send(l.sendFn, unsafe.Pointer(conn), (*C.uint8_t)(cframes), C.size_t(len(frames)))
}

func (l *cgoLibrary) recv(conn uintptr, waitMs uint32, into []byte) int {
	cinto := C.malloc(C.size_t(len(into)))
	defer C.free(cinto)
	n := int(C.call_recv(l.recvFn, unsafe.Pointer(conn), C.uint32_t(waitMs), (*C.uint8_t)(cinto), C.size_t(len(into))))
	copy(into, unsafe.Slice((*byte)(cinto), n))
	return n
}

func (l *cgoLibrary) close(conn uintptr) {
	C.call_close(l.closeFn, unsafe.Pointer(conn))
}
