package spike

import (
	"bytes"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"slices"
	"time"
)

const (
	recordEventLimit = 1024
	recordByteLimit  = 256 << 10
	recordFieldLimit = 128
	recordCellLimit  = 64 << 10
	recordWorkLimit  = 4 << 20
)

type recordField struct {
	key, value string
}

type recordEvent struct {
	at      int64
	stream  string
	name    string
	level   *int64
	body    *string
	traceID []byte
	spanID  []byte
	context []recordField
	attrs   []recordField
}

type recordWireEvent struct {
	Version int             `json:"version"`
	Time    string          `json:"time"`
	Stream  string          `json:"stream"`
	Name    string          `json:"name"`
	Level   *int64          `json:"level,omitempty"`
	Body    *string         `json:"body,omitempty"`
	TraceID string          `json:"trace_id,omitempty"`
	SpanID  string          `json:"span_id,omitempty"`
	Context json.RawMessage `json:"context,omitempty"`
	Attrs   json.RawMessage `json:"attrs,omitempty"`
}

func parseRecordEvent(data []byte) (recordEvent, error) {
	if len(data) > recordByteLimit {
		return recordEvent{}, errors.New("record event exceeds byte limit")
	}
	fields, err := parseRecordFields(data)
	if err != nil {
		return recordEvent{}, fmt.Errorf("record envelope: %w", err)
	}
	seen := map[string]bool{}
	for _, field := range fields {
		if seen[field.key] {
			return recordEvent{}, errors.New("record envelope has a duplicate key")
		}
		seen[field.key] = true
	}
	var wire recordWireEvent
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if err = decoder.Decode(&wire); err != nil {
		return recordEvent{}, fmt.Errorf("record envelope: %w", err)
	}
	if decoder.Decode(new(any)) != io.EOF || wire.Version != 1 || wire.Name == "" {
		return recordEvent{}, errors.New("record envelope version, name or trailing data")
	}
	at, err := time.Parse(time.RFC3339Nano, wire.Time)
	if err != nil || !time.Unix(0, at.UnixNano()).Equal(at) {
		return recordEvent{}, errors.New("record time is not an int64 nanosecond instant")
	}
	context, err := parseRecordFields(wire.Context)
	if err != nil {
		return recordEvent{}, fmt.Errorf("record context: %w", err)
	}
	attrs, err := parseRecordFields(wire.Attrs)
	if err != nil {
		return recordEvent{}, fmt.Errorf("record attributes: %w", err)
	}
	trace, err := parseRecordID(wire.TraceID, 16)
	if err != nil {
		return recordEvent{}, err
	}
	span, err := parseRecordID(wire.SpanID, 8)
	if err != nil {
		return recordEvent{}, err
	}
	return recordEvent{
		at: at.UnixNano(), stream: wire.Stream, name: wire.Name, level: wire.Level,
		body: wire.Body, traceID: trace, spanID: span, context: context, attrs: attrs,
	}, nil
}

func parseRecordFields(data []byte) ([]recordField, error) {
	if len(data) == 0 {
		return nil, nil
	}
	decoder := json.NewDecoder(bytes.NewReader(data))
	opening, err := decoder.Token()
	if err != nil || opening != json.Delim('{') {
		return nil, errors.New("expected an object")
	}
	var fields []recordField
	for decoder.More() {
		key, keyErr := decoder.Token()
		if keyErr != nil {
			return nil, keyErr
		}
		name, ok := key.(string)
		if !ok || len(fields) == recordFieldLimit {
			return nil, errors.New("invalid key or too many fields")
		}
		var value json.RawMessage
		if err = decoder.Decode(&value); err != nil {
			return nil, err
		}
		fields = append(fields, recordField{name, string(value)})
	}
	if _, err = decoder.Token(); err != nil {
		return nil, err
	}
	if decoder.Decode(new(any)) != io.EOF {
		return nil, errors.New("trailing object data")
	}
	return fields, nil
}

func appendRecordString(out []byte, value string) []byte {
	out = binary.AppendUvarint(out, uint64(len(value)))
	return append(out, value...)
}

func appendRecordFields(out []byte, fields []recordField) []byte {
	out = binary.AppendUvarint(out, uint64(len(fields)))
	for _, field := range fields {
		out = appendRecordString(out, field.key)
		out = appendRecordString(out, field.value)
	}
	return out
}

func appendRecordEvent(out []byte, event recordEvent) []byte {
	out = binary.AppendVarint(out, event.at)
	out = appendRecordString(out, event.stream)
	out = appendRecordString(out, event.name)
	out = append(out, event.presence())
	if event.level != nil {
		out = binary.AppendVarint(out, *event.level)
	}
	if event.body != nil {
		out = appendRecordString(out, *event.body)
	}
	out = append(out, event.traceID...)
	out = append(out, event.spanID...)
	out = appendRecordFields(out, event.context)
	return appendRecordFields(out, event.attrs)
}

func checkRecordEvent(event recordEvent) error {
	if event.name == "" || len(event.context) > recordFieldLimit || len(event.attrs) > recordFieldLimit {
		return errors.New("record name or field limit")
	}
	if (event.traceID != nil && len(event.traceID) != 16) || (event.spanID != nil && len(event.spanID) != 8) {
		return errors.New("record trace or span length")
	}
	bytes := len(event.name) + len(event.stream)
	if event.body != nil {
		bytes += len(*event.body)
	}
	for _, fields := range [][]recordField{event.context, event.attrs} {
		for _, field := range fields {
			bytes += len(field.key) + len(field.value)
			if bytes > recordByteLimit || !json.Valid([]byte(field.value)) {
				return errors.New("record byte limit or invalid field JSON")
			}
		}
	}
	if bytes > recordByteLimit {
		return errors.New("record byte limit")
	}
	return nil
}

type recordStream struct {
	codec   *recordBlockCodec
	pending []recordEvent
	bytes   int
	cells   int
}

func (s *recordStream) add(event recordEvent) ([]byte, error) {
	if err := checkRecordEvent(event); err != nil {
		return nil, err
	}
	size := len(appendRecordEvent(nil, event))
	if size > recordByteLimit {
		return nil, errors.New("record exceeds block byte limit")
	}
	cells := len(event.context) + len(event.attrs) + 8
	var sealed []byte
	if len(s.pending) == recordEventLimit || s.bytes+size > recordByteLimit || s.cells+cells > recordCellLimit {
		var err error
		sealed, err = s.flush()
		if err != nil {
			return nil, err
		}
	}
	event.context, event.attrs = slices.Clone(event.context), slices.Clone(event.attrs)
	event.traceID, event.spanID = slices.Clone(event.traceID), slices.Clone(event.spanID)
	if event.level != nil {
		level := *event.level
		event.level = &level
	}
	if event.body != nil {
		body := *event.body
		event.body = &body
	}
	s.pending = append(s.pending, event)
	s.bytes += size
	s.cells += cells
	return sealed, nil
}

func (s *recordStream) flush() ([]byte, error) {
	if len(s.pending) == 0 {
		return nil, nil
	}
	block, err := s.codec.encode(s.pending)
	if err != nil {
		return nil, err
	}
	clear(s.pending)
	s.pending, s.bytes, s.cells = s.pending[:0], 0, 0
	return block, nil
}
