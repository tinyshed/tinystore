// The runtime of Bun: sockets through Bun.connect, a Windows named pipe
// through node:net, which Bun implements, a private child through Bun.spawn,
// and a detached one through node:child_process, since Bun.spawn starts no
// process that outlives a Ctrl-C of its parent's group.

import { spawn } from 'node:child_process'
import net from 'node:net'

import manifest from '../../package.json' with { type: 'json' }
import { ClosedError } from '../errors.ts'
import type {
	Child,
	PrivateChild,
	Runtime,
	TlsOptions,
	Transport,
	TransportEvents,
} from '../runtime.ts'

export const bunRuntime: Runtime = {
	client: `tinystore-js/${manifest.version} bun/${Bun.version}`,
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

/** Where an endpoint named in SERVE, or given to connect, is. */
export type Address =
	| { kind: 'unix'; path: string }
	| { kind: 'pipe'; path: string }
	| { kind: 'tcp' | 'tls'; host: string; port: number }

export function address(endpoint: string): Address {
	if (endpoint.startsWith('unix://')) {
		return { kind: 'unix', path: endpoint.slice('unix://'.length) }
	}
	if (endpoint.startsWith('pipe:')) {
		return { kind: 'pipe', path: `\\\\.\\pipe\\${endpoint.slice('pipe:'.length)}` }
	}
	const remote = /^(tcp|tls):\/\/(\[[^\]]+\]|[^:/]+):(\d+)$/.exec(endpoint)
	if (remote === null) {
		throw new ClosedError(`no transport for the endpoint ${endpoint}`)
	}
	const host = remote[2]!.replace(/^\[|\]$/g, '')
	return { kind: remote[1] as 'tcp' | 'tls', host, port: Number(remote[3]) }
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

/** How much of a child's stderr is kept, its last lines, to say why it ended. */
const stderrKept = 16 << 10

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
			const code = await proc.exited
			events.end(
				new ClosedError(
					`the private server exited with ${code}${kept === '' ? '' : `: ${lastLines(kept)}`}`,
				),
			)
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

function spawnDetached(argv: string[]): Child {
	const child = spawn(argv[0]!, argv.slice(1), {
		detached: true,
		stdio: 'ignore',
		windowsHide: true,
	})
	const exited = new Promise<number>(resolve => {
		child.once('exit', code => resolve(code ?? -1))
		child.once('error', () => resolve(-1))
	})
	child.unref()
	return { exited, kill: () => child.kill() }
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

export function lastLines(text: string, n = 5): string {
	return text.trimEnd().split('\n').slice(-n).join('\n')
}
