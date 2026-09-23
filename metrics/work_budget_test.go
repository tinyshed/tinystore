package metrics

import (
	"context"
	"database/sql"
	"errors"
	"testing"
	"time"
)

func TestSharedWorkBudgetBoundsTwoStoresAndHonorsCancellation(t *testing.T) {
	limits := Limits{Series: 1, Blocks: 16, PayloadBytes: 4096, DecodedSamples: 512, OutputSamples: 512}
	readWeight, reservationErr := readReservation(limits)
	if reservationErr != nil {
		t.Fatal(reservationErr)
	}
	budget, budgetErr := NewWorkBudget(readWeight)
	if budgetErr != nil {
		t.Fatal(budgetErr)
	}
	options := Options{SharedBudget: budget, MaxReaders: 1, MaxConcurrentReads: 1, Limits: limits}
	first, _ := openTestStore(t, options)
	second, _ := openTestStore(t, options)
	request := Range{Matchers: testSeries().Labels, From: testEpoch, To: testEpoch + 1}
	held, entered := make(chan struct{}), make(chan struct{})
	defer func() {
		select {
		case <-held:
		default:
			close(held)
		}
	}()
	viewDone := make(chan error, 1)
	go func() {
		viewDone <- first.file.View(t.Context(), func(*sql.Tx) error {
			close(entered)
			<-held
			return nil
		})
	}()
	<-entered
	readDone := make(chan error, 1)
	go func() {
		_, readErr := first.Read(t.Context(), request)
		readDone <- readErr
	}()
	deadline := time.After(time.Second)
	for {
		used, _ := budget.Usage()
		if used == readWeight {
			break
		}
		select {
		case <-deadline:
			t.Fatal("first store did not reserve the shared budget")
		case <-time.After(time.Millisecond):
		}
	}
	readCtx, cancelRead := context.WithTimeout(t.Context(), 100*time.Millisecond)
	defer cancelRead()
	if _, err := second.Read(readCtx, request); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("second store read wait: %v", err)
	}
	close(held)
	if err := <-viewDone; err != nil {
		t.Fatal(err)
	}
	if err := <-readDone; err != nil {
		t.Fatal(err)
	}
	if used, _ := budget.Usage(); used != 0 {
		t.Fatalf("read reservation leaked %d bytes", used)
	}
	if _, err := second.Read(t.Context(), request); err != nil {
		t.Fatalf("read after release: %v", err)
	}
	batch := []Batch{{Series: testSeries(), Samples: testSamples(1)}}
	ingestWeight, reservationErr := first.ingestReservation(batch)
	if reservationErr != nil {
		t.Fatal(reservationErr)
	}
	ingestBudget, budgetErr := NewWorkBudget(ingestWeight)
	if budgetErr != nil {
		t.Fatal(budgetErr)
	}
	first.opts.SharedBudget = ingestBudget
	second.opts.SharedBudget = ingestBudget
	if err := ingestBudget.acquire(t.Context(), ingestWeight); err != nil {
		t.Fatal(err)
	}
	ingestCtx, cancelIngest := context.WithTimeout(t.Context(), 100*time.Millisecond)
	defer cancelIngest()
	if err := second.Ingest(ingestCtx, batch); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("second store ingest wait: %v", err)
	}
	ingestBudget.release(ingestWeight)
	if err := second.Ingest(t.Context(), batch); err != nil {
		t.Fatalf("ingest after release: %v", err)
	}
	if used, peak := ingestBudget.Usage(); used != 0 || peak > ingestWeight {
		t.Fatalf("ingest reservations used=%d peak=%d capacity=%d", used, peak, ingestWeight)
	}
	maintenanceWeight, reserveErr := second.maintenanceReservation()
	if reserveErr != nil {
		t.Fatal(reserveErr)
	}
	maintenanceBudget, budgetErr := NewWorkBudget(maintenanceWeight)
	if budgetErr != nil {
		t.Fatal(budgetErr)
	}
	first.opts.SharedBudget = maintenanceBudget
	second.opts.SharedBudget = maintenanceBudget
	if err := maintenanceBudget.acquire(t.Context(), maintenanceWeight); err != nil {
		t.Fatal(err)
	}
	maintenanceCtx, cancelMaintenance := context.WithTimeout(t.Context(), 100*time.Millisecond)
	defer cancelMaintenance()
	if _, err := second.Maintain(maintenanceCtx); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("second store maintenance wait: %v", err)
	}
	maintenanceBudget.release(maintenanceWeight)
	if _, err := second.Maintain(t.Context()); err != nil {
		t.Fatalf("maintenance after release: %v", err)
	}
	if used, peak := maintenanceBudget.Usage(); used != 0 || peak > maintenanceWeight {
		t.Fatalf("maintenance reservations used=%d peak=%d capacity=%d", used, peak, maintenanceWeight)
	}
	if _, err := NewWorkBudget(0); !errors.Is(err, ErrInvalid) {
		t.Fatalf("zero shared budget: %v", err)
	}
}
