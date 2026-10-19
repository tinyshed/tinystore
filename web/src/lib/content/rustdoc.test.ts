import { describe, expect, test } from 'bun:test'

import { docOf } from './rustdoc'

describe("a Rust doc comment as the site's markdown", () => {
	test('shows an intra-doc link as its code and keeps a link with a target', () => {
		expect(docOf('Opens a [`Bucket`]; see [`Store::open`](crate::Store::open).')).toBe(
			'Opens a `Bucket`; see [`Store::open`](crate::Store::open).',
		)
	})

	test('marks a fence without a language as Rust and leaves the hidden lines out', () => {
		const doc = ['```', '# use tinystore::Store;', 'let store = Store::open("data")?;', '```'].join('\n')
		expect(docOf(doc)).toBe(['```rust', 'let store = Store::open("data")?;', '```'].join('\n'))
	})

	test("leaves another language's fence and its lines as they are", () => {
		const doc = ['```text', '# not hidden', '[`Bucket`]', '```', 'then [`Bucket`]'].join('\n')
		expect(docOf(doc)).toBe(['```text', '# not hidden', '[`Bucket`]', '```', 'then `Bucket`'].join('\n'))
	})
})
