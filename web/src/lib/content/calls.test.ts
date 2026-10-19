import { describe, expect, test } from 'bun:test'
import { toHtml } from 'hast-util-to-html'

import type { ApiLanguage } from './api'
import { type ApiIndex, findCalls, mask, pageCalls, readApiIndex, type Scope } from './calls'
import { createHighlight } from './highlight'
import { checkout } from './repo'

const index: ApiIndex = readApiIndex(checkout())

// each call the block resolves, as `name → Owner.member`
function calls(language: ApiLanguage, ...blocks: string[]): string[] {
	const scope: Scope = new Map()
	return blocks.flatMap(code => {
		const found = findCalls(code, language, index, scope)
		expect(found.problems).toEqual([])
		return found.marks.map(mark => `${code.slice(mark.start, mark.end)} → ${mark.entry.name}`)
	})
}

describe('a block of a guide', () => {
	test('in TypeScript follows a bucket from the store to its calls, across blocks', () => {
		expect(
			calls(
				'bun',
				"const sessions = store.bucket<Session>('sessions', { idle: '30d' })",
				'await sessions.under(user.id).set(digest(token), { user: user.id, device })\n' +
					'const session = await sessions.under(user.id).get(digest(token))',
			),
		).toEqual([
			'bucket → Store.bucket',
			'under → Bucket.under',
			'set → Bucket.set',
			'under → Bucket.under',
			'get → Bucket.get',
		])
	})

	test('in Python', () => {
		expect(
			calls(
				'python',
				'sessions = store.bucket("sessions", Session, idle="30d")\n' +
					'session = await sessions.under(user.id).get(digest(token))',
			),
		).toEqual(['bucket → Store.bucket', 'under → Bucket.under', 'get → Bucket.get'])
	})

	test("in Go, from a package's function", () => {
		expect(
			calls(
				'go',
				'sessions, err := kv.OpenBucket[Session](store, "sessions", kv.Idle(30*24*time.Hour))\n' +
					'session, found, err := sessions.Under(user.ID).Get(ctx, digest(token))',
			),
		).toEqual([
			'OpenBucket → OpenBucket',
			'Idle → Idle',
			'Under → Bucket.Under',
			'Get → Bucket.Get',
		])
	})

	test('in Rust, through a builder and its ?', () => {
		expect(
			calls(
				'rust',
				'let sessions = store.bucket::<Session>("sessions").idle(Duration::from_hours(24 * 30)).open()?;\n' +
					'let session: Option<Session> = sessions.under(user_id).get(&digest(&token))?;',
			),
		).toEqual([
			'bucket → Store.bucket',
			'idle → BucketBuilder.idle',
			'open → BucketBuilder.open',
			'under → Bucket.under',
			'get → Bucket.get',
		])
	})

	test('leaves a call on a variable of unknown type alone', () => {
		expect(calls('bun', 'await cache.get(key)')).toEqual([])
	})

	test('reports a method the reference does not list on a type it knows', () => {
		const found = findCalls("store.kv.bucket('sessions')", 'bun', index, new Map())
		expect(found.problems.map(problem => problem.message)).toEqual([
			'kv: Store has no kv in the bun API reference',
		])
	})
})

describe('the code a call is looked for in', () => {
	test('has its comments and strings blanked, its offsets kept', () => {
		const code = "store.get('a.b') // store.set(x)"
		const masked = mask(code, 'bun')
		expect(masked).toHaveLength(code.length)
		expect(masked).toBe("store.get('   ')                ")
	})
})

describe('a page', () => {
	test('links each call to its reference, with its signature for the popover', async () => {
		const { highlight } = pageCalls(index, 'docs/kv/README.md', await createHighlight())
		const html = toHtml(highlight("const sessions = store.bucket('sessions')", 'ts'))

		expect(html).toContain('<a class="api" href="../reference/bun/store.md#storebucket"')
		expect(html).toContain('data-sig="bucket<T = unknown>(name: string')
		expect(html).toContain('>bucket</span></a>')
	})
})
