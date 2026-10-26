/**
 * A `tinystore` command read word by word, as the command line reads it:
 * which command it is, what each word stands for, and what a call like it
 * prints. Everything comes from the help the binary prints, `cli.json`, and
 * what its examples printed on a demo store, `cli-prints.json`, both written
 * by `just cli-json`: a word the CLI learns, the explain learns with it.
 *
 *     tinystore kv ./data set flags beta on --ttl 1h
 *     command   group directory verb <bucket> <key> <value> flag and its value
 */

import { words } from '../content/words'

export interface Flag {
	name: string
	value: string
	means: string
}

export interface Command {
	name: string
	group: string
	takes: string
	does: string
	flags: Flag[]
	examples: string[]
	more: string
}

export interface Help {
	commands: Command[]
	words: Record<string, string>
}

export type Printed = Record<string, { example: string; prints: string }>

/** What a word of the line is. */
export type Role = 'program' | 'command' | 'verb' | 'word' | 'flag' | 'value' | 'unknown'

export interface Word {
	text: string
	role: Role
	/** What it stands for: a placeholder of the command's `takes`, or a flag's name. */
	stands?: string
	means: string
}

export interface Explained {
	words: Word[]
	command?: Command
	/** What a call like it printed, its example's words given as the line's. */
	prints?: string
	problem?: string
}

/** Reads `line` against the help. */
export function explain(line: string, help: Help, printed: Printed = {}): Explained {
	const given = words(line)
	if (given[0] === 'tinystore') given.shift()
	const read: Word[] = [
		{ text: 'tinystore', role: 'program', means: 'the command line, one binary for every store' },
	]
	const command = commandOf(given, help)
	if (command === undefined) {
		const first = given[0] ?? ''
		const near = nearest(first, firsts(help))
		const problem =
			first === '' ? 'type a command, as kv ./data get sessions 42' : `no command ${first}${near}`
		return { words: [...read, ...given.map(text => unknown(text))], problem }
	}
	read.push(...wordsOf(command, given, help))
	const problem = read.find(word => word.role === 'unknown')?.means
	return { words: read, command, prints: printedFor(command, read, help, printed), problem }
}

/** The command the words name: a group's verb, `kv set`, or a command of its own, `status`. */
function commandOf(given: string[], help: Help): Command | undefined {
	const [first, ...rest] = given
	const ofGroup = help.commands.filter(command => command.name.startsWith(`${first} `))
	if (ofGroup.length > 0) {
		// a verb comes first, or after the directory
		return ofGroup.find(command => rest.slice(0, 2).includes(command.name.split(' ')[1] ?? ''))
	}
	const two = help.commands.find(command => command.name === `${first} ${rest[0]}`)
	return two ?? help.commands.find(command => command.name === first)
}

/** Each word after `tinystore`, read against the command's `takes` and flags. */
function wordsOf(command: Command, given: string[], help: Help): Word[] {
	const [group] = command.name.split(' ')
	const isGroup = help.commands.some(
		other => other !== command && other.name.startsWith(`${group} `),
	)
	const slots = takes(command)
	const read: Word[] = []
	let slot = 0
	for (let index = 0; index < given.length; index++) {
		const text = given[index] ?? ''
		if (index === 0) {
			read.push({
				text,
				role: 'command',
				means: isGroup ? groupMeans(group ?? '', help) : command.does,
			})
			continue
		}
		if (text === '--') {
			const rest = given.slice(index + 1).join(' ')
			if (rest !== '')
				read.push({
					text: rest,
					role: 'word',
					stands: '<command>',
					means: help.words['<command>'] ?? '',
				})
			break
		}
		if (text.startsWith('-') && text.length > 1 && Number.isNaN(Number(text))) {
			const [name, inline] = text.split(/=(.*)/s)
			const flag = command.flags.find(known => known.name === name)
			if (flag === undefined) {
				read.push(
					unknown(
						text,
						`${command.name} takes no flag ${name}${nearest(
							name ?? '',
							command.flags.map(known => known.name),
						)}`,
					),
				)
				continue
			}
			read.push({ text: name ?? text, role: 'flag', stands: flag.name, means: flag.means })
			if (flag.value !== '') {
				const value = inline ?? given[++index]
				if (value !== undefined)
					read.push({
						text: value,
						role: 'value',
						stands: flag.name,
						means: `${flag.value} of ${flag.name}`,
					})
			}
			continue
		}
		// an optional directory is left out when the verb comes at once
		while (
			slots[slot]?.optional &&
			slots[slot]?.literal === undefined &&
			slots[slot + 1]?.literal === text
		)
			slot++
		const taken = slots[slot]
		if (taken === undefined) {
			read.push(unknown(text, `${command.name} takes nothing more: ${command.takes}`))
			continue
		}
		if (taken.literal !== undefined) {
			read.push({ text, role: 'verb', means: command.does })
		} else {
			read.push({ text, role: 'word', stands: taken.word, means: help.words[taken.word] ?? '' })
		}
		if (!taken.many) slot++
	}
	return read
}

