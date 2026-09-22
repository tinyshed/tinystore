package metrics

import (
	"context"
	"embed"
	"errors"
	"fmt"
	"io/fs"
	"math"
	"sync"
	"sync/atomic"
	"time"

	"github.com/tinyshed/tinystore/codec"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

//go:embed migrations/*.sql
var migrationFiles embed.FS

type Store struct {
	file                                         *sqlite.File
	encoder, decoder                             *codec.Codec
	metadata                                     *metadataCodec
	opts                                         Options
	now                                          func() time.Time
	mu                                           sync.Mutex
	active                                       int
	closing                                      bool
	drained, closed                              chan struct{}
	closeErr                                     error
	ingested, rejected, queried, sealed, expired atomic.Uint64
}

func Open(ctx context.Context, path string, options Options) (*Store, error) {
	opts, err := normalizeOptions(options)
	if err != nil {
		return nil, err
	}
	f, err := sqlite.Open(ctx, path)
	if err != nil {
		return nil, fmt.Errorf("open metrics file: %w", err)
	}
	scripts, err := fs.Sub(migrationFiles, "migrations")
	if err != nil {
		return nil, errors.Join(fmt.Errorf("open metrics migrations: %w", err), f.Close())
	}
	if err = f.Migrate(ctx, 0x544d4554, scripts); err != nil {
		return nil, errors.Join(fmt.Errorf("migrate metrics: %w", err), f.Close())
	}
	encoder, err := codec.New()
	if err != nil {
		return nil, errors.Join(fmt.Errorf("create metrics encoder: %w", err), f.Close())
	}
	decoder, err := codec.New()
	if err != nil {
		return nil, errors.Join(fmt.Errorf("create metrics decoder: %w", err), encoder.Close(), f.Close())
	}
	metadata, err := newMetadataCodec()
	if err != nil {
		return nil, errors.Join(err, encoder.Close(), decoder.Close(), f.Close())
	}
	return &Store{file: f, encoder: encoder, decoder: decoder, metadata: metadata, opts: opts, now: time.Now, drained: make(chan struct{}), closed: make(chan struct{})}, nil
}

func (s *Store) enter(ctx context.Context) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closing {
		return ErrClosed
	}
	s.active++
	return nil
}

func (s *Store) leave() {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.active--
	if s.closing && s.active == 0 {
		close(s.drained)
	}
}

// Close stops admission and drains in-flight work; cancellation stops waiting, not cleanup.
func (s *Store) Close(ctx context.Context) error {
	s.mu.Lock()
	if !s.closing {
		s.closing = true
		if s.active == 0 {
			close(s.drained)
		}
		go func() {
			<-s.drained
			s.closeErr = errors.Join(s.encoder.Close(), s.decoder.Close(), s.metadata.close(), s.file.Close())
			close(s.closed)
		}()
	}
	s.mu.Unlock()
	select {
	case <-s.closed:
		return s.closeErr
	case <-ctx.Done():
		return ctx.Err()
	}
}

func (s *Store) Stats() Stats {
	return Stats{IngestedSamples: s.ingested.Load(), RejectedBatches: s.rejected.Load(), Queries: s.queried.Load(), SealedBlocks: s.sealed.Load(), ExpiredSamples: s.expired.Load()}
}

func normalizeOptions(o Options) (Options, error) {
	if o.Retention == 0 {
		o.Retention = 30 * 24 * time.Hour
	}
	if o.SnapshotTimeout == 0 {
		o.SnapshotTimeout = 5 * time.Second
	}
	if o.MaxBlockSpan == 0 {
		o.MaxBlockSpan = 24 * time.Hour
	}
	if o.MaxBlockSpan < time.Millisecond || o.MaxBlockSpan%time.Millisecond != 0 {
		return o, fmt.Errorf("%w: block span", ErrInvalid)
	}
	if o.Retention < time.Millisecond || o.Retention%time.Millisecond != 0 || o.Lateness < 0 || o.Lateness%time.Millisecond != 0 || o.SnapshotTimeout < 0 {
		return o, fmt.Errorf("%w: durations", ErrInvalid)
	}
	fields := []*int{&o.MaxSeries, &o.MaxHeadSamples, &o.MaxBatchSamples, &o.MaxBatchBytes, &o.MaintenanceSeries}
	defaults := []int{100000, 4096, 10000, 4 << 20, 64}
	for i, p := range fields {
		if *p < 0 {
			return o, fmt.Errorf("%w: negative capacity", ErrInvalid)
		}
		if *p == 0 {
			*p = defaults[i]
		}
	}
	if o.MaxHeadBytes == 0 {
		o.MaxHeadBytes = 256 << 10
	}
	if o.MaxHeadBytes < 1 || o.MaxHeadBytes > maximumHeadBytes || o.MaxHeadSamples > 1<<20 {
		return o, fmt.Errorf("%w: mutable head capacity", ErrInvalid)
	}
	defaultsLimits := Limits{Series: 1000, Blocks: 4096, PayloadBytes: 16 << 20, DecodedSamples: 1 << 20, OutputSamples: 100000}
	configured := []*int{&o.Limits.Series, &o.Limits.Blocks, &o.Limits.PayloadBytes, &o.Limits.DecodedSamples, &o.Limits.OutputSamples}
	limitDefaults := []int{defaultsLimits.Series, defaultsLimits.Blocks, defaultsLimits.PayloadBytes, defaultsLimits.DecodedSamples, defaultsLimits.OutputSamples}
	for i, capacity := range configured {
		if *capacity < 0 || *capacity == math.MaxInt {
			return o, fmt.Errorf("%w: query capacity", ErrInvalid)
		}
		if *capacity == 0 {
			*capacity = limitDefaults[i]
		}
	}
	return o, nil
}

func narrowLimits(want, ceiling Limits) (Limits, error) {
	a := []*int{&want.Series, &want.Blocks, &want.PayloadBytes, &want.DecodedSamples, &want.OutputSamples}
	b := []int{ceiling.Series, ceiling.Blocks, ceiling.PayloadBytes, ceiling.DecodedSamples, ceiling.OutputSamples}
	for i, p := range a {
		if *p < 0 {
			return want, fmt.Errorf("%w: negative query limit", ErrInvalid)
		}
		if *p == 0 || *p > b[i] {
			*p = b[i]
		}
	}
	return want, nil
}

func earlier(at, delta int64) int64 {
	if at < math.MinInt64+delta {
		return math.MinInt64
	}
	return at - delta
}

func (s *Store) cutoff() int64 { return earlier(s.now().UnixMilli(), s.opts.Retention.Milliseconds()) }
