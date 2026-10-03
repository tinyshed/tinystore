import type { ReaderEvent } from './events'
import { href } from './href'

/** Tells the server what the reader did; a refused or failed beacon changes nothing for them. */
export function track(name: ReaderEvent, value?: string): void {
	try {
		const body = JSON.stringify({
			name,
			page: location.pathname,
			...(value === undefined ? {} : { value }),
		})
		navigator.sendBeacon(href('/api/events'), body)
	} catch {
		// counting is best effort
	}
}
