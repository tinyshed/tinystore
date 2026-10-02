import { InvalidError } from './errors.ts'

/** A duration's units, largest first, in the order its text writes them. */
type Units = ['w', 'd', 'h', 'm', 's', 'ms']

/** One of a duration's units: weeks, days, hours, minutes, seconds, milliseconds. */
export type Unit = Units[number]

/**
 * Every spelling of units each written once, largest first: a number and its
 * unit, then the same of the units after it, or none.
 *
 *     ['s', 'ms'] → `${number}s${number}ms` | `${number}s` | `${number}ms`
 */
type Spelled<U extends readonly string[]> = U extends readonly [
	infer First extends string,
	...infer Rest extends string[],
]
	? `${number}${First}${Spelled<Rest> | ''}` | Spelled<Rest>
	: never

/**
 * A duration's text, '30d', '1h30m', '15m', '1s' or '250ms', which the type
 * checks as it is written, so that '1hr' or '90mins' does not compile. Text
 * from elsewhere, an environment's, is cast as Duration and checked when the
 * call runs.
 */
export type DurationText = Spelled<Units>

/** A length of time: milliseconds, or the text of one, such as '1h30m'. */
export type Duration = number | DurationText

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

/**
 * A duration's milliseconds; each unit once, largest first: 1h30m, not
 * 90m30m. It takes any text, since text cast as a Duration reaches it too.
 */
export function ms(d: Duration | string): number {
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
