package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"path/filepath"

	"github.com/tinyshed/tinystore/backup"
	"github.com/tinyshed/tinystore/server/wire"
)

const backupUsage = `usage:
  tinystore backup <dir> <file.zip>    every engine of a store in one checked zip, while its application runs
  tinystore restore <file.zip> <dir>   a backup into an empty directory, before anything opens it

backup reaches the store through the server serving it, and starts the
directory's sidecar when none does, as logs does. The zip is written beside
its name and renamed into place once whole, so a backup that fails leaves no
zip. restore checks every file's size and checksum before it keeps any of them`

// backupStore writes a backup of the store in a directory to a zip, as the
// server serving it makes one
func backupStore(ctx context.Context, args []string, out, stderr io.Writer) error {
	dir, file, err := twoArguments("backup", args, stderr)
	if err != nil {
		return err
	}
	conn, err := reachStore(ctx, dir)
	if err != nil {
		return err
	}
	defer conn.Close()
	st, err := conn.Open(ctx, wire.ServerBackup, wire.Empty{}, true)
	if err != nil {
		return err
	}
	if _, err = st.Response(ctx); err != nil {
		return err
	}
	size, err := writeWhole(file, func(w io.Writer) error {
		for {
			chunk, last, nextErr := st.Next(ctx)
			if nextErr != nil {
				return nextErr
			}
			if _, writeErr := w.Write(chunk); writeErr != nil || last {
				return writeErr
			}
		}
	})
	if err != nil {
		return err
	}
	p := painterFor(out)
	_, err = fmt.Fprintf(out, "%s %s  backed up to %s %s\n", p.in(green, "●"), p.in(bold, dir), file,
		p.in(dim, fmt.Sprintf("· %d bytes", size)))
	return err
}

// writeWhole writes a file beside its name, syncs it, and only then gives it
// its name, so that what fails leaves nothing behind
func writeWhole(name string, write func(io.Writer) error) (size int64, err error) {
	part, err := os.CreateTemp(filepath.Dir(name), filepath.Base(name)+".*.part")
	if err != nil {
		return 0, err
	}
	defer func() {
		if err != nil {
			err = errors.Join(err, os.Remove(part.Name())) //nolint:gosec // beside the zip the person names
		}
	}()
	counted := &countingWriter{w: part}
	err = write(counted)
	if err = errors.Join(err, part.Sync(), part.Close()); err != nil {
		return 0, err
	}
	return counted.n, os.Rename(part.Name(), name) //nolint:gosec // the zip the person names
}

type countingWriter struct {
	w io.Writer
	n int64
}

func (c *countingWriter) Write(p []byte) (int, error) {
	n, err := c.w.Write(p)
	c.n += int64(n)
	return n, err
}

// restoreStore writes a backup into an empty directory, every file checked
// before the store there opens
func restoreStore(ctx context.Context, args []string, out, stderr io.Writer) error {
	file, dir, err := twoArguments("restore", args, stderr)
	if err != nil {
		return err
	}
	archive, err := os.Open(file) //nolint:gosec // the backup the person names
	if err != nil {
		return err
	}
	defer archive.Close()
	info, err := archive.Stat()
	if err != nil {
		return err
	}
	if err = backup.Restore(ctx, dir, archive, info.Size()); err != nil {
		return err
	}
	p := painterFor(out)
	_, err = fmt.Fprintf(out, "%s %s  restored from %s\n", p.in(green, "●"), p.in(bold, dir), file)
	return err
}

// twoArguments is what backup and restore take: two names, in their order
func twoArguments(name string, args []string, stderr io.Writer) (first, second string, err error) {
	flags := flag.NewFlagSet(name, flag.ContinueOnError)
	flags.SetOutput(stderr)
	flags.Usage = func() { fmt.Fprintln(stderr, backupUsage) }
	if err = flags.Parse(args); err != nil {
		return "", "", err
	}
	if flags.NArg() != 2 {
		return "", "", errors.New(backupUsage)
	}
	return flags.Arg(0), flags.Arg(1), nil
}
