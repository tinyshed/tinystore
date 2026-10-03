import { exists, resolveFrom, sourceUrl } from './repo'

/** What a link may lead to on the site: a page, with the ids its headings have. */
export interface Target {
	url: string
	ids: Set<string>
}

export interface Links {
	root: string
	commit: string
	/** pages by the repository path of their file, `docs/store/metrics.md` */
	pages: Map<string, Target>
	/** files shown on a page, by repository path; the site serves each under /files/ */
	assets: Set<string>
}

export type Resolved = { href: string } | { problem: string }

/**
 * Where a link written for GitHub leads on the site. A page stays a page,
 * `kv.md#expiry` → `/docs/kv#expiry`, its heading checked; any other file of
 * the repository opens on GitHub at the commit the site was built from; an
 * image is served by the site; an address elsewhere is left as it is.
 * Anything that leads nowhere is a problem the build reports, since it is
 * broken on GitHub too.
 */
export function resolveLink(
	links: Links,
	from: string,
	url: string,
	ids: Set<string>,
	as: 'link' | 'asset',
): Resolved {
	if (url === '' || /^[a-z][a-z\d+.-]*:/i.test(url) || url.startsWith('//')) {
		return { href: url }
	}

	const [rest = '', fragment] = splitOnce(url, '#')
	const [path = ''] = splitOnce(rest, '?')

	if (path === '') {
		return fragment === undefined || ids.has(decode(fragment))
			? { href: url }
			: { problem: `no heading #${fragment} on this page` }
	}

	const target = resolveFrom(from, decode(path.startsWith('/') ? `.${path}` : path))
	if (target === undefined) {
		return { problem: `${url} climbs out of the repository` }
	}

	const page = as === 'link' ? links.pages.get(target) : undefined
	if (page !== undefined) {
		if (fragment !== undefined && !page.ids.has(decode(fragment))) {
			return { problem: `${target} has no heading #${fragment}` }
		}
		return { href: page.url + (fragment === undefined ? '' : `#${fragment}`) }
	}

	const kind = exists(links.root, target)
	if (kind === undefined) {
		return { problem: `${target} does not exist` }
	}
	if (as === 'asset') {
		links.assets.add(target)
		return { href: `/files/${target}` }
	}
	return {
		href: sourceUrl(target, links.commit, kind) + (fragment === undefined ? '' : `#${fragment}`),
	}
}

function splitOnce(text: string, separator: string): [string, string | undefined] {
	const at = text.indexOf(separator)
	return at === -1 ? [text, undefined] : [text.slice(0, at), text.slice(at + 1)]
}

function decode(text: string): string {
	try {
		return decodeURIComponent(text)
	} catch {
		return text
	}
}
