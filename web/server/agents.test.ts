import { describe, expect, test } from 'bun:test'

import { agentOf } from './agents.ts'

describe('who asked', () => {
	test('a browser is a person', () => {
		expect(
			agentOf(
				'Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 Version/18.0 Mobile Safari/604.1',
			),
		).toBe('person')
	})

	test('a fetcher reading for a model is an agent', () => {
		for (const name of [
			'ClaudeBot/1.0',
			'Mozilla/5.0 (compatible; GPTBot/1.2)',
			'PerplexityBot/1.0',
			'Claude-User',
		]) {
			expect(agentOf(name)).toBe('agent')
		}
	})

	test('any other program is a crawler, and so is a request that says nothing', () => {
		for (const name of [
			'Googlebot/2.1',
			'curl/8.9.1',
			'python-requests/2.32',
			'Mozilla/5.0 HeadlessChrome/140.0',
		]) {
			expect(agentOf(name)).toBe('crawler')
		}
		expect(agentOf(null)).toBe('crawler')
		expect(agentOf('  ')).toBe('crawler')
	})
})
