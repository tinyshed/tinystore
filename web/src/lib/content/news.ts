import { posix } from 'node:path'
import type { Element, Root as HastRoot } from 'hast'
import { visit } from 'unist-util-visit'

/** A page as the release reads it: its file, its address, its title and its HTML still to finish. */
export interface Drafted {
	file: string
	url: string
	title: string
	hast: HastRoot
	/** the release that brought the page itself, when the release links it whole */
	newIn?: NewIn
}

export interface NewIn {
	version: string
	/** the release's page and section on the site */
	url: string
}

/** What a release links: a guide's file, the heading it names, and the release's section it is in. */
interface Brought {
	file: string
	heading?: string
	section?: string
}

/**
 * Marks what the newest release brought where the guides describe it. Each
 * link of its page to another page names a feature: a heading of it gets a
 * badge, "New in 0.2.0", that leads back to the release's section; a page
 * linked whole gets `newIn`, which its title shows.
 */
export function markNew(pages: Drafted[], release: Drafted): void {
	const version = release.title.replace(/^TinyStore\s+/, '')
	for (const brought of broughtBy(release)) {
		const page = pages.find(candidate => candidate.file === brought.file)
		if (page === undefined || page === release) {
			continue
		}
		const section = brought.section === undefined ? '' : `#${brought.section}`
		if (brought.heading === undefined) {
			page.newIn ??= { version, url: `${release.url}${section}` }
			continue
		}
		// a link of the repository's markdown, which the page's finish checks as any other
		const href = `${posix.relative(posix.dirname(page.file), release.file)}${section}`
		const heading = headingOf(page.hast, brought.heading)
		if (heading !== undefined && !heading.children.some(isBadge)) {
			heading.children.push(badge(version, href))
		}
	}
}

/** The pages a release's page links, each with the section of the release its link is in. */
function broughtBy(release: Drafted): Brought[] {
	const found: Brought[] = []
	let section: string | undefined
	for (const node of release.hast.children) {
		if (node.type !== 'element') {
			continue
		}
		if (node.tagName === 'h2') {
			section = typeof node.properties.id === 'string' ? node.properties.id : undefined
			continue
		}
		visit(node, 'element', link => {
			const href = link.tagName === 'a' ? link.properties.href : undefined
			if (typeof href !== 'string' || /^[a-z]+:|^#|^\//.test(href)) {
				return
			}
			const [path = '', heading] = href.split('#')
			if (!path.endsWith('.md')) {
				return
			}
			const file = posix.normalize(posix.join(posix.dirname(release.file), path))
			found.push({ file, ...(heading ? { heading } : {}), ...(section ? { section } : {}) })
		})
	}
	return found
}

function headingOf(tree: HastRoot, id: string): Element | undefined {
	let found: Element | undefined
	visit(tree, 'element', node => {
		if (found === undefined && /^h[2-4]$/.test(node.tagName) && node.properties.id === id) {
			found = node
		}
	})
	return found
}

function isBadge(node: Element['children'][number]): boolean {
	return (
		node.type === 'element' &&
		Array.isArray(node.properties.className) &&
		node.properties.className.includes('new-in')
	)
}

function badge(version: string, href: string): Element {
	return {
		type: 'element',
		tagName: 'a',
		properties: { className: ['new-in'], href, title: `What TinyStore ${version} brought` },
		children: [{ type: 'text', value: `New in ${version}` }],
	}
}
