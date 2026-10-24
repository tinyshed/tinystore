// What the SDK needs of the runtime it runs in: a byte stream to a server, a
// child process, a file, a random byte. Everything else is plain TypeScript
// over Uint8Array, so a runtime other than Bun is one file implementing this.

/** What a transport tells the session reading it. */
export interface TransportEvents {
	data(bytes: Uint8Array): void
	/** the stream ended, cleanly or not; called once */
	end(err: Error): void
}

/** A byte stream to a server: a socket, a pipe, or a child's stdin and stdout. */
export interface Transport {
	/** Writes bytes whole and in order; what the stream does not take yet waits for it. */
	write(bytes: Uint8Array): void
	/** Ends the stream: a socket's close, or a child's stdin, which is its end. */
	close(): void
}

export interface TlsOptions {
	/** the certificates to trust besides the system's, PEM */
	ca?: string | string[]
	/** the name the certificate must carry, the endpoint's host when absent */
	serverName?: string
}

/** A process the SDK started. */
export interface Child {
	/** its exit code once it has exited */
	readonly exited: Promise<number>
	kill(): void
}

/** A private server: a child whose stdin and stdout are the connection. */
export interface PrivateChild extends Child {
	readonly transport: Transport
	/** the last lines it wrote to stderr, which say why it ended */
	stderr(): string
}

/** A sidecar this process started, which outlives it, while it starts. */
export interface Detached {
	/**
	 * Its exit code once it has exited and its stderr has ended, with the last
	 * of what it wrote there; it rejects with why it never started.
	 */
	readonly exited: Promise<{ code: number; stderr: string }>
	/** the last of what it wrote to stderr so far */
	stderr(): string
	/** Stops reading its stderr once its start is over: what it writes there later goes nowhere. */
	release(): void
}

export interface Runtime {
	/** a name and version for the server's logs: tinystore-js on bun/1.4.2 */
	readonly client: string
	readonly windows: boolean
	/**
	 * Connects to an endpoint: unix://path, pipe:name, tcp://host:port or
	 * tls://host:port. A signal that aborts first rejects with its reason and
	 * lets the socket go.
	 */
	connect(
		endpoint: string,
		events: TransportEvents,
		tls?: TlsOptions,
		signal?: AbortSignal,
	): Promise<Transport>
	/**
	 * Starts a private server whose stdin and stdout carry the frames, once it
	 * has started; it rejects with why it did not.
	 */
	spawnPrivate(argv: string[], events: TransportEvents): Promise<PrivateChild>
	/** Starts a process that outlives this one, its stderr read while it starts. */
	spawnDetached(argv: string[]): Detached
	/** A file's bytes, opened, read and closed at once; undefined when it is not there. */
	readFile(path: string): Promise<Uint8Array | undefined>
	/** The executable a name finds on PATH. */
	which(name: string): string | undefined
	/**
	 * A file this platform's package carries, `bin/tinystore` or the core's
	 * library in `lib/`, installed beside this one; undefined without it.
	 */
	packaged(file: string): string | undefined
}
