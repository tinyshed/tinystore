// What Bun's runtime and Node's do alike: the loop's turns between the answers
// of the core in this process, an endpoint read into an address, a sidecar
// started through node:child_process, which both implement, and the last
// lines of a child's stderr.

import { type ChildProcess, spawn } from 'node:child_process'
import { readdirSync } from 'node:fs'

import { ClosedError, InUseError, type TinystoreError } from '../errors.ts'
import type { Detached } from '../runtime.ts'

/**
 * The error of an open in this process that failed, which the core gives as a
 * message alone: in use when another process holds the directory, as a
 * sidecar does after its last client closed, closed otherwise.
 */
export function openFailure(dir: string, message: string): TinystoreError {
	const failure = message.includes('in use') ? InUseError : ClosedError
	return new failure(`cannot open ${dir}: ${message}`)
}

/** The writes in a row an event loop's one turn sends the core in this process. */
const sendsATurn = 64

/**
 * Lets the event loop turn between answers the core in this process gives
 * inside the write that asked. Each such answer resolves its promise in a
 * microtask, so a loop of awaited reads would never let a timer or a socket
 * run (research release-2026-10-23); every `sendsATurn` writes in one turn,
 * the next waits for `setImmediate`, and those after it wait behind it.
 */
export function turning(send: (bytes: Uint8Array) => void): (bytes: Uint8Array) => void {
	let sent = 0
	let held: Uint8Array[] | undefined
	const turned = (): void => {
		sent = 0
		const sending = held ?? []
		held = undefined
		for (const bytes of sending) {
			write(bytes)
		}
	}
	const write = (bytes: Uint8Array): void => {
		if (held !== undefined) {
			held.push(bytes)
			return
		}
		if (sent === 0) {
			setImmediate(turned)
		}
		sent++
		if (sent > sendsATurn) {
			held = [bytes]
			return
		}
		send(bytes)
	}
	return write
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

/**
 * Starts a sidecar that outlives this process, through node:child_process in
 * Bun too, since Bun.spawn starts no process that outlives a Ctrl-C of its
 * parent's group. Its stderr is read while it starts, since a start that
 * fails before its log opens says why there alone, and let go after: a pipe
 * still read would keep this process's event loop alive.
 */
export function spawnDetached(argv: string[]): Detached {
	let child: ChildProcess
	try {
		child = spawn(argv[0]!, argv.slice(1), {
			detached: true,
			stdio: ['ignore', 'ignore', 'pipe'],
			windowsHide: true,
		})
	} catch (err) {
		const exited = Promise.reject(err)
		exited.catch(() => {})
		return { exited, stderr: () => '', release: () => {} }
	}
	const stderr = child.stderr!
	let kept = ''
	stderr.setEncoding('utf8')
	stderr.on('data', (text: string) => {
		kept = (kept + text).slice(-stderrKept)
	})
	stderr.on('error', () => {})
	const exited = new Promise<{ code: number; stderr: string }>((resolve, reject) => {
		child.once('error', reject)
		// close comes once the child has exited and its stderr has ended
		child.once('close', code => resolve({ code: code ?? -1, stderr: kept }))
	})
	exited.catch(() => {})
	child.unref()
	return { exited, stderr: () => kept, release: () => stderr.destroy() }
}

/** Why a process did not start, in the words every SDK uses: not found, permission denied, or the system's own. */
export function notStarted(err: unknown): string {
	const code = (err as { code?: unknown } | undefined)?.code
	if (code === 'ENOENT') {
		return 'not found'
	}
	if (code === 'EACCES' || code === 'EPERM') {
		return 'permission denied'
	}
	return err instanceof Error ? err.message : String(err)
}

/** How much of a child's stderr is kept, its last lines, to say why it ended. */
export const stderrKept = 16 << 10

export function lastLines(text: string, n = 5): string {
	return text.trimEnd().split('\n').slice(-n).join('\n')
}

export function exitedWith(code: number, stderr: string): ClosedError {
	return new ClosedError(
		`the private server exited with ${code}${stderr === '' ? '' : `: ${lastLines(stderr)}`}`,
	)
}

/**
 * The package that carries this platform's executable and libraries:
 * `@tinyshed/tinystore-linux-x64`, or `-musl` after it where Linux's C
 * library is musl, as npm chose it by its `libc` at install.
 */
export function platformPackage(): string {
	const musl = process.platform === 'linux' && runsOnMusl()
	return `@tinyshed/tinystore-${process.platform}-${process.arch}${musl ? '-musl' : ''}`
}

/** Whether this Linux runs musl: its loader, `ld-musl-<arch>.so.1`, is in /lib. */
function runsOnMusl(): boolean {
	try {
		return readdirSync('/lib').some(name => name.startsWith('ld-musl-'))
	} catch {
		return false
	}
}
