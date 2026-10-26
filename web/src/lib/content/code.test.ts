import { describe, expect, test } from 'bun:test'
import { toHtml } from 'hast-util-to-html'

import { byLanguage, type Fence, linesOf, parseMeta, renderGroup, splitRun, toFence } from './code'
import type { Highlight } from './highlight'

const fence = (lang: string, title?: string, language?: Fence['language']): Fence => ({
	lang,
	code: `${lang} code`,
	...(title === undefined ? {} : { title }),
	...(language === undefined ? {} : { language }),
})

const go = fence('go', undefined, 'go')
const ts = fence('ts', undefined, 'bun')
const python = fence('python', undefined, 'python')
const rust = fence('rust', undefined, 'rust')

// a stand-in that keeps the code readable in the output
const plain: Highlight = (code, lang) => ({
	type: 'element',
	tagName: 'pre',
	properties: { className: ['shiki'], dataLang: lang },
	children: [{ type: 'text', value: code }],
})

describe('fences', () => {
	test('a run in three languages is one block', () => {
		expect(splitRun([go, ts, python])).toEqual([[go, ts, python]])
	})

	test('titled fences of one language are files of one block', () => {
		const files = [
			fence('ts', 'a.ts', 'bun'),
			fence('ts', 'b.ts', 'bun'),
			fence('python', 'a.py', 'python'),
			fence('python', 'b.py', 'python'),
		]
		expect(splitRun(files)).toEqual([files])
	})

	test('two untitled fences of one language stay two blocks', () => {
		expect(splitRun([go, go])).toEqual([[go], [go]])
	})

	test('a shell command joins a language only when it says which', () => {
		const shell = fence('sh')
		expect(splitRun([shell, go])).toEqual([[shell], [go]])

		const forGo = fence('sh', undefined, 'go')
		expect(splitRun([ts, forGo])).toEqual([[ts, forGo]])
	})

	test('plain fences join only as titled files of one language', () => {
		const a = fence('sql', 'a.sql')
		const b = fence('sql', 'b.sql')
		expect(splitRun([a, b])).toEqual([[a, b]])
		expect(splitRun([fence('text'), fence('text')])).toHaveLength(2)
	})

	test('a rust or rs fence is the Rust tab', () => {
		expect(toFence({ type: 'code', lang: 'rs', value: '' }).language).toBe('rust')
		expect(toFence({ type: 'code', lang: 'rust', value: '' }).language).toBe('rust')
	})

	test('an info string names a file, a language and an install command', () => {
		expect(parseMeta(`title="metrics.ts" for=bun install='bun add @tinyshed/tinystore'`)).toEqual({
			title: 'metrics.ts',
			for: 'bun',
			install: 'bun add @tinyshed/tinystore',
		})
	})

	test('languages come in the switch order, whatever order they were written in', () => {
		expect(byLanguage([rust, go, python, ts]).map(variant => variant.label)).toEqual([
			'Bun',
			'Python',
			'Go',
			'Rust',
		])
	})
})

describe('a block', () => {
	test('holds every language, the first the default, each with its own switch and copy', () => {
		const html = toHtml(renderGroup({ type: 'codeGroup', fences: [go, ts] }, plain))

		expect(html).toContain('<div class="code-variant" data-variant="bun" data-default="">')
		expect(html).toContain('<div class="code-variant" data-variant="go">')
		expect(html.match(/class="code-lang"/g)).toHaveLength(4)
		expect(html.match(/data-copy=""/g)).toHaveLength(2)
	})

	test('shows its first file and hides the others until a tab is picked', () => {
		const html = toHtml(
			renderGroup(
				{ type: 'codeGroup', fences: [fence('ts', 'a.ts', 'bun'), fence('ts', 'b.ts', 'bun')] },
				plain,
			),
		)

		expect(html).toContain('<pre class="shiki" data-lang="ts" data-file="0">')
		expect(html).toContain('<pre class="shiki" data-lang="ts" data-file="1" hidden>')
		expect(html).toContain('aria-selected="true">a.ts</button>')
	})

	test('in one language has no switch, and says its language when nothing names it', () => {
		const html = toHtml(renderGroup({ type: 'codeGroup', fences: [fence('sh')] }, plain))

		expect(html).not.toContain('code-lang"')
		expect(html).toContain('<span class="code-title">sh</span>')
		expect(toHtml(renderGroup({ type: 'codeGroup', fences: [fence('text')] }, plain))).toContain(
			'<span class="code-title"></span>',
		)
	})
})

describe('mark=', () => {
	test('names lines, ranges and lists of them, counted from 1', () => {
		expect(linesOf('3')).toEqual([3])
		expect(linesOf('2-4,7')).toEqual([2, 3, 4, 7])
	})

	test('refuses what names no line', () => {
		for (const wrong of ['0', '3-2', 'a', '2-']) {
			expect(() => linesOf(wrong)).toThrow('counted from 1')
		}
	})
})