interface Slot {
	word: string
	literal?: string
	optional: boolean
	many: boolean
}

/** The words a command takes after its own: `[dir] get <bucket> <key>`. */
function takes(command: Command): Slot[] {
	return command.takes
		.split(/\s+/)
		.filter(Boolean)
		.flatMap(raw => {
			const token = raw.replaceAll('"', '')
			const many = /(…|\.\.\.)$/.test(token)
			const word = token.replace(/(…|\.\.\.)$/, '')
			if (word.startsWith('--')) return []
			if (word.startsWith('<') || word.startsWith('[')) {
				// <host>:<port> is one word on the line
				return [
					{ word: word.includes('>:<') ? '<host>' : word, optional: word.startsWith('['), many },
				]
			}
			return [{ word, literal: word, optional: false, many: false }]
		})
}

function groupMeans(group: string, help: Help): string {
	const verbs = help.commands.filter(command => command.name.startsWith(`${group} `))
	return `the ${group} commands: ${verbs.map(command => command.name.split(' ')[1]).join(', ')}`
}

function unknown(text: string, means = 'not a word this command reads'): Word {
	return { text, role: 'unknown', means }
}

function firsts(help: Help): string[] {
	return [...new Set(help.commands.map(command => command.name.split(' ')[0] ?? ''))]
}

/** What a call like this printed: its example's words as the line gives them. */
function printedFor(
	command: Command,
	read: Word[],
	help: Help,
	printed: Printed,
): string | undefined {
	const shown = printed[command.name]
	if (shown === undefined || shown.prints === '') return undefined
	const example = explain(shown.example, help).words
	let prints = shown.prints
	const swaps = example
		.filter(word => (word.role === 'word' || word.role === 'value') && word.text.length > 1)
		.flatMap(word => {
			const given = read.find(other => other.role === word.role && other.stands === word.stands)
			return given === undefined || given.text === word.text
				? []
				: [[word.text, given.text] as const]
		})
		.sort((a, b) => b[0].length - a[0].length)
	for (const [from, to] of swaps) {
		prints = prints.replace(new RegExp(`(?<![\\w./-])${escaped(from)}(?![\\w-])`, 'g'), to)
	}
	return prints
}

function escaped(text: string): string {
	return text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

/** `; did you mean x?` for the name nearest a misspelt one, two edits away at most. */
function nearest(given: string, names: string[]): string {
	const edits = given.length < 3 ? 0 : given.length < 7 ? 1 : 2
	const near = names
		.map(name => [name, distance(given, name)] as const)
		.filter(([, far]) => far <= edits)
	near.sort((a, b) => a[1] - b[1])
	return near[0] === undefined ? '' : `; did you mean ${near[0][0]}?`
}

function distance(a: string, b: string): number {
	const row = Array.from({ length: b.length + 1 }, (_, index) => index)
	for (let i = 1; i <= a.length; i++) {
		let previous = row[0] ?? 0
		row[0] = i
		for (let j = 1; j <= b.length; j++) {
			const kept = row[j] ?? 0
			row[j] = Math.min(
				(row[j] ?? 0) + 1,
				(row[j - 1] ?? 0) + 1,
				previous + (a[i - 1] === b[j - 1] ? 0 : 1),
			)
			previous = kept
		}
	}
	return row[b.length] ?? 0
}
