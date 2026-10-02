// The runtime of Bun: sockets through Bun.connect, a Windows named pipe
// through node:net, which Bun implements, and a private child through
// Bun.spawn. Nothing here touches Bun until it is called, so that Node can
// load this file beside its own.

import net from 'node:net'

import manifest from '../../package.json' with { type: 'json' }
import { ClosedError } from '../errors.ts'
import type { PrivateChild, Runtime, TlsOptions, Transport, TransportEvents } from '../runtime.ts'
import { address, exitedWith, spawnDetached, stderrKept } from './shared.ts'

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
	packagedBinary,
}

async function connect(
	endpoint: string,
	events: TransportEvents,
	tls?: TlsOptions,
): Promise<Transport> {
	const at = address(endpoint)
	switch (at.kind) {
		case 'pipe':
			return connectPipe(at.path, events)
		case 'unix': {
			const path = at.path
			return connectSocket(socket => Bun.connect({ unix: path, socket }), events)
		}
		case 'tcp': {
			const { host, port } = at
			return connectSocket(socket => Bun.connect({ hostname: host, port, socket }), events)
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
 * in order, so the session hands a whole frame over and forgets it.
 */
async function connectSocket(
	dial: (handlers: Handlers) => Promise<Socket>,
	events: TransportEvents,
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
	socket = await dial({
		data: (_, data) => events.data(data),
		drain: () => pump(),
		end: () => end(new ClosedError('the server ended the connection')),
		close: (_, err) => end(err ?? new ClosedError('the connection closed')),
		error: (_, err) => end(err),
	})
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

function connectPipe(path: string, events: TransportEvents): Promise<Transport> {
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
		socket.once('connect', () => {
			connected = true
			resolve({ write: bytes => socket.write(bytes), close: () => socket.end() })
		})
		socket.on('data', (data: Uint8Array) => events.data(data))
		socket.on('error', err => (connected ? end(err) : reject(err)))
		socket.on('close', () => end(new ClosedError('the pipe closed')))
	})
}

function spawnPrivate(argv: string[], events: TransportEvents): PrivateChild {
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

/** The platform's package of the server binary, installed beside this one. */
function packagedBinary(): string | undefined {
	const name = `@tinyshed/tinystore-${process.platform}-${process.arch}`
	const binary = process.platform === 'win32' ? 'tinystore.exe' : 'tinystore'
	try {
		return Bun.resolveSync(`${name}/bin/${binary}`, import.meta.dir)
	} catch {
		return undefined
	}
}
