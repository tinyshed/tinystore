// What a connection tells its program of the sidecar it found.

import { expect, test } from 'bun:test'

import { olderSidecar } from '../src/connection.ts'

test('a sidecar of an older release than its SDK is told of, and only one', () => {
	const cases: [server: string, own: string, older: boolean][] = [
		['v0.1.0', '0.2.0', true],
		['v0.2.0-rc.1', '0.2.0', true],
		['v0.2.0-rc.2', '0.2.0-rc.10', true],
		['v0.2.0-beta.3', '0.2.0-rc.1', true],
		['v0.2.0', '0.2.0', false],
		['v0.3.0', '0.2.0', false],
		['v0.2.0', '0.2.0-rc.1', false],
		['(devel)', '0.2.0', false],
		['v0.0.0-20261002222701-fc080a056d05+dirty', '0.2.0', false],
		['v0.1.1-0.20261002222701-fc080a056d05', '0.2.0', false],
		['v0.1.0', '0.0.0', false],
	]
	for (const [server, own, older] of cases) {
		const told = olderSidecar(server, own, './data')
		expect(told !== undefined, `${server} against ${own}`).toBe(older)
		if (told !== undefined) {
			expect(told).toContain(`is ${server}, older than this SDK's ${own}`)
			expect(told).toContain('tinystore stop ./data')
		}
	}
})
