package spike

import (
	"archive/zip"
	"fmt"
	"image"
	"image/color"
	"image/jpeg"
	"math/rand/v2"
	"os"
	"path/filepath"
	"testing"
)

// TestBlobsBackupEntries backs up a large object and a thousand JPEGs of
// about 190 KB into a zip, their entries stored and then deflated, and
// restores each zip
func TestBlobsBackupEntries(t *testing.T) {
	kvMeasuring(t)
	dir := blobsDir(t)
	objects := filepath.Join(dir, "objects")
	if err := os.MkdirAll(filepath.Join(objects, "photos"), 0o750); err != nil {
		t.Fatal(err)
	}
	blobsLargeFile(t, filepath.Join(objects, "film"), blobsLarge())
	for n := range 1000 {
		if err := blobsJPEG(filepath.Join(objects, "photos", fmt.Sprintf("%04d.jpg", n)), uint64(n)); err != nil {
			t.Fatal(err)
		}
	}
	t.Logf("backed up: a %s object and 1000 JPEGs, %s on the disk", blobsSize(int(blobsLarge())),
		blobsDiskUsed(objects))
	for _, method := range []uint16{zip.Store, zip.Deflate} {
		archive := filepath.Join(dir, "backup-"+blobsMethod(method)+".zip")
		blobsZip(t, objects, archive, method)
		blobsRestore(t, archive, filepath.Join(dir, "restored-"+blobsMethod(method)))
		_ = os.Remove(archive)
		_ = os.RemoveAll(filepath.Join(dir, "restored-"+blobsMethod(method)))
	}
}

// blobsLargeFile writes size random bytes, 4 MiB at a time
func blobsLargeFile(t *testing.T, path string, size int64) {
	t.Helper()
	file, err := os.Create(path)
	if err != nil {
		t.Fatal(err)
	}
	chunk := blobsBytes(3, 4<<20)
	for written := int64(0); written < size && err == nil; written += int64(len(chunk)) {
		_, err = file.Write(chunk)
	}
	if err = errorsJoin(err, file.Close()); err != nil {
		t.Fatal(err)
	}
}

// blobsJPEG writes a 640 by 480 photo of noise over gradients, which encodes
// to about 190 KB at quality 90, as a phone's picture reduced for the web does
func blobsJPEG(path string, seed uint64) error {
	random := rand.New(rand.NewPCG(seed, 5))
	img := image.NewRGBA(image.Rect(0, 0, 640, 480))
	for y := range 480 {
		for x := range 640 {
			noise := byte(random.IntN(64))
			red, green := byte(x/3), byte(y/2)
			img.Set(x, y, color.RGBA{R: red + noise, G: green + noise, B: noise * 2, A: 255})
		}
	}
	file, err := os.Create(path)
	if err != nil {
		return err
	}
	return errorsJoin(jpeg.Encode(file, img, &jpeg.Options{Quality: 90}), file.Close())
}
