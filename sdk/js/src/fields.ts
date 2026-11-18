// A query is a plain object, and a field the SDK does not know would go
// nowhere: `names` on a metrics query reached the store as no condition at
// all. So a query's keys are checked against the fields it takes, before
// anything is sent.

import { InvalidError } from './errors.ts'

/**
 * Refuses a key of `given` that `known` does not list, naming it, and the
 * field it differs from by an `s` alone when there is one: `names` for
 * `name`. A key given as undefined carries nothing and passes.
 */
export function refuseUnknown(what: string, given: object, known: readonly string[]): void {
	for (const [key, value] of Object.entries(given)) {
		if (value === undefined || known.includes(key)) {
			continue
		}
		const near = known.find(field => field === `${key}s` || `${field}s` === key)
		const hint = near === undefined ? '' : `; did you mean \`${near}\`?`
		throw new InvalidError(`${what}: no field \`${key}\`${hint}`)
	}
}
