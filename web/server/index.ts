import { type App, start, stop } from './bootstrap.ts'

let app: App

try {
	app = await start()
} catch (error) {
	// the logger lives in the store, which may be what failed, so stderr is all there is
	const reason = error instanceof Error ? (error.stack ?? error.message) : String(error)

	process.stderr.write(`failed to start the site\n${reason}\n`)
	process.exit(1)
}

let shutdownTask: Promise<void> | undefined

const shutdown = (signal: NodeJS.Signals) => {
	shutdownTask ??= (async () => {
		app.logger.info('shutdown signal received', { signal })

		try {
			await stop(app)
			process.exit(0)
		} catch (error) {
			process.stderr.write(`failed to stop the site: ${String(error)}\n`)
			process.exit(1)
		}
	})()

	return shutdownTask
}

const handleSignal = (signal: NodeJS.Signals) => {
	void shutdown(signal)
}

process.on('SIGINT', handleSignal)
process.on('SIGTERM', handleSignal)
