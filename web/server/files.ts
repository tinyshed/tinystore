import { readdirSync, statSync } from 'node:fs'
import { extname, join, relative, sep } from 'node:path'

/** What a request for a file is, as the server counts it. */
export type Kind = 'page' | 'data' | 'markdown' | 'asset'

export interface SiteFile {
	/** the file on disk */
	path: string
	type: string
	etag: string
	kind: Kind
	/** the page a view of the file counts for: '/docs/kv' for its HTML, its data and its markdown */
	page: string
	immutable: boolean
	/** the same bytes compressed, made beside it at build time */
	br?: string
	gzip?: string
}

export interface Site {
	find(pathname: string): SiteFile | undefined
	/** the page served with a 404, for any path the build has no file for */
	notFound: SiteFile | undefined
	/** every page the build made, the values a view's page label may take */
	pages: Set<string>
}

const types: Record<string, string> = {
	'.html': 'text/html; charset=utf-8',
	'.js': 'text/javascript; charset=utf-8',
	'.css': 'text/css; charset=utf-8',
	'.json': 'application/json; charset=utf-8',
	'.md': 'text/markdown; charset=utf-8',
	'.txt': 'text/plain; charset=utf-8',
	'.xml': 'application/xml; charset=utf-8',
	'.svg': 'image/svg+xml',
	'.png': 'image/png',
	'.jpg': 'image/jpeg',
	'.webp': 'image/webp',
	'.ico': 'image/x-icon',
	'.woff2': 'font/woff2',
}

/**
 * Reads the build once, at start: every file by the path it answers. The
 * build never changes under a running server, so nothing is looked up on
 * disk per request; a new build is a new server.
 *
 *     index.html                         → /, a page
 *     docs/kv.html                       → /docs/kv, a page
 *     docs/kv/__data.json                → the same page's data, fetched on navigation
 *     docs/kv.md, llms.txt               → markdown, an agent's or a reader's copy
 *     _app/immutable/…                   → an asset, cached for good
 */
export function readSite(dir: string): Site {
	const files = new Map<string, SiteFile>()
	const all = walk(dir)
	const present = new Set(all)

	for (const path of all) {
		if (path.endsWith('.br') || path.endsWith('.gz')) {
			continue
		}
		const route = routeOf(relative(dir, path).split(sep).join('/'))
		const stat = statSync(path)
		files.set(route.path, {
			path,
			type: types[extname(path)] ?? 'application/octet-stream',
			etag: `"${stat.size.toString(36)}-${Math.trunc(stat.mtimeMs).toString(36)}"`,
			kind: route.kind,
			page: route.page,
			immutable: route.path.startsWith('/_app/immutable/'),
			...(present.has(`${path}.br`) ? { br: `${path}.br` } : {}),
			...(present.has(`${path}.gz`) ? { gzip: `${path}.gz` } : {}),
		})
	}

	if (!files.has('/')) {
		throw new Error(`${dir} holds no index.html: build the site first, bun run build`)
	}

	const pages = new Set(
		[...files.values()].filter(file => file.kind === 'page').map(file => file.page),
	)

	return {
		find: pathname => files.get(pathname),
		notFound: files.get('/404'),
		pages,
	}
}

export function routeOf(file: string): { path: string; kind: Kind; page: string } {
	if (file === 'index.html') {
		return { path: '/', kind: 'page', page: '/' }
	}
	if (file.endsWith('.html')) {
		const path = `/${file.slice(0, -'.html'.length)}`
		return { path, kind: 'page', page: path }
	}
	if (file === '__data.json' || file.endsWith('/__data.json')) {
		const page = `/${file.slice(0, -'__data.json'.length)}`.replace(/\/$/, '') || '/'
		return { path: `/${file}`, kind: 'data', page }
	}
	if (file.endsWith('.md')) {
		return { path: `/${file}`, kind: 'markdown', page: `/${file.slice(0, -'.md'.length)}` }
	}
	if (file === 'llms.txt' || file === 'llms-full.txt') {
		return { path: `/${file}`, kind: 'markdown', page: `/${file}` }
	}
	return { path: `/${file}`, kind: 'asset', page: '' }
}

/**
 * The file as the request can take it: brotli or gzip when it says so and the
 * build made one, a 304 when its copy is still the file, nothing but headers
 * for a HEAD.
 */
export function respond(file: SiteFile, request: Request, status = 200): Response {
	const accepts = request.headers.get('accept-encoding') ?? ''
	const [path, encoding] =
		file.br !== undefined && /\bbr\b/.test(accepts)
			? [file.br, 'br']
			: file.gzip !== undefined && /\bgzip\b/.test(accepts)
				? [file.gzip, 'gzip']
				: [file.path, undefined]

	const etag = encoding === undefined ? file.etag : `${file.etag.slice(0, -1)}-${encoding}"`
	const headers = new Headers({
		'content-type': file.type,
		'cache-control': cacheOf(file),
		etag,
		vary: 'accept-encoding',
	})
	if (encoding !== undefined) {
		headers.set('content-encoding', encoding)
	}

	if (status === 200 && request.headers.get('if-none-match') === etag) {
		return new Response(null, { status: 304, headers })
	}
	return new Response(request.method === 'HEAD' ? null : Bun.file(path), { status, headers })
}

// a hashed asset never changes; a page, its data and its markdown change with every deploy
function cacheOf(file: SiteFile): string {
	if (file.immutable) {
		return 'public, max-age=31536000, immutable'
	}
	return file.kind === 'asset' ? 'public, max-age=3600' : 'no-cache'
}

function walk(dir: string): string[] {
	return readdirSync(dir, { withFileTypes: true }).flatMap(entry => {
		const path = join(dir, entry.name)
		return entry.isDirectory() ? walk(path) : [path]
	})
}
