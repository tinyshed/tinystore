import { describe, expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'

import { llmsFull, llmsIndex, sitemap } from './llms'
import { checkout } from './repo'
import { loadSite } from './site'

describe('the docs as a site', () => {
	test('build from docs/README.md: the index first, then every page it links, each with a title', async () => {
		const site = await loadSite()

		expect(site.pages[0]?.url).toBe('/docs')
		expect(site.pages.length).toBeGreaterThan(1)
		for (const page of site.pages) {
			expect(page.title).not.toBe('')
			expect(page.url).toStartWith('/docs')
			expect(page.html).not.toContain('<h1')
		}
	})

	test('give an agent markdown whose links still lead somewhere', async () => {
		const site = await loadSite()
		const why = site.pages.find(page => page.slug === 'handbook/start/why')

		expect(why?.markdown).toStartWith('# Why TinyStore')
		expect(why?.markdown).not.toContain('](../')
		expect(llmsIndex(site)).toContain(`(${site.origin}/docs/handbook/start/why.md)`)
		expect(llmsFull(site)).toContain(`Source: ${site.origin}/docs/handbook/start/why`)
		expect(sitemap(site)).toContain(`<loc>${site.origin}/docs/handbook/start/why</loc>`)
	})

	test('pin the newest release wherever a page names a version, so that no page asks for an old one', async () => {
		const site = await loadSite()
		const releases = readFileSync(join(checkout(), 'docs/releases/README.md'), 'utf8')
		const newest = releases.match(/\]\(v(\d+\.\d+)\.\d+\.md\)/)?.[1]
		const pins = [
			/tinystore(?:-cli)?\s*=\s*\{[^}\n]*version\s*=\s*"(\d+\.\d+)/g, // tinystore = { version = "0.7" }
			/tinystore(?:-cli)?\s*=\s*"(\d+\.\d+)/g,
			/(?:@tinyshed\/tinystore|tinystore(?:-cli)?)@v?(\d+\.\d+)/g, // bun add, cargo add, go get
			/tinyshed-tinystore==(\d+\.\d+)/g,
			/ghcr\.io\/tinyshed\/tinystore:(\d+\.\d+)/g,
			/served by tinystore (\d+\.\d+)/g, // what tinystore status prints
			/\*\*Version (\d+\.\d+)\*\*/g,
		]
		const old: string[] = []
		// a release's page and a benchmark name the version they were, or measured
		const current = (file: string) =>
			!file.startsWith('docs/releases/') && file !== 'docs/handbook/production/benchmarks.md'
		for (const page of site.pages.filter(page => current(page.file))) {
			const text = readFileSync(join(checkout(), page.file), 'utf8')
			for (const pin of pins) {
				for (const [found, version] of text.matchAll(pin)) {
					if (version !== newest) old.push(`${page.file}: ${found}`)
				}
			}
		}

		expect(newest).toBeDefined()
		expect(old).toEqual([])
	})
})
