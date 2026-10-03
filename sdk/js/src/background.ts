// What the SDK does in the background, an instrument's flush or a logger's
// write, has no caller to tell when it fails. It says so as Go's
// tinystore.Store does, in TinyStore's own lines: once, again when the
// failure changes or ten minutes on, and once more when the work recovers.

import type { Fields } from './records.ts'

/** A failure repeated within this long is counted rather than said again. */
const quiet = 10 * 60_000

/** Where TinyStore's own lines go: the console of a logger of stream tinystore, or a test's. */
export type Say = (level: 'info' | 'warn', message: string, fields: Fields) => void

/**
 * A background work's failures, said as Go's failureLog logs them:
 *
 *     00:00  disk full        → warn "background work failed" failures=1
 *     00:01…00:09 the same    → counted
 *     00:10  disk full        → warn failures=11
 *     00:11  success          → info "background work recovered" failures=11
 */
export class FailureLog {
	readonly #work: string
	readonly #say: Say
	readonly #now: () => number
	#last = ''
	#saidAt = 0
	#failures = 0

	constructor(work: string, say: Say, now: () => number = Date.now) {
		this.#work = work
		this.#say = say
		this.#now = now
	}

	failed(err: unknown): void {
		this.#failures++
		const error = messageOf(err)
		if (error === this.#last && this.#now() - this.#saidAt < quiet) {
			return
		}
		this.#say('warn', 'background work failed', {
			work: this.#work,
			error,
			failures: this.#failures,
		})
		this.#last = error
		this.#saidAt = this.#now()
	}

	succeeded(): void {
		if (this.#failures > 0) {
			this.#say('info', 'background work recovered', {
				work: this.#work,
				failures: this.#failures,
			})
		}
		this.#last = ''
		this.#failures = 0
	}
}

/**
 * The lines a logger dropped for want of room, said at a flush and at most
 * once in a quiet period: a burst is one line, and a logger that keeps
 * dropping says how many every ten minutes rather than every second.
 */
export class DropNotice {
	readonly #stream: string
	readonly #buffer: number
	readonly #say: Say
	readonly #now: () => number
	#unsaid = 0
	#saidAt: number | undefined

	constructor(stream: string, buffer: number, say: Say, now: () => number = Date.now) {
		this.#stream = stream
		this.#buffer = buffer
		this.#say = say
		this.#now = now
	}

	dropped(): void {
		this.#unsaid++
	}

	sayIfDue(): void {
		if (this.#unsaid === 0) {
			return
		}
		if (this.#saidAt !== undefined && this.#now() - this.#saidAt < quiet) {
			return
		}
		// logger, since a console line's own stream is tinystore's
		this.#say('warn', 'log lines dropped', {
			logger: this.#stream,
			dropped: this.#unsaid,
			buffer: this.#buffer,
		})
		this.#unsaid = 0
		this.#saidAt = this.#now()
	}
}

/** An error's message, as Go's err.Error() reads, without the class JavaScript puts first. */
export function messageOf(err: unknown): string {
	return err instanceof Error ? err.message : String(err)
}
