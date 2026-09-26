package main

import (
	"context"
	"errors"
	"fmt"
	"math/rand/v2"
	"strings"
	"sync/atomic"
	"time"

	"github.com/tinyshed/tinystore"
)

// the notes of a user, the saves of a draft written before the phase, and the
// bytes of a draft's text
const (
	notesPerUser  = 4
	preparedSaves = 1
	shortestDraft = 100
	longestDraft  = 1500
)

// draftText is what a draft's body is cut from
var draftText = strings.Repeat("A draft is kept while its note is written, and saved every few seconds. ", 25)

// draftsLoad is the draft autosave case under load. Every note is open in two
// tabs, each on its own worker when there are two or more; a tab saves 85 times
// in a hundred and opens its note 15, and a tab whose save conflicted opens its
// note before it saves again. A draft holds 100 to 1500 bytes of text
type draftsLoad struct {
	drafts drafts
	keys   int
	now    func() time.Time
	saved  []atomic.Int64 // each note's saves that went through in the phase
	opened []tab          // each note as a tab that has just opened it, before the phase
}

func loadDrafts(ctx context.Context, b *backend, keys int) (workload, error) {
	store, err := openDrafts(ctx, b)
	if err != nil {
		return nil, err
	}
	return &draftsLoad{drafts: store, keys: keys, now: b.now, saved: make([]atomic.Int64, keys)}, nil
}

// prepare saves a draft of every note in a scattered order, each last saved
// over the last six days, then opens every note once, so that the phase times
// tabs that are open rather than their first reads
func (l *draftsLoad) prepare(ctx context.Context) error {
	now, order := l.now(), newScatter(l.keys)
	err := inBatches(l.keys, func(from, to int) error {
		batch := make([]storedDraft, 0, to-from)
		for n := from; n < to; n++ {
			batch = append(batch, preparedDraft(int64(order.at(n)), now))
		}
		return l.drafts.preload(ctx, batch)
	})

	l.opened = make([]tab, l.keys)
	for note := range l.opened {
		if err != nil {
			break
		}
		l.opened[note].note = int64(note)
		_, err = l.open(ctx, &l.opened[note])
	}
	return err
}

func preparedDraft(note int64, now time.Time) storedDraft {
	d := draft{Body: draftBody(seeded(seedDraftBodies, note)), Saves: preparedSaves}
	expires := now.Add(draftTerm - spread(seedDraftAges, note, 6*day))
	return storedDraft{user: noteUser(note), note: note, draft: d, expires: expires}
}

func draftBody(random *rand.Rand) string {
	return draftText[:shortestDraft+random.IntN(longestDraft-shortestDraft)]
}

func noteUser(note int64) int64 {
	return note / notesPerUser
}

// tab is what one tab knows of its note: the version it read and the saves the
// draft had then; a tab that is not current opens its note before it saves
type tab struct {
	note    int64
	version string
	saves   int64
	current bool
}

// requests drives the worker's own tabs, the tabs t of 0…2·keys-1 with t mod
// workers the worker, tab t being one of note t/2's two
func (l *draftsLoad) requests(worker, workers int) (request, error) {
	var tabs []tab
	for t := worker; t < 2*l.keys; t += workers {
		tabs = append(tabs, l.opened[t/2])
	}
	if len(tabs) == 0 {
		return nil, fmt.Errorf("drafts needs -keys of at least half the workers, not %d for %d", l.keys, workers)
	}
	random := seeded(seedDraftRequests, int64(worker))
	return func(ctx context.Context) (step, error) {
		t := &tabs[random.IntN(len(tabs))]
		if !t.current || random.IntN(100) < 15 {
			return l.open(ctx, t)
		}
		return l.save(ctx, t, random)
	}, nil
}

func (l *draftsLoad) open(ctx context.Context, t *tab) (step, error) {
	d, version, err := l.drafts.open(ctx, noteUser(t.note), t.note)
	if err != nil {
		return step{"open", ""}, err
	}
	t.version, t.saves, t.current = version, d.Saves, true
	return step{"open", "opened"}, nil
}

func (l *draftsLoad) save(ctx context.Context, t *tab, random *rand.Rand) (step, error) {
	next := draft{Body: draftBody(random), Saves: t.saves + 1}
	version, err := l.drafts.save(ctx, draftSave{user: noteUser(t.note), note: t.note, draft: next, read: t.version})
	switch {
	case errors.Is(err, tinystore.ErrConflict):
		t.current = false
		return step{"save", "conflict"}, nil
	case err != nil:
		return step{"save", ""}, err
	}
	t.version, t.saves = version, next.Saves
	l.saved[t.note].Add(1)
	return step{"save", "saved"}, nil
}

// verify finds a draft with fewer saves than went through in the phase: each
// save counts one more than the draft its tab read, so that two saves from one
// version, the second over the first, leave the count short
func (l *draftsLoad) verify(ctx context.Context) error {
	short := 0
	for i := range l.saved {
		saved, note := l.saved[i].Load(), int64(i)
		if saved == 0 {
			continue
		}
		d, _, err := l.drafts.open(ctx, noteUser(note), note)
		if err != nil {
			return err
		}
		if d.Saves != preparedSaves+saved {
			short++
		}
	}
	if short > 0 {
		return fmt.Errorf("%d drafts have fewer saves than went through: a save went over another", short)
	}
	return nil
}

// checkDrafts holds an implementation to the case: a save goes through only at
// the version its tab read, a note without a draft takes one save, and a draft
// is gone a week after its last save, with its version
func checkDrafts(ctx context.Context, b *backend, clock *fakeClock) error {
	store, err := openDrafts(ctx, b)
	if err != nil {
		return err
	}
	var t transcript
	open := func() string {
		d, version, openErr := store.open(ctx, 1, 10)
		t.note(d.Body, openErr)
		return version
	}
	save := func(body, read string) string {
		version, saveErr := store.save(ctx, draftSave{user: 1, note: 10, draft: draft{Body: body}, read: read})
		t.note("ok", saveErr)
		return version
	}

	open()
	first := save("a", "")
	save("b", "")
	read := open()
	t.note(read == first, nil)
	second := save("b", read)
	save("c", read)
	open()
	clock.advance(draftTerm + time.Minute)
	open()
	save("d", second)
	save("d", "")

	return t.differs("", "ok", "error: conflict", "a", "true", "ok", "error: conflict", "b", "", "error: conflict",
		"ok")
}
