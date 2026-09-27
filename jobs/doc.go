// Package jobs keeps work that runs at its time in jobs.db inside a
// tinystore.Store: typed queues whose jobs wait for their time, are leased to a
// worker, retried, repeated by cron text, and run at least once.
//
//	queues, err := jobs.Open(ctx, store, jobs.Options{})
//	reminders, err := jobs.OpenQueue[Reminder](ctx, queues, "reminders")
//	err = reminders.Enqueue(ctx, Reminder{User: 42, Text: "call mom"}, jobs.At(evening))
//	go reminders.Work(ctx, remind, jobs.Workers(4))
//
// Every call is one operation of a protocol a server could speak to another
// language: Work is a loop over Claim and a Job's Ack, Retry, Fail, Snooze and
// Extend. A queue keeps its jobs in the order of their time and knows when the
// next one is due, so nothing polls. Start with queue.go; every other file
// holds one step:
//
//	jobs.go      Open, Close, Snapshot: the handle, admission and job ids
//	queue.go     OpenQueue, OpenSchedule, WithTx: a handle on a queue
//	options.go   the engine's bounds, the options of queues, enqueues, claims and Work
//	enqueue.go   Enqueue, Update, Cancel, Tx: what a program puts in a queue
//	read.go      Get, Scan, All, Entry, Query, Page, JobError
//	claim.go     Claim and a Job's settlements: the lease that is their token
//	work.go      Work: the loop that claims, runs and settles in one write
//	alarm.go     a queue's next time in memory, so that nothing polls
//	repeat.go    Cron, Daily, Every: repeats kept as text, their next time
//	values.go    a value as JSON, and where a row keeps it
//	maintain.go  Maintain: failed jobs and done keys a queue keeps no longer
package jobs
