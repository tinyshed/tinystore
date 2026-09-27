package blobs

import (
	"errors"
	"io"
	"os"
	"syscall"
	"testing"
	"time"
	"unsafe"
)

// this diagnostic changes only the reader's handle; the public API continues to use cached I/O
func TestUncachedReadMeasured(t *testing.T) {
	measuring(t)
	s := openTestStore(t, measureDir(t))
	media := openTestBucket(t, s, "media")
	const size, alignment = 512 << 20, 64 << 10
	if _, err := media.Put(t.Context(), "cold", &endless{chunk: randomBytes(64, 64<<10), left: size}); err != nil {
		t.Fatal(err)
	}
	path := s.pathOf(t, media, "cold")
	raw := make([]byte, 2*alignment)
	offset := int((alignment - uintptr(unsafe.Pointer(&raw[0]))%alignment) % alignment)
	buffer := raw[offset : offset+alignment]
	for round := range 3 {
		for _, uncached := range []bool{true, false} {
			reader, found, err := media.Open(t.Context(), "cold")
			if err != nil || !found {
				t.Fatalf("Open: %v, %v", found, err)
			}
			if uncached {
				file := uncachedFile(t, path)
				if err = reader.file.Close(); err != nil {
					file.Close()
					t.Fatal(err)
				}
				reader.file = file
			}
			started := time.Now()
			var read int64
			for {
				n, readErr := reader.Read(buffer)
				read += int64(n)
				if errors.Is(readErr, io.EOF) {
					break
				}
				if readErr != nil {
					reader.Close()
					t.Fatal(readErr)
				}
			}
			took := time.Since(started)
			if err = reader.Close(); err != nil || read != size {
				t.Fatalf("read %d, close %v", read, err)
			}
			t.Logf("round %d, unbuffered handle %v", round+1, uncached)
			logRate(t, "checked Reader, 64 KiB aligned", size, took)
		}
	}
}

func uncachedFile(t *testing.T, path string) *os.File {
	t.Helper()
	name, err := syscall.UTF16PtrFromString(path)
	if err != nil {
		t.Fatal(err)
	}
	const noBuffering = 0x20000000
	const share = syscall.FILE_SHARE_READ | syscall.FILE_SHARE_WRITE | syscall.FILE_SHARE_DELETE
	handle, err := syscall.CreateFile(name, syscall.GENERIC_READ, share, nil, syscall.OPEN_EXISTING, noBuffering, 0)
	if err != nil {
		t.Fatal(err)
	}
	return os.NewFile(uintptr(handle), path)
}
