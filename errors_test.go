package tinystore

import (
	"errors"
	"testing"
)

func TestALimitErrorNamesItsBoundAndIsItsKind(t *testing.T) {
	engines := errors.Join(errors.New("metrics resource limit"), ErrLimit)
	err := error(&LimitError{Name: "decoded samples", Wanted: 120000, Bound: 100000, Kind: engines})
	var reached *LimitError
	if !errors.As(err, &reached) || !errors.Is(err, ErrLimit) || !errors.Is(err, engines) || reached.Wanted <= reached.Bound {
		t.Fatalf("%v: not the limit it reached", err)
	}
	if plain := (&LimitError{Name: "store memory", Wanted: 2, Bound: 1}).Error(); plain != "resource limit: store memory: 2 past 1" {
		t.Fatalf("Error() = %q", plain)
	}
}
