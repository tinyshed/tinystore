package wire_test

import (
	"errors"
	"reflect"
	"testing"

	"github.com/tinyshed/tinystore/server/wire"
)

func TestMessagesReadBackAsTheyWereWritten(t *testing.T) {
	hello := wire.Hello{
		Protocol: 1, Client: "tinystore-python/0.1", Token: "t0k3n", MaxBody: 64 << 10,
		StreamCredit: 256 << 10, Challenge: []byte("0123456789abcdef"),
	}
	var helloRead wire.Hello
	if err := helloRead.Decode(hello.Append(nil)); err != nil || !reflect.DeepEqual(helloRead, hello) {
		t.Errorf("HELLO read as %+v, %v", helloRead, err)
	}

	welcome := wire.Welcome{
		Protocol: 1, Server: "0.4.0", Instance: []byte("0123456789abcdef"),
		Capability: wire.Admin, MaxBody: 1 << 20, InFlight: 256, ConnectionCredit: 8 << 20,
		StreamCredit: 1 << 20, Engines: []string{"kv", "jobs"}, Now: 1_790_000_000_123,
		Proof: []byte("an hmac-sha256 of thirty-two by"),
	}
	var welcomeRead wire.Welcome
	if err := welcomeRead.Decode(welcome.Append(nil)); err != nil || !reflect.DeepEqual(welcomeRead, welcome) {
		t.Errorf("WELCOME read as %+v, %v", welcomeRead, err)
	}

	goAway := wire.GoAway{Code: wire.CodeUnavailable, Message: "the server is closing"}
	var goAwayRead wire.GoAway
	if err := goAwayRead.Decode(goAway.Append(nil)); err != nil || goAwayRead != goAway {
		t.Errorf("GOAWAY read as %+v, %v", goAwayRead, err)
	}
}

func TestAnErrorKeepsANameThatIsNotUTF8(t *testing.T) {
	failed := &wire.Error{
		Code: wire.CodeInvalid, Message: "an empty key",
		What: map[string]string{"bucket": "codes", "key": "\xff\x00raw"},
	}
	var read wire.Error
	if err := read.Decode(failed.Append(nil)); err != nil || !reflect.DeepEqual(&read, failed) {
		t.Fatalf("read as %+v, %v", read, err)
	}
}

// A reader takes its fields in any order and names those it does not know, so
// that a server refuses them in a request and a client may skip them in an
// answer, as SkipUnknown does.
func TestAMessageReadsWhatItKnowsAndNamesWhatItDoesNot(t *testing.T) {
	body := wire.AppendMap(nil, 4)
	body = wire.AppendStr(wire.AppendUint(body, 2), "tinystore-bun/0.2")
	body = wire.AppendMap(wire.AppendUint(body, 40), 1)
	body = wire.AppendArray(wire.AppendStr(body, "later"), 1)
	body = wire.AppendNil(body)
	body = wire.AppendUint(wire.AppendUint(body, 1), 1)
	body = wire.AppendBin(wire.AppendUint(body, 700), []byte{1, 2})

	var hello wire.Hello
	err := hello.Decode(body)
	var unknown *wire.UnknownFieldsError
	if !errors.As(err, &unknown) || !errors.Is(err, wire.ErrUnknownField) || unknown.List() != "40,700" {
		t.Fatalf("decoded with %v", err)
	}
	if wire.SkipUnknown(err) != nil {
		t.Fatalf("skipping the unknown fields left %v", wire.SkipUnknown(err))
	}
	if hello.Protocol != 1 || hello.Client != "tinystore-bun/0.2" || hello.MaxBody != 0 {
		t.Fatalf("read as %+v", hello)
	}

	broken := append(body[:len(body):len(body)], 0xc1)
	if err = wire.SkipUnknown(hello.Decode(broken)); !errors.Is(err, wire.ErrMessage) {
		t.Fatalf("a body that does not decode, its unknown fields skipped: %v", err)
	}
}

func TestAFieldTakesNilOnlyWhereItSaysSo(t *testing.T) {
	m := wire.BeginMap(nil)
	m.Uint(1, 1)
	m.Key(2)
	m.SetBuf(wire.AppendNil(m.Buf()))
	var hello wire.Hello
	if err := hello.Decode(m.End()); !errors.Is(err, wire.ErrMessage) {
		t.Fatalf("a nil client taken: %v", err)
	}
}

func TestAFieldOutOfItsRangeIsRefused(t *testing.T) {
	m := wire.BeginMap(nil)
	m.Uint(1, 1)
	m.Uint(4, 1<<32)
	var hello wire.Hello
	if err := hello.Decode(m.End()); !errors.Is(err, wire.ErrMessage) {
		t.Fatalf("a max body past u32 taken: %v", err)
	}
}
