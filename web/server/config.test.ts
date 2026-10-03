import { describe, expect, test } from 'bun:test'

import { createConfig } from './config.ts'

describe('config', () => {
	test('applies defaults and coerces strings', () => {
		const config = createConfig({ PORT: '8080', TRUST_PROXY: 'true' })

		expect(config.PORT).toBe(8080)
		expect(config.TRUST_PROXY).toBe(true)
		expect(config.HOST).toBe('127.0.0.1')
		expect(config.SITE_DIR).toBe('build')
		expect(config.DATA_DIR).toBe('data')
	})

	test('names the variables it rejected', () => {
		expect(() => createConfig({ PORT: 'eighty' })).toThrow(/PORT/)
	})
})
