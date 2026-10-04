// Package backup writes every engine of a tinystore.Store into one zip, and
// restores it into an empty directory before the store opens. It is a package
// of its own so that archive/zip is linked only by programs that back up.
//
//	backup.zip
//	├── manifest.json    format, time, and each file's engine, schema, size and sha256
//	├── metrics.db
//	├── records.db
//	├── sql/app.db
//	└── secret.key       a file of the host's, only when File names it
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
	"path/filepath"
	"slices"
	"strings"
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

// Option changes what a backup holds.
type Option func(*settings)

type settings struct {
	files []string
}

// File puts a file of the host's in the backup, by its path inside the
// store's directory, such as "secret.key": copied as it is, and checked by
// its size and sha256 on restore as an engine's file is. A backup holds no
// file but the engines' unless one is named, so that a key kept beside the
// store leaves only on purpose.
func File(name string) Option {
	return func(s *settings) { s.files = append(s.files, name) }
}

// Write copies every engine's file while the store keeps working, and writes
// the copies, the files File names and their manifest to w as one zip.
func Write(ctx context.Context, store *tinystore.Store, w io.Writer, options ...Option) (err error) {
	snapshot, err := store.Snapshot(ctx)
	if err != nil {
		return err
	}
	defer func() { err = errors.Join(err, snapshot.Remove()) }()
	return WriteSnapshot(ctx, snapshot, store.Now(), w, options...)
}

// WriteSnapshot writes the copies a snapshot holds and their manifest to w as
// one zip, as Write does, for a snapshot its caller added copies to: a server
// adds the databases no client opened. created is the time the manifest
// records, and a file File names is read from the store's directory, the one
// the snapshot is in. The snapshot stays the caller's to remove.
func WriteSnapshot(ctx context.Context, snapshot tinystore.Snapshot, created time.Time, w io.Writer,
	options ...Option,
) (err error) {
	var said settings
	for _, option := range options {
		option(&said)
	}
	if err = ctx.Err(); err != nil {
		return err
	}
	if err = checkHostFiles(snapshot, said.files); err != nil {
		return err
	}
	copies, err := os.OpenRoot(snapshot.Dir)
	if err != nil {
		return fmt.Errorf("backup: %w", err)
	}
	defer func() { err = errors.Join(err, copies.Close()) }()
	host, err := os.OpenRoot(filepath.Dir(snapshot.Dir))
	if err != nil {
		return fmt.Errorf("backup: %w", err)
	}
	defer func() { err = errors.Join(err, host.Close()) }()

	archive := zip.NewWriter(w)
	manifest := Manifest{Format: format, Created: created.UTC()}
	files := slices.Clone(snapshot.Files)
	for _, name := range said.files {
		files = append(files, tinystore.SnapshotFile{Name: name, Engine: hostEngine, Stored: true})
	}
	for _, file := range files {
		from := copies
		if file.Engine == hostEngine {
			from = host
		}
		entry, addErr := addFile(archive, from, file)
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

// the engine a manifest gives a file of the host's
const hostEngine = "host"

// checkHostFiles refuses a name outside the store's directory, and one an
// engine's file or another name already takes
func checkHostFiles(snapshot tinystore.Snapshot, names []string) error {
	taken := map[string]bool{}
	for _, file := range snapshot.Files {
		taken[file.Name] = true
	}
	for _, name := range names {
		local := filepath.ToSlash(filepath.Clean(name))
		switch {
		case !filepath.IsLocal(name):
			return fmt.Errorf("%w: backup.File(%q) is outside the store's directory", tinystore.ErrInvalid, name)
		case local != name:
			return fmt.Errorf("%w: backup.File(%q) is spelled %q inside the store", tinystore.ErrInvalid, name, local)
		case taken[name] || name == "LOCK" || strings.HasPrefix(name, ".snapshot-"):
			return fmt.Errorf("%w: backup.File(%q) is the store's own file", tinystore.ErrInvalid, name)
		}
		taken[name] = true
	}
	return nil
}

// addFile streams one copy into the zip and sums it on the way. It opens the
// copy through the snapshot's root: a file opened so shares its deletion on
// Windows, where an engine may remove the copy's other name while it is read.
// A host's file is opened through the store's root, which no link leaves.
func addFile(archive *zip.Writer, copies *os.Root, file tinystore.SnapshotFile) (ManifestFile, error) {
	source, err := copies.Open(filepath.FromSlash(file.Name))
	if err != nil {
		return ManifestFile{}, fmt.Errorf("backup %s: %w", file.Name, err)
	}
	defer source.Close()
	if info, statErr := source.Stat(); statErr == nil && !info.Mode().IsRegular() {
		return ManifestFile{}, fmt.Errorf("%w: backup %s: not a regular file", tinystore.ErrInvalid, file.Name)
	}

	method := zip.Deflate
	if file.Stored {
		method = zip.Store
	}
	target, err := archive.CreateHeader(&zip.FileHeader{Name: file.Name, Method: method})
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
