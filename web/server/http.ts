import type { Logger } from '@tinyshed/tinystore'
import { z } from 'zod'

import { readerEvents } from '../src/lib/events.ts'
import type { Analytics } from './analytics.ts'
import { respond, type Site } from './files.ts'

export interface HttpServerConfig {
	host: string
	port: number
	trustProxy: boolean
}

export interface HttpServer {
	host: string
	port: number

	stop(): Promise<void>
}

/** What the health routes ask the store: whether it answers, whether its counters and lines reach it. */
export interface Readiness {
	status(): Promise<unknown>
	metrics: { failures: number; lastFailure: Error | undefined }
	/** the loggers whose dropped lines health reports, by their stream */
	loggers: Record<string, { readonly dropped: number }>
}

type Handler = (request: Request, address: string) => Promise<Response> | Response

const eventSchema = z.object({
	name: z.enum(readerEvents),
	page: z.string().startsWith('/').max(300),
	value: z.string().max(200).optional(),
})

// a beacon is a few dozen bytes; anything near this is not one
const largestEvent = 2048

/**
 * The site's requests, without a port: the API's three routes, and every
 * other GET a file of the build, counted when it is a page, its data or its
 * markdown. Tests call it directly.
 */
export function createRouter(
	site: Site,
	analytics: Analytics,
	store: Readiness,
	logger: Logger,
): Handler {
	const api: Record<string, Handler> = {
		// the process is up; and what it failed to keep, which no response shows otherwise
		'GET /api/health': () =>
			Response.json({
				status: 'ok',
				flushesFailed: store.metrics.failures,
				lastFlushFailure: store.metrics.lastFailure?.message,
				linesDropped: Object.fromEntries(
					Object.entries(store.loggers).map(([stream, each]) => [stream, each.dropped]),
				),
			}),

		'GET /api/ready': async () => {
			try {
				await store.status()
				return Response.json({ status: 'ok' })
			} catch (error) {
				logger.warn('the store did not answer', { error: String(error) })
				return Response.json({ status: 'degraded' }, { status: 503 })
			}
		},

		'POST /api/events': (request, address) => receiveEvent(analytics, request, address),
	}

	return async (request, address) => {
		const url = new URL(request.url)
		const route = api[`${request.method} ${url.pathname}`]
		if (route !== undefined) {
			return secured(await route(request, address))
		}
		if (url.pathname.startsWith('/api/')) {
			return secured(
				Response.json({ code: 'NOT_FOUND', message: 'no such route' }, { status: 404 }),
			)
		}
		if (request.method !== 'GET' && request.method !== 'HEAD') {
			return secured(new Response(null, { status: 405, headers: { allow: 'GET, HEAD' } }))
		}
		return secured(serveFile(site, analytics, request, url, address))
	}
}

// a reader's event, as a page's script sends it through sendBeacon
async function receiveEvent(
	analytics: Analytics,
	request: Request,
	address: string,
): Promise<Response> {
	if (Number(request.headers.get('content-length') ?? '0') > largestEvent) {
		return new Response(null, { status: 413 })
	}
	let body: unknown
	try {
		// a beacon sends its JSON as text/plain, so the type is not checked
		body = JSON.parse(await request.text())
	} catch {
		return Response.json({ code: 'BAD_REQUEST', message: 'the body must be JSON' }, { status: 400 })
	}
	const event = eventSchema.safeParse(body)
	if (!event.success) {
		return Response.json(
			{ code: 'VALIDATION_ERROR', message: 'not an event this site counts' },
			{ status: 400 },
		)
	}
	const outcome = await analytics.event(request, event.data, address)
	return new Response(null, { status: outcome === 'limited' ? 429 : 204 })
}

function serveFile(
	site: Site,
	analytics: Analytics,
	request: Request,
	url: URL,
	address: string,
): Response {
	let pathname: string
	try {
		pathname = decodeURIComponent(url.pathname)
	} catch {
		return notFound(site, request)
	}

	// one address a page: /docs/ is /docs
	if (pathname.length > 1 && pathname.endsWith('/')) {
		return Response.redirect(`${url.origin}${pathname.replace(/\/+$/, '')}${url.search}`, 308)
	}

	const file = site.find(pathname)
	if (file === undefined || pathname === '/404') {
		return notFound(site, request)
	}

	const response = respond(file, request)
	if (file.kind !== 'asset' && request.method === 'GET') {
		void analytics.view(request, file, address)
	}
	return response
}

function notFound(site: Site, request: Request): Response {
	return site.notFound === undefined
		? new Response('not found', { status: 404 })
		: respond(site.notFound, request, 404)
}

function secured(response: Response): Response {
	response.headers.set('x-content-type-options', 'nosniff')
	response.headers.set('referrer-policy', 'strict-origin-when-cross-origin')
	response.headers.set('x-frame-options', 'DENY')
	return response
}

export function createHttpServer(
	site: Site,
	analytics: Analytics,
	store: Readiness,
	logger: Logger,
	config: HttpServerConfig,
): HttpServer {
	const route = createRouter(site, analytics, store, logger)

	const server = Bun.serve({
		hostname: config.host,
		port: config.port,
		fetch: (request, server) => route(request, addressOf(request, server, config.trustProxy)),
		error(error) {
			logger.error('unhandled http error', { error: String(error) })
			return new Response('internal server error', { status: 500 })
		},
	})

	return {
		host: config.host,
		port: server.port ?? config.port,
		async stop(): Promise<void> {
			await server.stop()
		},
	}
}

function addressOf(request: Request, server: Bun.Server<unknown>, trustProxy: boolean): string {
	if (trustProxy) {
		const forwarded = request.headers.get('x-forwarded-for')?.split(',')[0]?.trim()
		if (forwarded !== undefined && forwarded !== '') {
			return forwarded
		}
	}
	return server.requestIP(request)?.address ?? ''
}
