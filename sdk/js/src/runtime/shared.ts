// What Bun's runtime and Node's do alike: an endpoint read into an address, a
// sidecar started through node:child_process, which both implement, and the
// last lines of a child's stderr.

import { spawn } from 'node:child_process'

import { ClosedError } from '../errors.ts'
import type { Child } from '../runtime.ts'

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
