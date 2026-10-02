package main

import (
	"context"
	"flag"
	"fmt"
	"io"
	"time"

	"github.com/tinyshed/tinystore/server/reach"
	"github.com/tinyshed/tinystore/server/wire"
)

const stopUsage = `usage:
  tinystore stop [dir]   stop the server serving dir: its streams finish, then it gives back SERVE and the directory

It starts nothing, so a directory nobody serves stays as it is. A sidecar the
application's SDK started comes back at its next open, with that SDK's binary:
this is how one older than its SDK is replaced.`

// how long stop waits for the server it asked to leave: the streams running
// have closeWait to finish
const stopWait = closeWait + 5*time.Second

// stopServing asks the server SERVE names to stop and waits until it has
// ended the connection, which it does once its streams have finished
func stopServing(ctx context.Context, args []string, out, stderr io.Writer) error {
	flags := flag.NewFlagSet("stop", flag.ContinueOnError)
	flags.SetOutput(stderr)
	flags.Usage = func() { fmt.Fprintln(stderr, stopUsage) }
	given := flags.String("dir", "", "")
	dir, err := parseWithDir(flags, args, given)
	if err != nil {
		return err
	}
	found, err := readStatus(dir)
	if err != nil {
		return err
	}
	p := painterFor(out)
	if found.Server == nil {
		_, err = fmt.Fprintf(out, "%s %s  %s\n", p.in(dim, "○"), p.in(bold, dir), p.in(dim, "served by none"))
		return err
	}

	conn, err := reach.Found(ctx, dir, clientName())
	if err != nil {
		return fmt.Errorf("SERVE names pid %d, which does not answer, so nothing serves %s: %w", found.Server.PID,
			dir, err)
	}
	defer conn.Close() // ended by the server already, once it has stopped
	if _, err = conn.Call(ctx, wire.ServerStop, wire.Empty{}); err != nil {
		return err
	}
	select {
	case <-conn.Done():
	case <-time.After(stopWait):
		return fmt.Errorf("the server of %s, pid %d, was asked to stop and still serves after %s", dir,
			found.Server.PID, stopWait)
	case <-ctx.Done():
		return ctx.Err()
	}
	_, err = fmt.Fprintf(out, "%s %s  stopped %s\n", p.in(dim, "○"), p.in(bold, dir),
		p.in(dim, fmt.Sprintf("%s · pid %d", found.Server.Version, found.Server.PID)))
	return err
}
