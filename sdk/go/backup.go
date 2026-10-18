package tinystore

import (
	"context"
	"fmt"
	"os"
	"path/filepath"

	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/wire"
)

// Backup is what a backup holds.
type Backup struct {
	// Files are the files, the engines' and those it was told to take.
	Files int
	// Bytes are their bytes, the zip's own fields aside.
	Bytes int64
	// KeyLeftOut says the store seals values with an encryption key the
	// backup does not hold: they open with that key alone.
	KeyLeftOut bool
}

// BackupOption is what a backup takes beside every engine's files.
type BackupOption func(*wire.BackupWrite)

// Include takes these files of the program's, by their paths in the store's directory.
func Include(paths ...string) BackupOption {
	return func(w *wire.BackupWrite) { w.Include = append(w.Include, paths...) }
}

// IncludeEncryptionKey takes the store's encryption key, the zip not being encrypted.
func IncludeEncryptionKey() BackupOption {
	return func(w *wire.BackupWrite) { w.IncludeEncryptionKey = true }
}

// Backup writes a backup of every engine of the store to path as one zip,
// made while the store keeps working, which tinystore restore takes back
// into an empty directory. It is written beside path and renamed once
// whole, so that a backup that fails leaves no zip; a connection lost on the
// way makes it again from its start.
//
//	backup, err := store.Backup(ctx, "backup-2026-10-13.zip")
func (s *Store) Backup(ctx context.Context, path string, opts ...BackupOption) (Backup, error) {
	var asked wire.BackupWrite
	for _, opt := range opts {
		opt(&asked)
	}
	partial := filepath.Join(filepath.Dir(path), fmt.Sprintf("%s.%d.part", filepath.Base(path), os.Getpid()))
	var written wire.BackupWritten
	err := s.core.Link.Run(ctx, core.Reading, func(c *core.Connection) error {
		var err error
		written, err = backupTo(ctx, c, partial, asked)
		return err
	})
	if err == nil {
		err = os.Rename(partial, path)
	}
	if err != nil {
		_ = os.Remove(partial)
		return Backup{}, err
	}
	return Backup{Files: int(written.Files), Bytes: int64(written.Bytes), KeyLeftOut: written.KeyLeftOut}, nil
}

// backupTo writes the zip's pieces as they come to a file, synced once whole.
func backupTo(ctx context.Context, c *core.Connection, path string, asked wire.BackupWrite) (wire.BackupWritten, error) {
	var written wire.BackupWritten
	file, err := os.Create(path)
	if err != nil {
		return written, core.Wrap(core.Invalid, err, "backup %s", path)
	}
	defer file.Close()
	stream, err := c.Session.Open(ctx, wire.MethodBackupWrite, wire.Encode(&asked), true)
	if err != nil {
		return written, err
	}
	defer stream.Cancel()
	if _, err := stream.Next(ctx); err != nil {
		return written, err
	}
	for {
		event, err := stream.Next(ctx)
		if err != nil {
			return written, err
		}
		if event.End {
			if err := wire.Decode(event.Body, &written); err != nil {
				return written, core.Wrap(core.Protocol, err, "an answer that does not read")
			}
			return written, file.Sync()
		}
		stream.Consumed(len(event.Body))
		var piece wire.BackupPiece
		if err := wire.Decode(event.Body, &piece); err != nil {
			return written, core.Wrap(core.Protocol, err, "a piece that does not read")
		}
		if _, err := file.Write(piece.Bytes); err != nil {
			return written, core.Wrap(core.Invalid, err, "backup %s", path)
		}
	}
}
