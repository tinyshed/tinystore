import type { Bucket } from '@tinyshed/tinystore'

/**
 * A reader's id for one day, as Plausible counts them: a hash of the address
 * and the User-Agent under a salt made each day. The salt is kept a day and a
 * half and then expires, so that no id links two days together and no address
 * is ever stored; kept in kv, a restart the same day does not count everyone
 * twice.
 */
export class Visitors {
	#day = ''
	#salt: Promise<string> | undefined

	constructor(
		private readonly salts: Bucket<string>,
		private readonly now: () => Date = () => new Date(),
	) {}

	async of(address: string, userAgent: string): Promise<string> {
		const day = this.now().toISOString().slice(0, 10)
		if (day !== this.#day || this.#salt === undefined) {
			this.#day = day
			this.#salt = this.saltOf(day)
		}

		let salt: string
		try {
			salt = await this.#salt
		} catch (error) {
			// asked again on the next view, not refused for the rest of the day
			this.#salt = undefined
			throw error
		}

		return new Bun.CryptoHasher('sha256')
			.update(`${salt}\n${address}\n${userAgent}`)
			.digest('hex')
			.slice(0, 16)
	}

	// one write, which keeps the salt already there: two servers starting the same day agree
	private async saltOf(day: string): Promise<string> {
		const { entry } = await this.salts.setEntryIfAbsent(`salt/${day}`, crypto.randomUUID(), {
			ttl: '36h',
		})
		return entry.value
	}
}
