package jobs

import (
	"context"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/wire"
)

// Schedule is a repeat the code owns: one repeating job under its name, and
// the worker that runs it. The code's repeat replaces the one kept each time
// the program opens it.
type Schedule struct {
	source
	name string
}

// OpenSchedule opens a schedule of the store: jobs.Every(d) or
// jobs.Cron(expr, zone), one of them, and its attempts, backoff and timeout.
//
//	cleanup, err := jobs.OpenSchedule(store, "cleanup", jobs.Cron("10 3 * * *", "Europe/Berlin"))
func OpenSchedule(home tinystore.Home, name string, opts ...ScheduleOption) (*Schedule, error) {
	var o queueOptions
	for _, opt := range opts {
		opt.schedule(&o)
	}
	describe := "jobs schedule " + name
	if err := core.CheckName(name, "schedule"); err != nil {
		return nil, err
	}
	if o.err != nil {
		return nil, core.Wrap(core.Invalid, o.err, "%s", describe)
	}
	if (o.every == nil) == (o.cron == nil) {
		return nil, core.Errorf(core.Invalid, "%s: a schedule runs every span or by a cron, one of them", describe)
	}
	message := wire.JobsScheduleOpen{Name: name, Attempts: o.attempts, Backoff: o.backoff}
	var err error
	if message.Every, err = millis(o.every, describe+": every"); err != nil {
		return nil, err
	}
	if message.Timeout, err = millis(o.timeout, describe+": timeout"); err != nil {
		return nil, err
	}
	if o.cron != nil {
		if o.cron.timeZone == "" {
			return nil, core.Errorf(core.Invalid, "%s: a cron needs a time zone, \"UTC\" among them", describe)
		}
		message.Cron, message.TimeZone = &o.cron.expr, &o.cron.timeZone
	}
	of := home.CoreHome()
	s := &Schedule{name: name, source: source{
		store: of.Store, describe: describe, timeout: time.Minute,
		opener: &core.Opener{Method: wire.MethodJobsScheduleOpen, Body: wire.Encode(&message)},
		via:    core.ViaLink{Link: of.Store.Link},
	}}
	if o.timeout != nil {
		s.timeout = *o.timeout
	}
	return s, nil
}

// Work runs handler at each of the schedule's times until ctx ends or the
// store closes, as a queue's Work does.
func (s *Schedule) Work(ctx context.Context, handler func(ctx context.Context, run *Run) error) error {
	return keepWorking(ctx, &s.source, nil, func(ctx context.Context, _ wire.JobsHeld, run *Run) error {
		return handler(ctx, run)
	})
}

// RunDue runs the schedule's job when it is due, as a queue's RunDue does.
func (s *Schedule) RunDue(ctx context.Context, handler func(ctx context.Context, run *Run) error) error {
	_, err := runDue(ctx, &s.source, nil, func(ctx context.Context, _ wire.JobsHeld, run *Run) error {
		return handler(ctx, run)
	})
	return err
}

// Get is when the schedule runs next and how its last run went, what a page
// of cron jobs shows: its At, LastRun and Error.
func (s *Schedule) Get(ctx context.Context) (Job[struct{}], error) {
	job, found, err := get[struct{}](ctx, &s.source, s.name)
	if err == nil && !found {
		err = core.Errorf(core.NotFound, "%s: no job until the schedule is worked", s.describe)
	}
	return job, err
}

// Cancel removes the schedule's job, until the program opens it again; it
// says whether there was one.
func (s *Schedule) Cancel(ctx context.Context) (bool, error) {
	return cancel(ctx, &s.source, s.name)
}
