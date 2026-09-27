package blobs

import (
	"os"
	"syscall"
	"testing"
	"time"
	"unsafe"
)

// DONTNEED affects only this test's immutable file; mincore verifies eviction before the read
func TestColdReadMeasured(t *testing.T) {
	measuring(t)
	s := openTestStore(t, measureDir(t))
	media := openTestBucket(t, s, "media")
	const size = 512 << 20
	if _, err := media.Put(t.Context(), "cold", &endless{chunk: randomBytes(64, 64<<10), left: size}); err != nil {
		t.Fatal(err)
	}
	file, err := os.Open(s.pathOf(t, media, "cold"))
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	for round := range 3 {
		_, _, failure := syscall.Syscall6(syscall.SYS_FADVISE64, file.Fd(), 0, size, 4, 0, 0)
		if failure != 0 {
			t.Fatal(failure)
		}
		mapping, mapErr := syscall.Mmap(int(file.Fd()), 0, size, syscall.PROT_READ, syscall.MAP_SHARED)
		if mapErr != nil {
			t.Fatal(mapErr)
		}
		pages := make([]byte, size/os.Getpagesize())
		_, _, failure = syscall.Syscall(syscall.SYS_MINCORE, uintptr(unsafe.Pointer(&mapping[0])), size,
			uintptr(unsafe.Pointer(&pages[0])))
		unmapErr := syscall.Munmap(mapping)
		resident := 0
		for _, page := range pages {
			resident += int(page & 1)
		}
		if failure != 0 || unmapErr != nil || resident != 0 {
			t.Fatalf("eviction: %d/%d resident pages, %v, %v", resident, len(pages), failure, unmapErr)
		}
		for _, kind := range []string{"cold", "warm"} {
			started := time.Now()
			if err = readWhole(t.Context(), media, "cold"); err != nil {
				t.Fatal(err)
			}
			t.Logf("round %d, %d resident pages before cold read", round+1, resident)
			logRate(t, kind+" checked whole read", size, time.Since(started))
		}
	}
}
