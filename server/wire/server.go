package wire

// The server's own methods, in the range below the engines'.
const (
	// ServerStop asks a server to stop as its closing does: the streams
	// running finish, every connection is told GOAWAY, and a sidecar gives
	// back SERVE and its directory. An admin's alone, {} each way.
	ServerStop Method = 0x0001
)
