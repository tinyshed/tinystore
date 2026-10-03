// A test's clock: the time a private server opened with `clock` runs on, which
// a test moves forward rather than waits for.

import type { Link } from './connection.ts'
import { type Duration, ms, type Time, unixMs } from './time.ts'
import { Clock as ClockMessage, methods } from './wire/messages.ts'

/**
 * The clock of a store opened with `{ private: true, clock }`. Moving it
 * forward expires keys, makes jobs due and ages records at once, as Go's
 * tinystore.Options.Clock does; a store on the system's time refuses it with
 * InvalidError.
 *
 *     await using store = await open(dir, { private: true, clock: new Date('2026-10-03T09:00:00Z') })
 *     await store.clock.advance('1h')
 */
export class Clock {
	readonly #link: Link

	constructor(link: Link) {
		this.#link = link
	}

	/** The time the server's clock reads. */
	now(): Promise<Date> {
		return this.#move({})
	}

	/** Moves the clock forward by a while, and gives the time it then reads. */
	advance(by: Duration): Promise<Date> {
		return this.#move({ advance: ms(by) })
	}

	/** Sets the clock to a time, which may not be before its own, and gives it. */
	set(to: Time): Promise<Date> {
		return this.#move({ at: unixMs(to) })
	}

	async #move(fields: Parameters<typeof ClockMessage.encode>[0]): Promise<Date> {
		const body = await this.#link.run('write', connection =>
			connection.session.call(methods['server.clock'], ClockMessage.encode(fields)),
		)
		return new Date(Number(ClockMessage.decode(body).at ?? 0))
	}
}
