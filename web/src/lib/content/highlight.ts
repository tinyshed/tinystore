import githubDark from '@shikijs/themes/github-dark-default'
import githubLight from '@shikijs/themes/github-light-default'
import type { Element } from 'hast'
import {
	createHighlighter,
	type DecorationItem,
	type LanguageRegistration,
	type ShikiTransformer,
	type ThemeRegistration,
} from 'shiki'

import cli from './cli.json'

// what a fence may say, and the grammar it means; anything else is shown as plain text
const grammars: Record<string, string> = {
	go: 'go',
	rust: 'rust',
	rs: 'rust',
	ts: 'typescript',
	typescript: 'typescript',
	js: 'javascript',
	javascript: 'javascript',
	python: 'python',
	py: 'python',
	sh: 'shellscript',
	bash: 'shellscript',
	shell: 'shellscript',
	console: 'shellscript',
	sql: 'sql',
	json: 'json',
	yaml: 'yaml',
	yml: 'yaml',
	toml: 'toml',
	diff: 'diff',
	log: 'tinystore-log',
	dockerfile: 'dockerfile',
	html: 'html',
	css: 'css',
	svelte: 'svelte',
}

/**
 * The lines a logger of the store prints on a terminal, coloured as
 * records/console.go colours them, so that a fence of them reads as it would:
 *
 *     11:02:14.502 WARN  api  slow request  requestId=7f3a ms=1200
 *     dim          yellow cyan              dim=           dim=
 *
 * The time may be in full or left out, and so may the stream, the one word
 * that two spaces follow. A line indented by four spaces, a stack, is dim.
 */
const consoleLog: LanguageRegistration = {
	name: 'tinystore-log',
	scopeName: 'source.tinystore-log',
	repository: {},
	patterns: [
		{
			match:
				'^(?:(\\d{4}-\\d\\d-\\d\\d \\d\\d:\\d\\d:\\d\\d\\.\\d{3} [+-]\\d\\d:\\d\\d|\\d\\d:\\d\\d:\\d\\d\\.\\d{3}) +)?' +
				'(?:(DEBUG\\S*)|(INFO\\S*)|(WARN\\S*)|(ERROR\\S*)|(EVENT))(?: +([^\\s=]+)(?=  |$))?',
			captures: {
				1: { name: 'log.dim' },
				2: { name: 'log.blue' },
				3: { name: 'log.green' },
				4: { name: 'log.yellow' },
				5: { name: 'log.red' },
				6: { name: 'log.magenta' },
				7: { name: 'log.cyan' },
			},
		},
		{ match: '(?<= )(?:"(?:[^"\\\\]|\\\\.)*"|[^\\s="]+)=', name: 'log.dim' },
		{ match: '^    .*$', name: 'log.dim' },
	],
}

/** Each word a command line's command or verb is, read from the help it prints. */
const cliWords = (at: 0 | 1) => [
	...new Set(cli.commands.flatMap(command => command.name.split(' ')[at] ?? [])),
]

/**
 * A `tinystore` command in a shell fence, coloured by what each word is, as
 * the help names them: the command and its verb, a flag, a field's name
 * before its `=` or `:=`, and a path. The words come from `cli.json`, the
 * help the binary prints, so that a command added is coloured at once.
 *
 *     tinystore kv ./data set flags beta on --ttl 1h
 *     function  keyword path keyword        parameter
 */
const commandLine: LanguageRegistration = {
	name: 'tinystore-cli',
	scopeName: 'source.tinystore-cli',
	injectTo: ['source.shell'],
	injectionSelector: 'L:source.shell -comment -string',
	patterns: [{ include: '#command' }],
	repository: {
		command: {
			begin: '(?<![\\w./-])(tinystore)(?=\\s|$)',
			beginCaptures: { 1: { name: 'entity.name.function.tinystore' } },
			// a comment, a pipe or the line's end ends it; a line that ends in a backslash goes on in the next
			end: '(?=[|;&)]|\\s+#|(?<!\\\\)$)',
			patterns: [
				{
					match: `(?<=\\s)(?:${[...cliWords(0), ...cliWords(1)].join('|')})(?=\\s|$)`,
					name: 'keyword.control.tinystore',
				},
				{ match: '(?<=\\s)--?[A-Za-z][\\w-]*', name: 'variable.parameter.tinystore' },
				{
					match: '(?<=\\s)([A-Za-z_][\\w.]*)(:?=)',
					captures: {
						// GitHub's theme colours a property's name, and an editor's look a property
						1: { name: 'variable.other.property.tinystore meta.property-name.tinystore' },
						2: { name: 'keyword.operator.tinystore' },
					},
				},
				{ match: '(?<=\\s)(?:\\.{1,2}|~)?/[^\\s]*', name: 'string.unquoted.path.tinystore' },
				// the shell's strings and variables, and none of its statements, which would take the rest
				{ include: 'source.shell#string' },
				{ include: 'source.shell#variable' },
				{ include: 'source.shell#interpolation' },
			],
		},
	},
}

// the colours console.go writes, as the theme's terminal shows them
const terminalColour = {
	blue: 'terminal.ansiBlue',
	green: 'terminal.ansiGreen',
	yellow: 'terminal.ansiYellow',
	red: 'terminal.ansiRed',
	magenta: 'terminal.ansiMagenta',
	cyan: 'terminal.ansiCyan',
} as const
const consoleColours = Object.keys(terminalColour) as (keyof typeof terminalColour)[]

