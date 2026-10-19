import { urlOf } from '$lib/content/nav'
import { loadSite } from '$lib/content/site'
import type { NavGroup } from '$lib/nav'

import type { LayoutServerLoad } from './$types'

export const load: LayoutServerLoad = async () => {
	const site = await loadSite()

	const nav: NavGroup[] = site.nav.map(section => ({
		book: section.book,
		title: section.title,
		items: section.items.map(item => {
			switch (item.kind) {
				case 'page':
					return { title: item.title, href: urlOf(item.slug) }
				case 'link':
					return { title: item.title, href: item.href, external: true }
				default:
					return { title: item.title }
			}
		}),
	}))

	// the header's Reference tab: the section's first page, or the section in the index
	// each part's tab leads to its first page
	const books = [...new Set(nav.map(group => group.book))].map(book => ({
		title: book,
		href:
			nav
				.filter(group => group.book === book)
				.flatMap(group => group.items)
				.find(item => item.href !== undefined && item.external !== true)?.href ?? '/docs',
	}))

	return { nav, books, origin: site.origin }
}
