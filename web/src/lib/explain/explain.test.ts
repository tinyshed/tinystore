import { describe, expect, test } from 'bun:test'

import cli from '../content/cli.json'
import printed from '../content/cli-prints.json'
import { explain, type Help } from './explain'

const help = cli as Help

/** Each word of the line with what it is, as `role:text`. */
function roles(line: string): string[] {
	return explain(line, help).words.map(word => `${word.role}:${word.text}`)
}

describe('explain', () => {
	test('reads each word of a command as the help names it', () => {
		const read = explain('tinystore kv ./data set flags beta on --ttl 1h', help)
		expect(read.command?.name).toBe('kv set')
		expect(read.words.map(word => word.stands ?? word.role)).toEqual([
			'program',
			'command',
			'<dir>',
			'verb',
			'<bucket>',
			'<key>',
			'<value>',
			'--ttl',
			'--ttl',
		])
		expect(read.words[4]?.means).toBe(help.words['<bucket>'])
		expect(read.problem).toBeUndefined()
	})

	test('takes a verb before its directory, the current one', () => {
		expect(roles('kv get sessions 42')).toEqual([
			'program:tinystore',
			'command:kv',
			'verb:get',
			'word:sessions',
			'word:42',
		])
	})

	test('says a misspelt command or flag, and its nearest', () => {
		expect(explain('tinystore metircs ./data names', help).problem).toBe(
			'no command metircs; did you mean metrics?',
		)
		expect(explain('tinystore kv ./data set a b c --tll 1h', help).problem).toContain(
			'did you mean --ttl?',
		)
	})

	test('gives a lock its command after the double dash, whole', () => {
		const read = explain('tinystore lock ./data hold cron nightly -- ./backup.sh --fast', help)
		expect(read.words.at(-1)).toMatchObject({ text: './backup.sh --fast', stands: '<command>' })
	})

	test('prints what a call like it printed, with the line’s own words', () => {
		const read = explain('tinystore kv ./data set sessions 42 on', help, printed)
		expect(printed['kv set'].prints).toBe('set beta in bucket flags, expiring in 1h')
		expect(read.prints).toBe('set 42 in bucket sessions, expiring in 1h')
	})
})
