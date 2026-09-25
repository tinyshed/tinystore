package records_test

import (
	"context"
	"fmt"
	"log/slog"
	"os"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/records"
)

func ExampleStore() {
	ctx := context.Background()
	directory, err := os.MkdirTemp("", "tinystore-records-")
	if err != nil {
		panic(err)
	}
	defer os.RemoveAll(directory)

	// Manual: the example flushes and seals itself instead of waiting for the store
	store, err := tinystore.Open(ctx, directory, tinystore.Options{Manual: true})
	if err != nil {
		panic(err)
	}
	defer store.Close(ctx)
	logs, err := records.Open(ctx, store, records.Options{})
	if err != nil {
		panic(err)
	}

	// the application's own lines, through slog
	logger := slog.New(logs.Handler("notes")).With("request_id", "r-17")
	logger.Warn("slow request", "route", "/notes", "ms", 1200)
	if err = logs.Flush(ctx); err != nil {
		panic(err)
	}

	// an event from elsewhere: no level, no body, a context and attributes
	err = logs.Append(ctx, records.Record{
		At: time.Now(), Stream: "web", Name: "click",
		Context: []records.Field{records.String("session", "s-42")},
		Attrs:   []records.Field{records.String("element", "buy"), records.Int("x", 812)},
	})
	if err != nil {
		panic(err)
	}

	page, err := logs.Read(ctx, records.Query{From: time.Now().Add(-time.Minute)})
	if err != nil {
		panic(err)
	}
	for _, record := range page.Records {
		fmt.Println(record.Stream, record.Name, record.Context, record.Attrs)
	}
	// Output:
	// notes log [{request_id "r-17"}] [{route "/notes"} {ms 1200}]
	// web click [{session "s-42"}] [{element "buy"} {x 812}]
}
