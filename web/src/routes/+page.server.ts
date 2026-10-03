import { engineRows, heroSample, readCards } from '$lib/content/landing'
import { checkout } from '$lib/content/repo'
import { loadSite } from '$lib/content/site'

import type { PageServerLoad } from './$types'

export const load: PageServerLoad = async () => {
	const site = await loadSite()

	// "Start here" opens the index's first section, until a page of it is written: the index
	const starting = site.nav[0]?.items.find(item => item.kind === 'page')

	return {
		sample: await heroSample(),
		engines: engineRows(site),
		cards: readCards(checkout()),
		start: starting?.kind === 'page' ? `/docs/${starting.slug}` : '/docs',
	}
}
