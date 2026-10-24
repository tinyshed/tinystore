// The runtime of Bun: sockets through Bun.connect, a Windows named pipe
// through node:net, which Bun implements, and a private child through
// Bun.spawn. Nothing here touches Bun until it is called, so that Node can
// load this file beside its own.

import net from 'node:net'

import manifest from '../../package.json' with { type: 'json' }
import { ClosedError } from '../errors.ts'
import type { PrivateChild, Runtime, TlsOptions, Transport, TransportEvents } from '../runtime.ts'
import {
	address,
	connecting,
	exitedWith,
	platformPackage,
	spawnDetached,
	stderrKept,
} from './shared.ts'

export const bunRuntime: Runtime = {
	get client() {
		return `tinystore-js/${manifest.version} bun/${Bun.version}`
	},
	windows: process.platform === 'win32',
	connect,
	spawnPrivate,
	spawnDetached,
	readFile: async path => {
		const file = Bun.file(path)
		try {
			return await file.bytes()
		} catch (err) {
			if ((err as { code?: string }).code === 'ENOENT') {
				return undefined
			}
			throw err
		}
	},
	which: name => Bun.which(name) ?? undefined,
	packaged,
}

async function connect(
	endpoint: string,
	events: TransportEvents,
	tls?: TlsOptions,
	signal?: AbortSignal,
): Promise<Transport> {
	const at = address(endpoint)
	switch (at.kind) {
		case 'pipe':
			return connectPipe(at.path, events, signal)
		case 'unix': {
			const path = at.path
			return connectSocket(socket => Bun.connect({ unix: path, socket }), events, signal)
		}
		case 'tcp': {
			const { host, port } = at
			return connectSocket(socket => Bun.connect({ hostname: host, port, socket }), events, signal)
		}
		case 'tls': {
			const { host, port } = at
			const verify = {
				serverName: tls?.serverName ?? host,
				...(tls?.ca === undefined ? {} : { ca: tls.ca }),
			}
			return connectSocket(
				socket => Bun.connect({ hostname: host, port, tls: verify, socket }),
				events,
				signal,
			)
		}
	}
}

type Socket = Awaited<ReturnType<typeof Bun.connect>>

interface Handlers {
	data(socket: Socket, data: Uint8Array): void
	drain(socket: Socket): void
	end(socket: Socket): void
	close(socket: Socket, err?: Error): void
	error(socket: Socket, err: Error): void
}

/**
 * A socket's write may take part of the bytes: the rest waits for its drain,
 * in order, so the session hands a whole frame over and forgets it. Bun's
 * connect takes no signal: one that aborts first rejects at once, and the
 * socket connected after is ended.
 */
async function connectSocket(
	dial: (handlers: Handlers) => Promise<Socket>,
	events: TransportEvents,
	signal?: AbortSignal,
): Promise<Transport> {
	const waiting: Uint8Array[] = []
	let ended = false
	const end = (err: Error) => {
		if (!ended) {
			ended = true
			events.end(err)
		}
	}
	let socket: Socket | undefined
	const pump = () => {
		while (socket !== undefined && waiting.length > 0) {
			const bytes = waiting[0]!
			const wrote = socket.write(bytes)
			if (wrote === bytes.length) {
				waiting.shift()
				continue
			}
			waiting[0] = bytes.subarray(Math.max(wrote, 0))
			return
		}
	}
	const dialled = dial({
		data: (_, data) => events.data(data),
		drain: () => pump(),
		end: () => end(new ClosedError('the server ended the connection')),
		close: (_, err) => end(err ?? new ClosedError('the connection closed')),
		error: (_, err) => end(err),
	})
	socket = await connecting(dialled, signal, late => late.end())
	return {
		write: bytes => {
			waiting.push(bytes)
			if (waiting.length === 1) {
				pump()
			}
		},
		close: () => socket?.end(),
	}
}

function connectPipe(
	path: string,
	events: TransportEvents,
	signal?: AbortSignal,
): Promise<Transport> {
	return new Promise((resolve, reject) => {
		const socket = net.createConnection(path)
		let connected = false
		let ended = false
		const end = (err: Error) => {
			if (!ended) {
				ended = true
				events.end(err)
			}
		}
		const abort = () => {
			reject(signal?.reason)
			socket.destroy()
		}
		if (signal?.aborted) {
			abort()
			return
		}
		signal?.addEventListener('abort', abort, { once: true })
		socket.once('connect', () => {
			connected = true
			signal?.removeEventListener('abort', abort)
			resolve({ write: bytes => socket.write(bytes), close: () => socket.end() })
		})
		socket.on('data', (data: Uint8Array) => events.data(data))
		socket.on('error', err => {
			if (connected) {
				end(err)
				return
			}
			signal?.removeEventListener('abort', abort)
			reject(err)
		})
		socket.on('close', () => end(new ClosedError('the pipe closed')))
	})
}

// async, so that the throw of a Bun.spawn that cannot start is its rejection
async function spawnPrivate(argv: string[], events: TransportEvents): Promise<PrivateChild> {
	const proc = Bun.spawn(argv, { stdin: 'pipe', stdout: 'pipe', stderr: 'pipe', windowsHide: true })
	let kept = ''
	// a pipe nobody drains stops the server once it fills, so stderr is read to its end
	const draining = (async () => {
		const decoder = new TextDecoder()
		for await (const chunk of proc.stderr) {
			kept = (kept + decoder.decode(chunk, { stream: true })).slice(-stderrKept)
		}
	})()
	void (async () => {
		try {
			for await (const chunk of proc.stdout) {
				events.data(chunk)
			}
		} finally {
			await draining.catch(() => {})
			events.end(exitedWith(await proc.exited, kept))
		}
	})()
	return {
		transport: {
			write: bytes => {
				proc.stdin.write(bytes)
				void proc.stdin.flush()
			},
			close: () => {
				void proc.stdin.end()
			},
		},
		exited: proc.exited,
		kill: () => proc.kill(),
		stderr: () => kept,
	}
}

/** A file of the platform's package, installed beside this one. */
function packaged(file: string): string | undefined {
	try {
		return Bun.resolveSync(`${platformPackage()}/${file}`, import.meta.dir)
	} catch {
		return undefined
	}
}
