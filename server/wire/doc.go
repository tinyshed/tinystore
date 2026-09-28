// Package wire is the bytes a TinyStore server and its clients exchange over
// any byte stream, as docs/wire.md lays them out: frames, the MessagePack
// profile their bodies follow, the messages and the error codes. It imports
// only the standard library and knows no engine's Go type, so a client links
// it without linking SQLite.
//
//	frame.go     a frame's twelve-byte header, its rules, and a Reader of frames
//	msgpack.go   the profile: Append functions that write canonically, a Decoder that checks
//	message.go   HELLO, WELCOME and GOAWAY
//	serve.go     SERVE, and the proof a WELCOME answers a HELLO's challenge with
//	errors.go    the codes, and Error, a stream's failure
package wire
