// The runtime of Node: every transport through node:net and node:tls, a
// Windows named pipe and a Unix socket included, and a private child through
// node:child_process.

import { spawn } from 'node:child_process'
import { accessSync, constants, statSync } from 'node:fs'
import { readFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import net from 'node:net'
import { delimiter, join } from 'node:path'
import tls from 'node:tls'

import manifest from '../../package.json' with { type: 'json' }
import { ClosedError } from '../errors.ts'
import type { PrivateChild, Runtime, TlsOptions, Transport, TransportEvents } from '../runtime.ts'
import { address, exitedWith, spawnDetached, stderrKept } from './shared.ts'

export const nodeRuntime: Runtime = {
	client: `tinystore-js/${manifest.version} node/${process.versions.node}`,
	windows: process.platform === 'win32',
	connect,
	spawnPrivate,
	spawnDetached,
	readFile: async path => {
		try {
			return await readFile(path)
		} catch (err) {
			if ((err as { code?: string }).code === 'ENOENT') {
				return undefined
			}
			throw err
		}
	},
	which,
	packagedBinary,
}

function connect(
	endpoint: string,
	events: TransportEvents,
	options?: TlsOptions,
): Promise<Transport> {
	const at = address(endpoint)
	switch (at.kind) {
		case 'pipe':
		case 'unix':
			return connected(net.createConnection(at.path), 'connect', events)
		case 'tcp':
			return connected(
				net.createConnection({ host: at.host, port: at.port, noDelay: true }),
				'connect',
				events,
			)
		case 'tls':
			return connected(connectTLS(at.host, at.port, options), 'secureConnect', events)
	}
}

/**
 * A TLS socket checking the certificate against the name asked for, or the
 * host, and trusting the given certificates beside Node's own: Node's ca
 * replaces them otherwise.
 */
function connectTLS(host: string, port: number, options?: TlsOptions): tls.TLSSocket {
	const name = options?.serverName ?? host
	const given = options?.ca === undefined ? [] : [options.ca].flat()
	const socket = tls.connect({
		host,
		port,
		// a server name is a DNS name: one that is an address is checked, not sent
		...(net.isIP(name) === 0 ? { servername: name } : {}),
		checkServerIdentity: (_, certificate) => tls.checkServerIdentity(name, certificate),
		...(given.length === 0 ? {} : { ca: [...tls.rootCertificates, ...given] }),
	})
	return socket.setNoDelay(true)
}

/**
 * A transport once the socket is ready: a socket's write queues what the
 * kernel does not take yet, in order, so the session hands a whole frame over
 * and forgets it.
 */
function connected(
	socket: net.Socket,
	ready: 'connect' | 'secureConnect',
	events: TransportEvents,
): Promise<Transport> {
	return new Promise((resolve, reject) => {
		let open = false
		let ended = false
		const end = (err: Error) => {
			if (!ended) {
				ended = true
				events.end(err)
			}
		}
		socket.once(ready, () => {
			open = true
			resolve({ write: bytes => void socket.write(bytes), close: () => void socket.end() })
		})
		socket.on('data', (data: Uint8Array) => events.data(data))
		socket.on('error', err => (open ? end(err) : reject(err)))
		socket.on('end', () => end(new ClosedError('the server ended the connection')))
		socket.on('close', () => end(new ClosedError('the connection closed')))
	})
}

function spawnPrivate(argv: string[], events: TransportEvents): PrivateChild {
	const proc = spawn(argv[0]!, argv.slice(1), { stdio: 'pipe', windowsHide: true })
	let kept = ''
	// a pipe nobody drains stops the server once it fills, so stderr is read to its end
	proc.stderr.setEncoding('utf8')
	proc.stderr.on('data', (text: string) => {
		kept = (kept + text).slice(-stderrKept)
	})
	proc.stdout.on('data', (data: Uint8Array) => events.data(data))
	// a write to a child that has exited fails here; its exit says why
	proc.stdin.on('error', () => {})
	// close comes once the child has exited and its stdout and stderr have ended
	const exited = new Promise<number>(resolve => {
		proc.once('close', code => resolve(code ?? -1))
		proc.once('error', err => {
			kept += err.message
			resolve(-1)
		})
	})
	void exited.then(code => events.end(exitedWith(code, kept)))
	return {
		transport: {
			write: bytes => void proc.stdin.write(bytes),
			close: () => void proc.stdin.end(),
		},
		exited,
		kill: () => void proc.kill(),
		stderr: () => kept,
	}
}

/** The executable a name finds on PATH, with an extension of PATHEXT on Windows. */
function which(name: string): string | undefined {
	const windows = process.platform === 'win32'
	const extensions = windows ? (process.env.PATHEXT ?? '.EXE;.CMD;.BAT;.COM').split(';') : ['']
	for (const dir of (process.env.PATH ?? '').split(delimiter)) {
		for (const extension of dir === '' ? [] : extensions) {
			const path = join(dir, name + extension)
			if (executable(path, windows)) {
				return path
			}
		}
	}
	return undefined
}

function executable(path: string, windows: boolean): boolean {
	try {
		if (!statSync(path).isFile()) {
			return false
		}
		if (!windows) {
			accessSync(path, constants.X_OK)
		}
		return true
	} catch {
		return false
	}
}

/** The platform's package of the server binary, installed beside this one. */
function packagedBinary(): string | undefined {
	const name = `@tinyshed/tinystore-${process.platform}-${process.arch}`
	const binary = process.platform === 'win32' ? 'tinystore.exe' : 'tinystore'
	try {
		return createRequire(import.meta.url).resolve(`${name}/bin/${binary}`)
	} catch {
		return undefined
	}
}
