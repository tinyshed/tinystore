import { describe, expect, test } from 'bun:test'
import { toHtml } from 'hast-util-to-html'

import { createHighlight } from './highlight'

const highlight = await createHighlight()

// each piece of a line with the dark theme's colour it was given
function colours(code: string): [string, string][] {
	const html = toHtml(highlight(code, 'log'))
	return [...html.matchAll(/--shiki-dark:(#[0-9a-f]+)[^>]*>([^<]*)</gi)].map(([, colour, text]) => [
		(text ?? '').replaceAll('&#x22;', '"').trim(),
		(colour ?? '').toLowerCase(),
	])
}

describe('a log fence', () => {
	test('colours a line as the logger colours it on a terminal', () => {
		const pieces = new Map(
			colours('11:02:15.911 ERROR api  payment failed  orderId=981 reason="card declined"'),
		)
		expect(pieces.get('11:02:15.911')).toBe('#7f7c8f')
		expect(pieces.get('ERROR')).toBe('#ff7b72')
		expect(pieces.get('api')).toBe('#39c5cf')
		expect(pieces.get('orderId=')).toBe('#7f7c8f')
		expect(pieces.get('reason=')).toBe('#7f7c8f')
	})

	test('gives each level the colour of its own', () => {
		const level = (line: string) => colours(line)[1]
		expect(level('09:08:47.058 INFO  app  code sent')).toEqual(['INFO', '#3fb950'])
		expect(level('09:08:47.058 WARN  app  slow')).toEqual(['WARN', '#d29922'])
		expect(level('09:08:47.058 DEBUG app  miss')).toEqual(['DEBUG', '#58a6ff'])
		expect(level('09:08:47.058 EVENT web  click')).toEqual(['EVENT', '#bc8cff'])
	})

	test('colours a line with its time in full or none, and no word of a message as a stream', () => {
		const streams = (line: string) =>
			colours(line).flatMap(([text, colour]) => (colour === '#39c5cf' ? [text] : []))
		const full = new Map(colours('2026-10-02 11:02:14.502 +03:00 WARN  api  slow request'))
		expect(full.get('2026-10-02 11:02:14.502 +03:00')).toBe('#7f7c8f')
		expect(full.get('api')).toBe('#39c5cf')

		const bare = colours('WARN  slow request  requestId=7f3a ms=1200')
		expect(bare[0]).toEqual(['WARN', '#d29922'])
		expect(new Map(bare).get('requestId=')).toBe('#7f7c8f')
		expect(streams('WARN  slow request  requestId=7f3a ms=1200')).toEqual([])
		expect(streams('11:02:14.502 INFO  n=1')).toEqual([])
		expect(streams('11:02:14.502 INFO  api')).toEqual(['api'])
	})
})
