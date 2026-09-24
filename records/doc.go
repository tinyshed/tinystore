// Package records keeps an application's structured logs in records.db inside
// a tinystore.Store. Its Handler plugs into log/slog, so the application logs
// as it already does, and never waits for the file.
//
//	records.go   Open, Flush, Maintain, Close: the engine and its file
//	handler.go   the slog.Handler that queues, drops and counts
//	read.go      Read: records by time and level, always bounded
package records
