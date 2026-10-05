package jobs

import (
	"context"
	"database/sql"
	"errors"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// A due job whose group has no room is parked: its next moves parkedBy later,
// past every time a job may have, so that the next claim does not meet it
// again, and moves back when its group frees a place. A claim parks a job
// once, rather than passing over a group's backlog each time, which would let
// one busy group starve every group behind it.
//
// The _tinystore_jobs_parked index holds the rows from parkedFrom on, and a
// query reaches it only by naming that bound as the index does, in its digits,
// and the index itself: the primary key's range looks as good to a planner
// without statistics, and walks the parked rows of every group.
const (
	parkedBy       = 1 << 60
	parkedFrom     = 1 << 59
	parkedFromText = "576460752303423488"
)

// the rows a claim parks at most, so that a backlog holds the writer briefly
const parkAtMost = 1000

// dueOf is when a job is due: its next, or a parked job's before it was parked
func dueOf(next int64) int64 {
	if next >= parkedFrom {
		return next - parkedBy
	}
	return next
}

// what parking reads and writes: the running jobs of each group, a row moved
// with its key and a lease it outlived, and the first parked row of a group
const (
	runningInGroups = `select j.grp, count(*) from _tinystore_jobs_leases l
			join _tinystore_jobs j on j.queue = l.queue and j.next = l.next and j.id = l.id
		where l.queue = ?1 and l.until > ?2 and j.grp is not null
		group by j.grp`
	moveRow     = `update _tinystore_jobs set next = ?4 where queue = ?1 and next = ?2 and id = ?3`
	moveLease   = `update _tinystore_jobs_leases set next = ?2 where id = ?1`
	firstParked = `select next, id, key from _tinystore_jobs indexed by _tinystore_jobs_parked
		where queue = ?1 and grp = ?2 and next >= ` + parkedFromText + ` order by next, id limit 1`
	unparkJobs = `update _tinystore_jobs set next = next - ?2 where queue = ?1 and next >= ` + parkedFromText
	unparkKeys = `update _tinystore_jobs_keys set next = next - ?2 where queue = ?1 and next >= ` + parkedFromText
)

// claimByGroup leases due jobs in the order of their time, as claimInOrder
// does, and parks those of a group whose MaxRunningInGroup places are taken,
// counting what it leases. Past parkAtMost it stops, with more due jobs to
// look at.
func claimByGroup(ctx context.Context, w sqlite.Writer, c claiming) (claimed, error) {
	running, err := runningByGroup(ctx, w, c.queue, c.now)
	if err != nil {
		return claimed{}, err
	}
	g := groupClaim{claiming: c, running: running}
	for len(g.got.rows) < c.limit {
		asked := c.limit - len(g.got.rows)
		due, err := dueRows(ctx, w, c, asked)
		if err != nil {
			return g.got, err
		}
		for i := range due {
			if err = g.take(ctx, w, &due[i]); err != nil || g.got.more {
				return g.got, err
			}
		}
		if len(due) < asked {
			break
		}
	}
	return g.got, nil
}

// groupClaim is one claim of a queue with MaxRunningInGroup: the running jobs
// of each group, those it leased counted, the rows it parked, and what it got
type groupClaim struct {
	claiming
	running map[string]int
	parked  int
	got     claimed
}

// take leases a due row whose group has room, and parks one whose group has
// none, or says there is more to look at past parkAtMost
func (g *groupClaim) take(ctx context.Context, w sqlite.Writer, row *claimedRow) error {
	group := row.group.String
	if row.group.Valid && g.running[group] >= g.maxInGroup {
		if g.parked == parkAtMost {
			g.got.more = true
			return nil
		}
		g.parked++
		return moveTo(ctx, w, g.queue, *row, row.next+parkedBy)
	}
	leased, err := g.got.lease(ctx, w, g.claiming, row)
	if leased && row.group.Valid {
		g.running[group]++
	}
	return err
}

// lease leases a due row, or fails it for good past its attempts, in which
// case a job of its group may take the place it no longer wants
func (got *claimed) lease(ctx context.Context, w sqlite.Writer, c claiming, row *claimedRow) (bool, error) {
	leased, err := leaseRow(ctx, w, c, row)
	switch {
	case err != nil:
		return false, err
	case leased:
		got.rows = append(got.rows, *row)
		return true, nil
	}
	got.abandoned = append(got.abandoned, row.id)
	if c.maxInGroup > 0 && row.group.Valid {
		return false, unparkOne(ctx, w, c.queue, row.group.String)
	}
	return false, nil
}

// runningByGroup counts the live leases of each group of the queue
func runningByGroup(ctx context.Context, w sqlite.Writer, queue, now int64) (map[string]int, error) {
	rows, err := w.QueryContext(ctx, runningInGroups, queue, now) //nolint:rowserrcheck // EachRow checks Err
	if err != nil {
		return nil, err
	}
	running := map[string]int{}
	err = sqlite.EachRow(rows, "running groups", func(rows *sql.Rows) error {
		var group string
		var count int
		scanErr := rows.Scan(&group, &count)
		running[group] = count
		return scanErr
	})
	return running, err
}

// moveTo moves a job's row to next, with its key and a lease it outlived
func moveTo(ctx context.Context, w sqlite.Writer, queue int64, row claimedRow, next int64) error {
	if _, err := w.ExecContext(ctx, moveRow, queue, row.next, row.id, next); err != nil {
		return err
	}
	if _, err := w.ExecContext(ctx, moveLease, row.id, next); err != nil {
		return err
	}
	return moveKeyTo(ctx, w, queue, row.key.String, next)
}

// unparkOne gives the first parked job of a group back its time, once the
// group has freed a place
func unparkOne(ctx context.Context, w sqlite.Writer, queue int64, group string) error {
	var row claimedRow
	err := sqlite.QueryRowByKey(ctx, w, firstParked, queue, group).Scan(&row.next, &row.id, &row.key)
	if errors.Is(err, sql.ErrNoRows) {
		return nil
	}
	if err != nil {
		return err
	}
	return moveTo(ctx, w, queue, row, row.next-parkedBy)
}

// unparkAll gives every parked job of the queue back its time, for a queue
// opened with another MaxRunningInGroup than the one that parked them. No
// lease is left to move: the store gave back every lease when it opened, and
// only this process claims.
func unparkAll(ctx context.Context, w sqlite.Writer, queue int64) error {
	if _, err := w.ExecContext(ctx, unparkJobs, queue, int64(parkedBy)); err != nil {
		return err
	}
	_, err := w.ExecContext(ctx, unparkKeys, queue, int64(parkedBy))
	return err
}
