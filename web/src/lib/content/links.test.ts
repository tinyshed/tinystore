import { describe, expect, test } from 'bun:test'

import { type Links, resolveLink } from './links'
import { checkout } from './repo'

function links(): Links {
	return {
		root: checkout(),
		commit: 'abc123',
		pages: new Map([
			['docs/kv.md', { url: '/docs/kv', ids: new Set(['expiry']) }],
			['docs/store/metrics.md', { url: '/docs/store/metrics', ids: new Set(['count-something']) }],
		]),
		assets: new Set(),
	}
}

const here = new Set(['top'])
const from = 'docs/store/metrics.md'

describe('a link written for GitHub', () => {
	test('to a page stays on the site, its heading checked', () => {
		expect(resolveLink(links(), from, '../kv.md#expiry', here, 'link')).toEqual({
			href: '/docs/kv#expiry',
		})
		expect(resolveLink(links(), from, '../kv.md#nowhere', here, 'link')).toEqual({
			problem: 'docs/kv.md has no heading #nowhere',
		})
	})

	test('to another file of the repository opens on GitHub at the built commit', () => {
		expect(resolveLink(links(), from, '../../metrics/README.md#public-use', here, 'link')).toEqual({
			href: 'https://github.com/tinyshed/tinystore/blob/abc123/metrics/README.md#public-use',
		})
		expect(resolveLink(links(), from, '../../metrics', here, 'link')).toEqual({
			href: 'https://github.com/tinyshed/tinystore/tree/abc123/metrics',
		})
	})

	test('to an image is served by the site', () => {
		const known = links()
		expect(resolveLink(known, 'README.md', '.github/assets/logo-dark.svg', here, 'asset')).toEqual({
			href: '/files/.github/assets/logo-dark.svg',
		})
		expect([...known.assets]).toEqual(['.github/assets/logo-dark.svg'])
	})

	test('within the page is checked against its headings', () => {
		expect(resolveLink(links(), from, '#top', here, 'link')).toEqual({ href: '#top' })
		expect(resolveLink(links(), from, '#bottom', here, 'link')).toEqual({
			problem: 'no heading #bottom on this page',
		})
	})

	test('elsewhere is left as it is', () => {
		for (const url of ['https://example.com/a', 'mailto:a@example.com', '//cdn.example.com/x']) {
			expect(resolveLink(links(), from, url, here, 'link')).toEqual({ href: url })
		}
	})

	test('that leads nowhere is a problem', () => {
		expect(resolveLink(links(), from, 'missing.md', here, 'link')).toEqual({
			problem: 'docs/store/missing.md does not exist',
		})
		expect(resolveLink(links(), from, '../../../outside.md', here, 'link')).toEqual({
			problem: '../../../outside.md climbs out of the repository',
		})
	})
})
