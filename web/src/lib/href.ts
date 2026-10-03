import { resolve } from '$app/paths'

/**
 * A path of the site wherever it is served: the root in production, a folder
 * on a preview. resolve() types its argument as the routes it knows, and these
 * paths come from the docs, so the cast lives here once.
 */
export function href(path: string): string {
	return resolve(path as '/')
}

/** The site's absolute links in HTML made at build time, `href="/docs"`, under the same base. */
export function rebase(html: string): string {
	const base = href('/').replace(/\/$/, '')
	return base === '' ? html : html.replace(/(href|src|srcset)="\//g, `$1="${base}/`)
}

/** A pathname as the site's own path, without the folder a preview serves it from. */
export function unbase(pathname: string): string {
	const base = href('/').replace(/\/$/, '')
	return base.startsWith('/') && pathname.startsWith(base)
		? pathname.slice(base.length) || '/'
		: pathname
}
