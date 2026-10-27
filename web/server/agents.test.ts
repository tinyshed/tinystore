import { describe, expect, test } from 'bun:test'

import { agentOf, providerOf } from './agents.ts'

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

	test("an agent is its provider's, by the name it gives itself", () => {
		expect(providerOf('Claude-User (claude-code/2.1)')).toBe('anthropic')
		expect(providerOf('Mozilla/5.0 AppleWebKit/537.36; compatible; ChatGPT-User/1.0')).toBe(
			'openai',
		)
		expect(providerOf('Mozilla/5.0 (compatible; PerplexityBot/1.0)')).toBe('perplexity')
		expect(providerOf('CCBot/2.0 (https://commoncrawl.org/faq/)')).toBe('common-crawl')

		expect(providerOf('Googlebot/2.1')).toBeUndefined()
		expect(providerOf('Mozilla/5.0 Safari/604.1')).toBeUndefined()
		expect(providerOf(null)).toBeUndefined()
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
