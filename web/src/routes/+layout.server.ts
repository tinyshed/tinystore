import { urlOf } from '$lib/content/nav'
import { loadSite } from '$lib/content/site'
import type { NavGroup } from '$lib/nav'

import type { LayoutServerLoad } from './$types'

export const load: LayoutServerLoad = async () => {
	const site = await loadSite()

	const nav: NavGroup[] = site.nav.map(section => ({
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
	const reference =
		nav
			.find(group => group.title.toLowerCase() === 'reference')
			?.items.find(item => item.href !== undefined && item.external !== true)?.href ??
		'/docs#reference'

	return { nav, reference, origin: site.origin }
}
