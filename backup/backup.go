// Package backup writes every engine of a tinystore.Store into one zip, and
// restores it into an empty directory before the store opens. It is a package
// of its own so that archive/zip is linked only by programs that back up.
//
//	backup.zip
//	├── manifest.json    format, time, and each file's engine, schema, size and sha256
//	├── metrics.db
//	├── records.db
//	└── sql/app.db
package backup

import (
	"archive/zip"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"time"

	"github.com/tinyshed/tinystore"
)

// the manifest's own version; a reader refuses one it does not know
const format = 1

const manifestName = "manifest.json"

type Manifest struct {
	Format  int            `json:"format"`
	Created time.Time      `json:"created"`
	Files   []ManifestFile `json:"files"`
}

type ManifestFile struct {
	tinystore.SnapshotFile
	Size   int64  `json:"size"`
	SHA256 string `json:"sha256"`
}

// Write copies every engine's file while the store keeps working, and writes
// the copies and their manifest to w as one zip.
func Write(ctx context.Context, store *tinystore.Store, w io.Writer) (err error) {
	snapshot, err := store.Snapshot(ctx)
	if err != nil {
		return err
	}
	defer func() { err = errors.Join(err, snapshot.Remove()) }()

	archive := zip.NewWriter(w)
	manifest := Manifest{Format: format, Created: store.Now().UTC()}
	for _, file := range snapshot.Files {
		entry, addErr := addFile(archive, tinystore.SnapshotPath(snapshot.Dir, file.Name), file)
		if addErr != nil {
			return errors.Join(addErr, archive.Close())
		}
		manifest.Files = append(manifest.Files, entry)
	}
	if err = addManifest(archive, manifest); err != nil {
		return errors.Join(err, archive.Close())
	}
	return archive.Close()
}

// addFile streams one copy into the zip and sums it on the way
func addFile(archive *zip.Writer, path string, file tinystore.SnapshotFile) (ManifestFile, error) {
	source, err := os.Open(path) //nolint:gosec // a copy the store just wrote
	if err != nil {
		return ManifestFile{}, fmt.Errorf("backup %s: %w", file.Name, err)
	}
	defer source.Close()

	target, err := archive.Create(file.Name)
	if err != nil {
		return ManifestFile{}, fmt.Errorf("backup %s: %w", file.Name, err)
	}
	sum := sha256.New()
	size, err := io.Copy(io.MultiWriter(target, sum), source)
	if err != nil {
		return ManifestFile{}, fmt.Errorf("backup %s: %w", file.Name, err)
	}
	return ManifestFile{SnapshotFile: file, Size: size, SHA256: hex.EncodeToString(sum.Sum(nil))}, nil
}

func addManifest(archive *zip.Writer, manifest Manifest) error {
	target, err := archive.Create(manifestName)
	if err != nil {
		return fmt.Errorf("backup manifest: %w", err)
	}
	encoder := json.NewEncoder(target)
	encoder.SetIndent("", "  ")
	return encoder.Encode(manifest)
}
