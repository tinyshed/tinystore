package jobs

import (
	"encoding/json"
	"errors"
	"fmt"
	"reflect"

	"github.com/tinyshed/tinystore"
)

// errUnreadable is a value that no longer reads into its queue's type: its job
// fails for good with the reason rather than stopping the queue
var errUnreadable = errors.New("the value no longer reads into the queue's type")

// valueCodec writes a queue's values as JSON, so that a worker in another
// language reads what Go wrote, and keeps a []byte or json.RawMessage as it is
type valueCodec[V any] struct {
	raw bool
}

func newValueCodec[V any]() valueCodec[V] {
	kind := reflect.TypeFor[V]()
	return valueCodec[V]{raw: kind == reflect.TypeFor[[]byte]() || kind == reflect.TypeFor[json.RawMessage]()}
}

func (c valueCodec[V]) encode(value V) ([]byte, error) {
	var encoded []byte
	switch bytes := any(value).(type) {
	case []byte:
		encoded = bytes
	case json.RawMessage:
		encoded = bytes
	default:
		var err error
		if encoded, err = json.Marshal(value); err != nil {
			return nil, fmt.Errorf("%w: jobs: a value JSON cannot write: %w", tinystore.ErrInvalid, err)
		}
	}
	if len(encoded) > maxValue {
		return nil, fmt.Errorf("%w: jobs: a value of %d bytes, over 1 MiB", tinystore.ErrLimit, len(encoded))
	}
	return encoded, nil
}

// decode reads a value back; one that no longer reads into V, because the
// type changed, fails its job for good rather than stopping the queue
func (c valueCodec[V]) decode(encoded []byte) (V, error) {
	var value V
	if c.raw {
		reflect.ValueOf(&value).Elem().SetBytes(append([]byte(nil), encoded...))
		return value, nil
	}
	if err := json.Unmarshal(encoded, &value); err != nil {
		return value, fmt.Errorf("%w %T: %w", errUnreadable, value, err)
	}
	return value, nil
}

// kept is a value as a job's row holds it: in the row, or spilled to a row of
// its own past 512 bytes, where a claim of the jobs around it need not read it
type kept struct {
	inline  []byte
	spilled []byte
}

func keep(encoded []byte) kept {
	if len(encoded) > inlineValue {
		return kept{spilled: encoded}
	}
	if encoded == nil {
		encoded = []byte{}
	}
	return kept{inline: encoded}
}

func (k kept) size() int {
	return len(k.inline) + len(k.spilled)
}
