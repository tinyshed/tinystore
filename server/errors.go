package server

import (
	"context"
	"errors"
	"strconv"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/blobs"
	"github.com/tinyshed/tinystore/jobs"
	"github.com/tinyshed/tinystore/kv"
	"github.com/tinyshed/tinystore/metrics"
	"github.com/tinyshed/tinystore/records"
	"github.com/tinyshed/tinystore/server/wire"
	"github.com/tinyshed/tinystore/sqldb"
)

// failure is an error as the wire carries it: the code its sentinel means,
// what it says, and the item it names
func failure(ctx context.Context, err error) *wire.Error {
	var already *wire.Error
	if errors.As(err, &already) {
		return already
	}
	return &wire.Error{Code: codeOf(ctx, err), Message: err.Error(), What: whatOf(err)}
}

// The errors a code is found by, the first that matches: the store's sentinels,
// whose codes a client's error classes mirror, and the server's own.
var sentinels = []struct {
	err  error
	code wire.Code
}{
	{kv.ErrOutcomeUnknown, wire.CodeOutcomeUnknown}, // every engine's, the same error
	{errDataConnection, wire.CodePermission},
	{errAdminOnly, wire.CodePermission},
	{tinystore.ErrInvalid, wire.CodeInvalid},
	{tinystore.ErrLimit, wire.CodeLimit},
	{tinystore.ErrClosed, wire.CodeClosed},
	{tinystore.ErrInUse, wire.CodeInUse},
	{tinystore.ErrConflict, wire.CodeConflict},
	{tinystore.ErrCorrupt, wire.CodeCorrupt},
	{tinystore.ErrTooOld, wire.CodeTooOld},
	{tinystore.ErrTooNew, wire.CodeTooNew},
	{tinystore.ErrSuspended, wire.CodeSuspended},
	{wire.ErrMessage, wire.CodeInvalid},
}

func codeOf(ctx context.Context, err error) wire.Code {
	for _, sentinel := range sentinels {
		if errors.Is(err, sentinel.err) {
			return sentinel.code
		}
	}
	cause := context.Cause(ctx)
	switch {
	case errors.Is(err, errCancelled), errors.Is(cause, errCancelled):
		return wire.CodeCancelled
	case errors.Is(err, errClosing), errors.Is(cause, errClosing):
		return wire.CodeUnavailable
	case errors.Is(err, context.DeadlineExceeded):
		return wire.CodeLimit
	}
	return wire.CodeInternal
}

// whatOf is the item an engine's error names, and the call of a batch it failed
func whatOf(err error) map[string]string {
	var op *opError
	if errors.As(err, &op) {
		return op.what()
	}
	var key *kv.KeyError
	if errors.As(err, &key) {
		return map[string]string{"bucket": key.Bucket, "key": key.Path}
	}
	var job *jobs.JobError
	if errors.As(err, &job) {
		return map[string]string{"queue": job.Queue, "key": job.Key}
	}
	var object *blobs.KeyError
	if errors.As(err, &object) {
		return map[string]string{"bucket": object.Bucket, "key": object.Path}
	}
	var constraint *sqldb.ConstraintError
	if errors.As(err, &constraint) {
		return constraintOf(constraint)
	}
	var series *metrics.SeriesError
	if errors.As(err, &series) {
		return wireLabels(metrics.Series{Name: series.Name, Labels: series.Labels})
	}
	return recordsWhat(err)
}

// recordsWhat names the record an append refused, by its place in the batch,
// or the damaged row a read or a follow met, as a drop names it
func recordsWhat(err error) map[string]string {
	var refused *records.RecordError
	if errors.As(err, &refused) {
		return map[string]string{"call": strconv.Itoa(refused.Index), "stream": refused.Stream, "name": refused.Name}
	}
	var damaged *records.DamageError
	if !errors.As(err, &damaged) {
		return nil
	}
	damage := wireDamage(damaged.Damage)
	what := map[string]string{"stream": damage.Stream, "reason": damage.Reason}
	for name, n := range map[string]int64{
		"segment": damage.Segment, "head row": damage.HeadRow, "from": damage.From, "to": damage.To,
	} {
		if n != 0 {
			what[name] = strconv.FormatInt(n, 10)
		}
	}
	return what
}

// constraintOf names the table and the constraint a write broke, as far as
// SQLite's message names them
func constraintOf(broken *sqldb.ConstraintError) map[string]string {
	what := map[string]string{}
	if broken.Table != "" {
		what["table"] = broken.Table
	}
	if broken.Constraint != "" {
		what["constraint"] = broken.Constraint
	}
	return what
}
