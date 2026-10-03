import type { Logger, Store } from 'tinystore'

import { type Analytics, createAnalytics } from './analytics.ts'
import { type Config, createConfig } from './config.ts'
import { readSite, type Site } from './files.ts'
import { createHttpServer, type HttpServer } from './http.ts'
import { openStore } from './store.ts'
import { withTimeout } from './timeout.ts'

export interface App {
	config: Config
	site: Site
	store: Store
	logger: Logger
	analytics: Analytics
	http: HttpServer
}

export async function start(): Promise<App> {
	const config = createConfig(process.env)

	// a missing build fails here, before a store is opened for nothing
	const site = readSite(config.SITE_DIR)

	const store = await openStore(config.DATA_DIR)

	// the console and the store at once: the server needs no other logger
	const logger = store.records.logger('site')

	// readers' views and events: kept, and too many to print
	const readers = store.records.logger('readers', { console: 'off' })

	const analytics = createAnalytics(store, readers, site.pages)

	const http = createHttpServer(site, analytics, store, logger, {
		host: config.HOST,
		port: config.PORT,
		trustProxy: config.TRUST_PROXY,
	})

	logger.info('site serving', {
		host: http.host,
		port: http.port,
		pages: site.pages.size,
		data: config.DATA_DIR,
	})

	return { config, site, store, logger, analytics, http }
}

interface ShutdownStep {
	name: string
	close(): Promise<void>
}

export async function stop(app: App): Promise<void> {
	const timeoutMs = app.config.SHUTDOWN_TIMEOUT_MS

	app.logger.info('site stopping')

	// ordered: stop taking requests before closing the store they write into;
	// closing the store writes the counters' last values and the logger's last lines
	const steps: ShutdownStep[] = [
		{ name: 'http', close: () => app.http.stop() },
		{ name: 'store', close: () => app.store.close() },
	]

	const failed: string[] = []

	for (const step of steps) {
		const error = await withTimeout(step.close(), timeoutMs, step.name).then(
			() => undefined,
			(reason: unknown) => reason ?? new Error('unknown failure'),
		)

		if (error !== undefined) {
			failed.push(step.name)
			process.stderr.write(`shutdown step ${step.name} failed: ${String(error)}\n`)
		}
	}

	if (failed.length > 0) {
		throw new Error(`shutdown failed: ${failed.join(', ')}`)
	}
}
