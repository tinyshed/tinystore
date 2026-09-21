package spike

import (
	"os"
	"runtime"
	"slices"
	"testing"
	"time"

	"github.com/klauspost/compress/zstd"
)

func TestSmallBlockCodecMemory(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	t.Logf("GOMAXPROCS=%d", runtime.GOMAXPROCS(0))
	for _, settings := range []struct {
		name string
		enc  []zstd.EOption
		dec  []zstd.DOption
	}{
		{name: "defaults"},
		{name: "one worker", enc: []zstd.EOption{zstd.WithEncoderConcurrency(1)}, dec: []zstd.DOption{zstd.WithDecoderConcurrency(1)}},
		{
			name: "one worker 8KiB", enc: []zstd.EOption{zstd.WithEncoderConcurrency(1), zstd.WithWindowSize(8192), zstd.WithLowerEncoderMem(true)},
			dec: []zstd.DOption{zstd.WithDecoderConcurrency(1), zstd.WithDecoderMaxMemory(8192), zstd.WithDecoderMaxWindow(8192)},
		},
	} {
		runtime.GC()
		var before, after runtime.MemStats
		runtime.ReadMemStats(&before)
		writer, err := zstd.NewWriter(nil, settings.enc...)
		if err != nil {
			t.Fatal(err)
		}
		reader, err := zstd.NewReader(nil, settings.dec...)
		if err != nil {
			t.Fatal(err)
		}
		samples := capacitySamples("noisy gauge", 240, 15*time.Second)
		for range max(64, runtime.GOMAXPROCS(0)*2) {
			back, readErr := decode(encode(samples, writer), reader)
			if readErr != nil || !slices.Equal(samples, back) {
				t.Fatalf("round trip: %v", readErr)
			}
		}
		runtime.GC()
		runtime.ReadMemStats(&after)
		retained := int64(after.HeapAlloc) - int64(before.HeapAlloc)
		const runs = 2000
		start := time.Now()
		for range runs {
			encode(samples, writer)
		}
		encodeTime := time.Since(start) / runs
		packed := encode(samples, writer)
		start = time.Now()
		for range runs {
			if _, err = decode(packed, reader); err != nil {
				t.Fatal(err)
			}
		}
		decodeTime := time.Since(start) / runs
		runtime.KeepAlive(writer)
		runtime.KeepAlive(reader)
		t.Logf("%-18s live Go heap delta %.3f MiB; encode %s decode %s; float %d B integer %d B",
			settings.name, float64(retained)/(1<<20), encodeTime, decodeTime,
			len(packed), len(encode(capacitySamples("whole numbers", 240, 15*time.Second), writer)))
		reader.Close()
		if err = writer.Close(); err != nil {
			t.Fatal(err)
		}
	}
}
