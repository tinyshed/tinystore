// Package private makes a directory its owner's alone, as a local server's
// SERVE and socket need.
//
// On Unix that is mode 0700 on a directory the user owns and no link leads to.
// On Windows it is a protected DACL naming the user alone, which the files made
// in it inherit.
package private

import (
	"errors"
	"fmt"
	"os"
)

var errNotADirectory = errors.New("not a directory of its own")

// Dir makes dir, or takes the one there, and leaves it its owner's alone.
func Dir(dir string) error {
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return fmt.Errorf("private %s: %w", dir, err)
	}
	found, err := os.Lstat(dir)
	if err != nil {
		return fmt.Errorf("private %s: %w", dir, err)
	}
	if !found.IsDir() {
		return fmt.Errorf("private %s: %w: %s", dir, errNotADirectory, found.Mode().Type())
	}
	if err = restrict(dir, found); err != nil {
		return fmt.Errorf("private %s: %w", dir, err)
	}
	return nil
}
