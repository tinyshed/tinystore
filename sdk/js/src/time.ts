import { InvalidError } from './errors.ts'

/**
 * A length of time: milliseconds, or text such as '30d', '1h30m', '15m', '1s'
 * and '250ms'.
 */
export type Duration = number | string

/** A moment: a Date, or unix milliseconds. */
export type Time = Date | number

const units: Record<string, number> = {
	ms: 1,
	s: 1000,
	m: 60_000,
	h: 3_600_000,
	d: 86_400_000,
	w: 604_800_000,
}

/** A duration's milliseconds; each unit once, largest first: 1h30m, not 90m30m. */
export function ms(d: Duration): number {
	if (typeof d === 'number') {
		if (!Number.isFinite(d) || d < 0) {
			throw new InvalidError(`a duration of ${d} milliseconds`)
		}
		return Math.round(d)
	}
	const parts = /^(?:(\d+)w)?(?:(\d+)d)?(?:(\d+)h)?(?:(\d+)m(?!s))?(?:(\d+)s)?(?:(\d+)ms)?$/.exec(
		d.trim(),
	)
	if (parts === null || d.trim() === '') {
		throw new InvalidError(
			`the duration ${JSON.stringify(d)}; write it as 30d, 1h30m, 15m, 1s or 250ms`,
		)
	}
	const [, w, days, h, m, s, milli] = parts
	return (
		Number(w ?? 0) * units.w! +
		Number(days ?? 0) * units.d! +
		Number(h ?? 0) * units.h! +
		Number(m ?? 0) * units.m! +
		Number(s ?? 0) * units.s! +
		Number(milli ?? 0)
	)
}

/** A moment's unix milliseconds. */
export function unixMs(t: Time): number {
	const n = typeof t === 'number' ? t : t.getTime()
	if (!Number.isFinite(n)) {
		throw new InvalidError('an invalid Date')
	}
	return Math.trunc(n)
}

/** A moment the server named in unix milliseconds, none for 0 or absent. */
export function dateOf(unix: number | undefined): Date | undefined {
	return unix === undefined || unix === 0 ? undefined : new Date(unix)
}
