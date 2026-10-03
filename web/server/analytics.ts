import type { Logger, Store } from '@tinyshed/tinystore'

import type { ReaderEvent } from '../src/lib/events.ts'
import { agentOf } from './agents.ts'
import type { SiteFile } from './files.ts'
import { Visitors } from './visitors.ts'

export interface ReaderEventInput {
	name: ReaderEvent
	page: string
	value?: string | undefined
}

export interface Analytics {
	/** a page, its data or its markdown was served; the response never waits for what this returns */
	view(request: Request, file: SiteFile, address: string): Promise<void>
	/** what a reader did on a page; refused past a reader's rate */
	event(request: Request, event: ReaderEventInput, address: string): Promise<'kept' | 'limited'>
}

/**
 * The site's own analytics, kept in TinyStore as any application's would be.
 *
 *     metrics  site_views_total{page, via, agent}  site_events_total{name}
 *     records  stream "readers": a view or an event a record, with the day's
 *              visitor id, where the reader came from and their language
 *     kv       the day's salt, and the limiter a reader's events go through
 *
 * Counters live in the process and are written every 15 seconds; a record is
 * handed to a logger that never waits. Only a reader's first view of a day, and
 * an event's limiter, wait for the store.
 */
export function createAnalytics(store: Store, logger: Logger, pages: Set<string>): Analytics {
	const views = store.metrics.counter('site_views_total')
	const events = store.metrics.counter('site_events_total')
	const visitors = new Visitors(store.kv.bucket<string>('site'))
	const limiter = store.kv.limiter('site-events', { rate: '60/m', burst: 30 })

	return {
		view(request, file, address) {
			const userAgent = request.headers.get('user-agent')
			const agent = agentOf(userAgent)

			views.with({ page: file.page, via: file.kind, agent }).inc()

			return visitors.of(address, userAgent ?? '').then(
				visitor => {
					logger.event('view', {
						page: file.page,
						via: file.kind,
						agent,
						visitor,
						...origin(request),
						...language(request),
					})
				},
				(error: unknown) => {
					logger.warn('a view went unrecorded', { page: file.page, error: String(error) })
				},
			)
		},

		async event(request, event, address) {
			const visitor = await visitors.of(address, request.headers.get('user-agent') ?? '')
			const allowance = await limiter.allow(visitor)
			if (!allowance.ok) {
				return 'limited'
			}

			const page = pages.has(event.page) ? event.page : 'other'
			events.with({ name: event.name }).inc()
			logger.event(event.name, {
				page,
				visitor,
				...(event.value === undefined ? {} : { value: event.value }),
			})
			return 'kept'
		},
	}
}

/**
 * Where a reader came from: a page of the site when they moved within it, or
 * the host of the site that sent them. Never the whole address, which can carry
 * a search or a token.
 */
export function origin(request: Request): { from?: string; referrer?: string } {
	const referer = request.headers.get('referer')
	if (referer === null) {
		return {}
	}
	try {
		const url = new URL(referer)
		return url.host === request.headers.get('host')
			? { from: url.pathname }
			: { referrer: url.hostname }
	} catch {
		return {}
	}
}

/** The reader's first language, 'ru' of `ru-RU,ru;q=0.9,en;q=0.8`. */
export function language(request: Request): { language?: string } {
	const first = request.headers.get('accept-language')?.split(',')[0]?.split(/[-;]/)[0]?.trim()
	return first === undefined || first === '' || first === '*'
		? {}
		: { language: first.toLowerCase() }
}
