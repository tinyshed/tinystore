package wire

// The server's own methods, in the range below the engines'.
const (
	// ServerStop asks a server to stop as its closing does: the streams
	// running finish, every connection is told GOAWAY, and a sidecar gives
	// back SERVE and its directory. An admin's alone, {} each way.
	ServerStop Method = 0x0001

	// ServerClock moves the clock a private server started for a test runs
	// on, or reads it: a Clock each way. An admin's alone.
	ServerClock Method = 0x0002
)

// Clock is server.clock's request, a time to set the clock to or a while to
// move it forward by, neither to read it, and its answer, the time the clock
// reads once moved.
type Clock struct {
	At      int64 // unix milliseconds
	Advance int64 // milliseconds
}

func (c Clock) Append(dst []byte) []byte {
	m := BeginMap(dst)
	optionalInt(&m, 1, c.At)
	optionalInt(&m, 2, c.Advance)
	return m.End()
}

func (c *Clock) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			c.At = d.Int()
		case 2:
			c.Advance = d.Duration()
		}
	}
	return d.End()
}

// ServerBackup is a download of the whole store as one zip, as the backup
// package writes it: {} to ask, then the zip's bytes in DATA. An admin's alone.
const ServerBackup Method = 0x0003
