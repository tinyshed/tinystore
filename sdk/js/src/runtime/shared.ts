// What Bun's runtime and Node's do alike: an endpoint read into an address, a
// sidecar started through node:child_process, which both implement, and the
// last lines of a child's stderr.

import { spawn } from 'node:child_process'
import { readdirSync } from 'node:fs'

import { ClosedError, InUseError, type TinystoreError } from '../errors.ts'
import type { Child } from '../runtime.ts'

/**
 * The error of an open in this process that failed, which the core gives as a
 * message alone: in use when another process holds the directory, as a
 * sidecar does after its last client closed, closed otherwise.
 */
export function openFailure(dir: string, message: string): TinystoreError {
	const failure = message.includes('in use') ? InUseError : ClosedError
	return new failure(`cannot open ${dir}: ${message}`)
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
 * parent's group.
 */
export function spawnDetached(argv: string[]): Child {
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