/**
 * GitHub's palettes, which the design takes its code colours from, with its
 * own text and comment colours over them.
 */
const themes = {
	dark: retint(githubDark, 'tinystore-dark', { text: '#eceaf3', comment: '#7f7c8f' }),
	light: retint(githubLight, 'tinystore-light', { text: '#1f2328', comment: '#6e7781' }),
}

/** A fence as HTML; each decoration wraps its range of the code, as a call is wrapped in its link. */
export type Highlight = (code: string, lang: string, decorations?: DecorationItem[]) => Element

/**
 * A highlighter of the fences the docs use. Each token carries both themes'
 * colours as --shiki-light and --shiki-dark, and the stylesheet picks one, so
 * the page needs no script to highlight and none to change theme.
 */
export async function createHighlight(): Promise<Highlight> {
	const highlighter = await createHighlighter({
		themes: [themes.dark, themes.light, lookTheme],
		// Oniguruma, the grammars' own engine: the JavaScript one tokenized a first fence wrongly
		langs: [
			...new Set(Object.values(grammars).filter(name => name !== consoleLog.name)),
			consoleLog,
			commandLine,
		],
	})

	return (code, lang, decorations = []) => {
		const root = highlighter.codeToHast(code, {
			lang: grammars[lang] ?? 'text',
			themes: { light: 'tinystore-light', dark: 'tinystore-dark', look: 'tinystore-look' },
			defaultColor: false,
			// each theme tokenizes the line again: a slow first compile of a grammar must not cut it short
			tokenizeTimeLimit: 10_000,
			decorations,
			transformers: lang === 'diff' ? [diffLines(code)] : [],
		})
		const pre = root.children[0]
		if (pre?.type !== 'element') {
			throw new Error(`shiki gave no <pre> for a ${lang} fence`)
		}
		// the stylesheet owns the block's background and focus, not the theme
		pre.properties = { className: ['shiki'] }
		return pre
	}
}

/**
 * The code's colours of an editor's look, each token's kind named rather than
 * coloured, `--shiki-look: var(--code-keyword)`, and each look's stylesheet
 * saying what its keywords are: one variable more a token, however many looks
 * there are, where a shiki theme a look would carry every look's colours on
 * every token.
 */
const lookTheme: ThemeRegistration = {
	name: 'tinystore-look',
	type: 'dark',
	colors: {
		'editor.foreground': 'var(--code-foreground)',
		'editor.background': 'var(--code-background)',
	},
	tokenColors: [
		{
			scope: ['comment', 'punctuation.definition.comment'],
			settings: { foreground: 'var(--code-comment)' },
		},
		{ scope: ['keyword', 'storage'], settings: { foreground: 'var(--code-keyword)' } },
		{
			scope: ['string', 'punctuation.definition.string'],
			settings: { foreground: 'var(--code-string)' },
		},
		{
			scope: ['constant', 'support.constant', 'variable.other.constant', 'variable.language'],
			settings: { foreground: 'var(--code-constant)' },
		},
		{
			scope: ['entity.name.function', 'support.function'],
			settings: { foreground: 'var(--code-function)' },
		},
		{
			scope: [
				'entity.name.type',
				'entity.name.class',
				'support.type',
				'support.class',
				'entity.other.inherited-class',
			],
			settings: { foreground: 'var(--code-type)' },
		},
		{ scope: ['variable.parameter'], settings: { foreground: 'var(--code-parameter)' } },
		{
			scope: [
				'variable.other.property',
				'support.variable.property',
				'meta.object-literal.key',
				'entity.name.tag',
			],
			settings: { foreground: 'var(--code-property)' },
		},
		{ scope: ['punctuation', 'meta.brace'], settings: { foreground: 'var(--code-punctuation)' } },
		{ scope: ['markup.inserted'], settings: { foreground: 'var(--code-function)' } },
		{ scope: ['markup.deleted'], settings: { foreground: 'var(--code-keyword)' } },
	],
}

/** A diff's lines marked by their first character, so that the stylesheet tints a whole row. */
function diffLines(code: string): ShikiTransformer {
	const marks: Record<string, string> = { '+': 'diff-add', '-': 'diff-remove' }
	const firsts = code.split('\n').map(line => line[0] ?? '')
	return {
		line(node, number) {
			const mark = marks[firsts[number - 1] ?? '']
			if (mark !== undefined) {
				this.addClassToHast(node, mark)
			}
		},
	}
}

function retint(
	theme: ThemeRegistration,
	name: string,
	colors: { text: string; comment: string },
): ThemeRegistration {
	const text = theme.colors?.['editor.foreground']?.toLowerCase()

	return {
		...theme,
		name,
		colors: { ...theme.colors, 'editor.foreground': colors.text },
		tokenColors: [
			...(theme.tokenColors ?? []).map(rule => {
				if ([rule.scope].flat().includes('comment')) {
					return { ...rule, settings: { ...rule.settings, foreground: colors.comment } }
				}
				// a rule that repeats the theme's text colour follows the design's text
				if (rule.settings.foreground?.toLowerCase() === text) {
					return { ...rule, settings: { ...rule.settings, foreground: colors.text } }
				}
				return rule
			}),
			{ scope: 'log.dim', settings: { foreground: colors.comment } },
			...consoleColours.map(colour => ({
				scope: `log.${colour}`,
				settings: { foreground: theme.colors?.[terminalColour[colour]] },
			})),
		],
	}
}
