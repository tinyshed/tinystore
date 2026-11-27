import { describe, expect, test } from 'bun:test'

import { docOf } from './rustdoc'

describe("a Rust doc comment as the site's markdown", () => {
	test('shows an intra-doc link as its code, one with a path as its text, and keeps a link to a page', () => {
		expect(
			docOf(
				'Opens a [`Bucket`]; see [`Store::open`](crate::Store::open), [its keys](super::Keys), [`sql!`](crate::sql!) and [kv](../kv.md).',
			),
		).toBe('Opens a `Bucket`; see `Store::open`, its keys, `sql!` and [kv](../kv.md).')
	})

	test('marks a fence without a language as Rust and leaves the hidden lines out', () => {
		const doc = ['```', '# use tinystore::Store;', 'let store = Store::open("data")?;', '```'].join(
			'\n',
		)
		expect(docOf(doc)).toBe(['```rust', 'let store = Store::open("data")?;', '```'].join('\n'))
	})

	test("leaves another language's fence and its lines as they are", () => {
		const doc = ['```text', '# not hidden', '[`Bucket`]', '```', 'then [`Bucket`]'].join('\n')
		expect(docOf(doc)).toBe(
			['```text', '# not hidden', '[`Bucket`]', '```', 'then `Bucket`'].join('\n'),
		)
	})
})
