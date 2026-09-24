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

	"github.com/tinyshed/tinystore"
)

// Restore unpacks a backup into dir, which must be empty or absent: it runs
// before tinystore.Open, never on an open store. Every file is checked against
// the manifest's size and sha256; on any failure nothing is left in dir.
func Restore(ctx context.Context, dir string, archive io.ReaderAt, size int64) error {
	reader, err := zip.NewReader(archive, size)
	if err != nil {
		return fmt.Errorf("%w: backup archive: %w", tinystore.ErrCorrupt, err)
	}
	manifest, err := readManifest(reader)
	if err != nil {
		return err
	}
	if err = claimEmpty(dir); err != nil {
		return err
	}

	for _, file := range manifest.Files {
		if err = ctx.Err(); err == nil {
			err = restoreFile(reader, dir, file)
		}
		if err != nil {
			return errors.Join(err, emptyDirectory(dir))
		}
	}
	return nil
}

func readManifest(reader *zip.Reader) (Manifest, error) {
	var manifest Manifest
	source, err := reader.Open(manifestName)
	if err != nil {
		return manifest, fmt.Errorf("%w: backup without a manifest: %w", tinystore.ErrCorrupt, err)
	}
	defer source.Close()
	if err = json.NewDecoder(source).Decode(&manifest); err != nil {
		return manifest, fmt.Errorf("%w: backup manifest: %w", tinystore.ErrCorrupt, err)
	}
	if manifest.Format != format {
		return manifest, fmt.Errorf("%w: backup format %d, this binary reads %d",
			tinystore.ErrInvalid, manifest.Format, format)
	}
	return manifest, nil
}

func claimEmpty(dir string) error {
	entries, err := os.ReadDir(dir)
	if len(entries) > 0 {
		return fmt.Errorf("%w: restore into %s, which is not empty", tinystore.ErrInvalid, dir)
	}
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		return fmt.Errorf("restore into %s: %w", dir, err)
	}
	return os.MkdirAll(dir, 0o750)
}

// restoreFile writes one file beside its final name, checks it, syncs it and
// only then gives it its name
func restoreFile(reader *zip.Reader, dir string, file ManifestFile) error {
	if !filepath.IsLocal(filepath.FromSlash(file.Name)) {
		return fmt.Errorf("%w: backup names %q outside the store", tinystore.ErrCorrupt, file.Name)
	}
	source, err := reader.Open(file.Name)
	if err != nil {
		return fmt.Errorf("%w: backup lacks %s: %w", tinystore.ErrCorrupt, file.Name, err)
	}
	defer source.Close()

	final := tinystore.SnapshotPath(dir, file.Name)
	if err = os.MkdirAll(filepath.Dir(final), 0o750); err != nil {
		return fmt.Errorf("restore %s: %w", file.Name, err)
	}
	partial := final + ".partial"
	if err = writeChecked(partial, source, file); err != nil {
		return err
	}
	return os.Rename(partial, final)
}

func writeChecked(path string, source io.Reader, file ManifestFile) (err error) {
	flags := os.O_CREATE | os.O_EXCL | os.O_WRONLY
	target, err := os.OpenFile(path, flags, 0o600) //nolint:gosec // inside the restored directory
	if err != nil {
		return fmt.Errorf("restore %s: %w", file.Name, err)
	}
	defer func() { err = errors.Join(err, target.Close()) }()

	sum := sha256.New()
	size, err := io.Copy(io.MultiWriter(target, sum), source)
	if err != nil {
		return fmt.Errorf("restore %s: %w", file.Name, err)
	}
	if size != file.Size || hex.EncodeToString(sum.Sum(nil)) != file.SHA256 {
		return fmt.Errorf("%w: %s does not match its manifest", tinystore.ErrCorrupt, file.Name)
	}
	return target.Sync()
}

// emptyDirectory removes what a failed restore wrote, leaving dir itself
func emptyDirectory(dir string) error {
	entries, err := os.ReadDir(dir)
	for _, entry := range entries {
		err = errors.Join(err, os.RemoveAll(filepath.Join(dir, entry.Name())))
	}
	return err
}
