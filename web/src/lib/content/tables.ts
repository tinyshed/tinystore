import { execFileSync } from 'node:child_process'
import { readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'

import { checkout } from './repo'

/**
 * Markdown with each table's cells padded so that its pipes line up, as an
 * IDE formats a table. A fence's lines stay as they are, and a pipe escaped
 * as `\|` stays inside its cell.
 *
 *     | a | bb |        | a   | bb |
 *     |---|---|    →    |-----|----|
 *     | ccc | d |       | ccc | d  |
 */
export function alignTables(markdown: string): string {
	const lines = markdown.split('\n')
	const out: string[] = []
	let fence: string | undefined
	for (let i = 0; i < lines.length; i++) {
		const line = lines[i] as string
		if (fence !== undefined) {
			out.push(line)
			if (closes(line, fence)) {
				fence = undefined
			}
			continue
		}
		fence = opens(line)
		const rows = fence === undefined ? tableAt(lines, i) : 0
		if (rows === 0) {
			out.push(line)
			continue
		}
		out.push(...aligned(lines.slice(i, i + rows)))
		i += rows - 1
	}
	return out.join('\n')
}

/** The tracked markdown files whose tables `alignTables` would change. */
export function unalignedFiles(root = checkout()): string[] {
	return markdownFiles(root).filter(file => {
		const text = readFileSync(join(root, file), 'utf8')
		return alignTables(text) !== text
	})
}

/** Aligns the tables of every tracked markdown file, as `task tables` does, and names those it changed. */
export function alignFiles(root = checkout()): string[] {
	return markdownFiles(root).filter(file => {
		const path = join(root, file)
		const text = readFileSync(path, 'utf8')
		const written = alignTables(text)
		if (written !== text) {
			writeFileSync(path, written)
		}
		return written !== text
	})
}

function markdownFiles(root: string): string[] {
	const listed = execFileSync(
		'git',
		['ls-files', '-z', '--cached', '--others', '--exclude-standard', '*.md'],
		{ cwd: root, encoding: 'utf8' },
	)
	return listed.split('\0').filter(file => file !== '')
}

// a fence opens with three or more backticks or tildes, and closes with as many of the same
function opens(line: string): string | undefined {
	return /^\s*(`{3,}|~{3,})/.exec(line)?.[1]
}

function closes(line: string, fence: string): boolean {
	const match = /^\s*(`{3,}|~{3,})\s*$/.exec(line)
	return match !== null && match[1]?.[0] === fence[0] && (match[1]?.length ?? 0) >= fence.length
}

const separator = /^\s*\|(\s*-+\s*\|)+\s*$/

/** How many lines from i make a table: a header, its separator, then the rows that follow at its indent. */
function tableAt(lines: string[], i: number): number {
	const header = lines[i] as string
	const indent = indentOf(header)
	if (!header.trimStart().startsWith('|') || !separator.test(lines[i + 1] ?? '')) {
		return 0
	}
	let end = i + 2
	while (
		end < lines.length &&
		indentOf(lines[end] as string) === indent &&
		lines[end]?.trimStart().startsWith('|')
	) {
		end++
	}
	return end - i
}

function aligned(rows: string[]): string[] {
	const indent = indentOf(rows[0] as string)
	const cells = rows.map(cellsOf)
	const columns = Math.max(...cells.map(row => row.length))
	const widths = Array.from({ length: columns }, (_, column) =>
		Math.max(...cells.map((row, index) => (index === 1 ? 0 : width(row[column] ?? '')))),
	)
	return cells.map((row, index) => {
		if (index === 1) {
			return `${indent}|${widths.map(w => '-'.repeat(w + 2)).join('|')}|`
		}
		const padded = widths.map((w, column) => {
			const cell = row[column] ?? ''
			return cell + ' '.repeat(w - width(cell))
		})
		return `${indent}| ${padded.join(' | ')} |`
	})
}

/** A row's cells, trimmed: what lies between its unescaped pipes. */
function cellsOf(row: string): string[] {
	const cells: string[] = []
	let cell = ''
	const text = row.trim()
	for (let i = 1; i < text.length; i++) {
		const c = text[i] as string
		if (c === '\\' && i + 1 < text.length) {
			cell += c + text[i + 1]
			i++
		} else if (c === '|') {
			cells.push(cell.trim())
			cell = ''
		} else {
			cell += c
		}
	}
	if (cell.trim() !== '') {
		cells.push(cell.trim())
	}
	return cells
}

// the characters a monospaced editor gives a column each
function width(text: string): number {
	return [...text].length
}

function indentOf(line: string): string {
	return /^\s*/.exec(line)?.[0] ?? ''
}

if (import.meta.main) {
	for (const file of alignFiles()) {
		process.stdout.write(`${file}: its tables aligned\n`)
	}
}
