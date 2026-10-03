// How background work says what failed: the same lines, at the same times, as
// Go's failureLog, whose TestBackgroundFailuresAreLoggedOncePerQuietPeriod
// runs the same sequence.

import { describe, expect, test } from 'bun:test'

import { DropNotice, FailureLog, type Say } from '../src/background.ts'
import type { Fields } from '../src/records.ts'

const minute = 60_000

function recorder(): { say: Say; said: string[] } {
	const said: string[] = []
	return {
		said,
		say: (level: string, message: string, fields: Fields) =>
			void said.push(`${level} ${message} ${JSON.stringify(fields)}`),
	}
}

describe('background work', () => {
	test('a repeated failure is said once a quiet period, and its recovery once', () => {
		const { say, said } = recorder()
		let now = Date.UTC(2026, 8, 24)
		const failures = new FailureLog('maintenance', say, () => now)
		for (let n = 0; n < 11; n++) {
			failures.failed(new Error('disk full'))
			now += minute
		}
		failures.succeeded()
		failures.succeeded()

		expect(said).toEqual([
			'warn background work failed {"work":"maintenance","error":"disk full","failures":1}',
			'warn background work failed {"work":"maintenance","error":"disk full","failures":11}',
			'info background work recovered {"work":"maintenance","failures":11}',
		])
	})

	test('a failure that changes is said at once', () => {
		const { say, said } = recorder()
		const failures = new FailureLog('records flush api', say, () => 0)
		failures.failed(new Error('disk full'))
		failures.failed(new Error('connection lost'))
		failures.failed(new Error('connection lost'))

		expect(said).toEqual([
			'warn background work failed {"work":"records flush api","error":"disk full","failures":1}',
			'warn background work failed {"work":"records flush api","error":"connection lost","failures":2}',
		])
	})

	test('dropped lines are said at a flush, once a quiet period, counted since the last time', () => {
		const { say, said } = recorder()
		let now = 0
		const drops = new DropNotice('api', 1024, say, () => now)
		drops.sayIfDue()
		for (let n = 0; n < 3; n++) {
			drops.dropped()
		}
		drops.sayIfDue()
		drops.dropped()
		now += 9 * minute
		drops.sayIfDue()
		now += minute
		drops.sayIfDue()
		drops.sayIfDue()

		expect(said).toEqual([
			'warn log lines dropped {"logger":"api","dropped":3,"buffer":1024}',
			'warn log lines dropped {"logger":"api","dropped":1,"buffer":1024}',
		])
	})
})
