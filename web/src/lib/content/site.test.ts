import { describe, expect, test } from 'bun:test'

import { llmsFull, llmsIndex, sitemap } from './llms'
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
		const metrics = site.pages.find(page => page.slug === 'store/metrics')

		expect(metrics?.markdown).toStartWith('# Metrics')
		expect(metrics?.markdown).not.toContain('](../')
		expect(llmsIndex(site)).toContain(`(${site.origin}/docs/store/metrics.md)`)
		expect(llmsFull(site)).toContain(`Source: ${site.origin}/docs/store/metrics`)
		expect(sitemap(site)).toContain(`<loc>${site.origin}/docs/store/metrics</loc>`)
	})
})
