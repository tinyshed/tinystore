// A .env file's variables, as a store-less logger reads them for LOG_LEVEL and
// the rest; a config sends its .env files to the core, which reads them with
// the same rules (testdata/config/env.json).

import { InvalidError } from './errors.ts'

/**
 * The variables of a .env file, as Go's and Python's configs read one: NAME=value
 * lines, export before a name, # comments, a comment after a space, double
 * quotes reading \n \r \t \" and \\, single quotes reading nothing, a quoted
 * value over lines; a later name wins.
 */
export function parseDotenv(text: string): Record<string, string> {
	const values: Record<string, string> = {}
	let line = 1
	let rest = text
	while (rest !== '') {
		const [name, value, after] = dotenvAssignment(rest, line)
		line += rest.slice(0, rest.length - after.length).split('\n').length - 1
		if (name !== '') {
			values[name] = value
		}
		rest = after
	}
	return values
}

function dotenvAssignment(text: string, number: number): [string, string, string] {
	const end = text.indexOf('\n')
	const line = end < 0 ? text : text.slice(0, end)
	const rest = end < 0 ? '' : text.slice(end + 1)
	const trimmed = line.trim()
	if (trimmed === '' || trimmed.startsWith('#')) {
		return ['', '', rest]
	}
	const equals = line.indexOf('=')
	if (equals < 0) {
		throw new InvalidError(`line ${number}: ${JSON.stringify(line)} is no NAME=value`)
	}
	let name = line.slice(0, equals).trim()
	if (name.startsWith('export ')) {
		name = name.slice('export '.length).trim()
	}
	if (!/^[\p{L}_][\p{L}\p{Nd}_]*$/u.test(name)) {
		throw new InvalidError(`line ${number}: ${JSON.stringify(name)} is no variable's name`)
	}
	let start = equals + 1
	while (line[start] === ' ' || line[start] === '\t') {
		start++
	}
	if (line[start] === '"' || line[start] === "'") {
		return [name, ...dotenvQuoted(text.slice(start), number)]
	}
	let value = line.slice(start)
	const comment = value.indexOf(' #')
	if (comment >= 0) {
		value = value.slice(0, comment)
	}
	return [name, value.replace(/[ \t\r]+$/, ''), rest]
}

function dotenvQuoted(text: string, number: number): [string, string] {
	const quote = text[0]
	let out = ''
	for (let i = 1; i < text.length; i++) {
		const c = text[i] as string
		if (c === quote) {
			const end = text.indexOf('\n', i + 1)
			return [out, end < 0 ? '' : text.slice(end + 1)]
		}
		if (c === '\\' && quote === '"' && i + 1 < text.length) {
			i++
			const escaped = text[i] as string
			out += escaped === 'n' ? '\n' : escaped === 'r' ? '\r' : escaped === 't' ? '\t' : escaped
		} else {
			out += c
		}
	}
	throw new InvalidError(`line ${number}: a value opened with ${quote} is never closed`)
}
