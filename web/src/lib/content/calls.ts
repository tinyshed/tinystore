import { readFileSync } from 'node:fs'
import { join, posix } from 'node:path'
import GithubSlugger from 'github-slugger'

import { type ApiLanguage, apiEngines, apiFile, apiLanguages } from './api'
import type { Highlight } from './highlight'

/**
 * The calls of a guide's code blocks, each found in the API reference so
 * that the site can open its signature on a click, and an example cannot
 * call what the API does not have.
 *
 * A call is resolved only where its receiver's type is known, because a bare
 * `get` belongs to a dozen types and a wrong signature is worse than none:
 *
 *     const sessions = store.bucket<Session>('sessions')   // store is a Store, so bucket is Store.bucket,
 *     await sessions.get('k7q2')                            // which returns a Bucket: Bucket.get
 *
 * `store` is a `Store` on every page, as the docs name it. A variable takes
 * the type its assignment's chain returns, and keeps it for the page's later
 * blocks of the same language, since a page opens its handles once. In Rust a
 * type's associated function, `Store::open`, and in Go a package's function,
 * `kv.OpenBucket`, start a chain too. A method the reference does not list on
 * a type it knows is a problem that fails the build.
 */

export interface ApiEntry {
	/** as the reference's heading writes it: `Bucket.get`, `kv.OpenBucket`, `open` */
	name: string
	code: string
	/** the doc comment's first paragraph */
	doc: string
	file: string
	anchor: string
	owner?: string
	/** a field's type as its declaration writes it */
	fieldType?: string
}

interface LanguageIndex {
	/** a type's methods and fields by name */
	types: Map<string, Map<string, ApiEntry>>
	/** functions and types by the name a program calls them by: `open`, `kv.OpenBucket` */
	top: Map<string, ApiEntry>
	/** Go's packages, `kv`, `sqldb`, `tinystore` */
	packages: Set<string>
	/** a Rust type that derefs to another, whose methods it has too: `Table` to `Query` */
	derefs: Map<string, string>
}

export type ApiIndex = Record<ApiLanguage, LanguageIndex>

export interface CallMark {
	start: number
	end: number
	entry: ApiEntry
}

const fenceLanguages: Record<string, ApiLanguage> = {
	ts: 'bun',
	typescript: 'bun',
	js: 'bun',
	javascript: 'bun',
	python: 'python',
	py: 'python',
	go: 'go',
	rust: 'rust',
	rs: 'rust',
}

export function apiLanguageOf(fence: string): ApiLanguage | undefined {
	return fenceLanguages[fence]
}

// --- the index, read back from the generated pages ---------------------------

export function readApiIndex(root: string): ApiIndex {
	const index = Object.fromEntries(
		apiLanguages.map(language => [
			language,
			{ types: new Map(), top: new Map(), packages: new Set<string>(), derefs: new Map() },
		]),
	) as ApiIndex
	for (const language of apiLanguages) {
		for (const engine of apiEngines) {
			const file = apiFile(language, engine.slug)
			let text: string
			try {
				text = readFileSync(join(root, file), 'utf8')
			} catch {
				continue
			}
			addPage(index[language], language, file, text)
		}
	}
	return index
}

/** One generated page's headings: `## Item` and `### Item.member`, each with its code and doc. */
export function addPage(
	index: LanguageIndex,
	language: ApiLanguage,
	file: string,
	text: string,
): void {
	const slugger = new GithubSlugger()
	const pkg = language === 'go' ? goPackageOf(text) : undefined
	if (pkg !== undefined) {
		index.packages.add(pkg)
	}
	const lines = text.split(/\r?\n/)
	let fenced = false
	for (let at = 0; at < lines.length; at++) {
		const line = lines[at] ?? ''
		if (line.startsWith('```')) {
			fenced = !fenced
			continue
		}
		const heading = fenced ? null : /^(##|###) (\S+)$/.exec(line)
		if (heading === null) {
			if (!fenced && /^#{1,6} /.test(line)) {
				slugger.slug(line.replace(/^#+ /, ''))
			}
			continue
		}
		const name = heading[2] ?? ''
		const { code, doc } = bodyAfter(lines, at + 1)
		const entry: ApiEntry = { name, code, doc, file, anchor: slugger.slug(name) }
		add(index, language, entry, pkg)
	}
}

function add(
	index: LanguageIndex,
	language: ApiLanguage,
	entry: ApiEntry,
	pkg: string | undefined,
): void {
	const dot = entry.name.lastIndexOf('.')
	const qualified =
		pkg !== undefined && !entry.name.includes('.') ? `${pkg}.${entry.name}` : entry.name
	if (dot === -1 || (language === 'go' && /^[a-z]/.test(entry.name))) {
		const name = language === 'go' ? qualified : shortName(entry.name)
		index.top.set(name, entry)
		const target = baseOf(entry.code, language)
		if (target !== undefined) {
			index.derefs.set(shortName(entry.name), shortName(target))
		}
		for (const variant of variantsOf(entry.code, language)) {
			member(index, shortName(entry.name), { ...entry, name: `${entry.name}.${variant}` })
		}
		if (language === 'go' && pkg !== undefined) {
			for (const constant of groupedConstants(entry.code)) {
				index.top.set(`${pkg}.${constant}`, entry)
			}
		}
		for (const field of fieldsOf(entry, language)) {
			member(index, shortName(entry.name), field)
		}
		return
	}
	const owner = shortName(entry.name.slice(0, dot))
	member(index, owner, { ...entry, owner })
}

function member(index: LanguageIndex, owner: string, entry: ApiEntry): void {
	const members = index.types.get(owner) ?? new Map<string, ApiEntry>()
	index.types.set(owner, members)
	const name = entry.name.slice(entry.name.lastIndexOf('.') + 1)
	if (!members.has(name)) {
		members.set(name, { ...entry, owner })
	}
}

/**
 * What a type has every method of: Rust's `impl Deref<Target = Query>`,
 * Python's `class Table(Query[T])`, Go's embedded `*Query[T]`.
 */
function baseOf(code: string, language: ApiLanguage): string | undefined {
	const found =
		language === 'rust'
			? /^impl Deref<Target = ([\w:]+)/m.exec(code)
			: language === 'python'
				? /^class \w+(?:\[[^\]]*\])?\((\w+)/m.exec(code)
				: language === 'go'
					? /^type \w+(?:\[[^\]]*\])? struct \{\n\t\*?(\w+)(?:\[[^\]]*\])?\n/m.exec(code)
					: null
	return found?.[1]
}

// a Rust enum's variants, `ConstraintKind::ForeignKey`, are called as its members
function variantsOf(code: string, language: ApiLanguage): string[] {
	if (language !== 'rust' || !code.startsWith('pub enum ')) {
		return []
	}
	return [...code.matchAll(/^ {4}([A-Z]\w*)/gm)].map(match => match[1] ?? '')
}

// every name of a Go `const ( … )` block, which the reference lists under its first
function groupedConstants(code: string): string[] {
	if (!code.startsWith('const (')) {
		return []
	}
	return [...code.matchAll(/^\t([A-Z]\w*)\b/gm)].map(match => match[1] ?? '')
}

// `memory::Budget` is `Budget` to a program that imported it
function shortName(name: string): string {
	return name.split('::').at(-1) ?? name
}

function bodyAfter(lines: string[], from: number): { code: string; doc: string } {
	let at = from
	while (at < lines.length && (lines[at] ?? '').trim() === '') {
		at++
	}
	const code: string[] = []
	if ((lines[at] ?? '').startsWith('```')) {
		for (at++; at < lines.length && !(lines[at] ?? '').startsWith('```'); at++) {
			code.push(lines[at] ?? '')
		}
		at++
	}
	while (at < lines.length && (lines[at] ?? '').trim() === '') {
		at++
	}
	const doc: string[] = []
	for (; at < lines.length; at++) {
		const line = lines[at] ?? ''
		if (
			line.trim() === '' ||
			line.startsWith('#') ||
			line.startsWith('```') ||
			line.startsWith('<!--')
		) {
			break
		}
		doc.push(line)
	}
	return { code: code.join('\n'), doc: doc.join(' ') }
}

// the page's lead names its packages: in `tinyshed.org/tinystore/kv`, the root's `tinyshed.org/tinystore`
function goPackageOf(text: string): string | undefined {
	const path = /`tinyshed\.org\/tinystore(\/[\w/]+)?`/.exec(text)
	if (path === null) {
		return undefined
	}
	return path[1] === undefined ? 'tinystore' : path[1].slice(path[1].lastIndexOf('/') + 1)
}

/** The fields a type's declaration lists, each a member whose type is what follows its name. */
function fieldsOf(entry: ApiEntry, language: ApiLanguage): ApiEntry[] {
	const pattern: Record<ApiLanguage, RegExp> = {
		bun: /^\s+(?:readonly\s+)?(\w+)\??:\s*(.+?);?$/,
		python: /^\s+(\w+):\s*(.+)$/,
		rust: /^\s+pub (\w+): (.+),$/,
		go: /^\t(\w+)\s+([*\w.[\]]+)/,
	}
	return entry.code
		.split('\n')
		.slice(1)
		.flatMap(line => {
			const match = pattern[language].exec(line)
			if (match === null) {
				return []
			}
			return [
				{
					...entry,
					name: `${entry.name}.${match[1]}`,
					code: line.trim(),
					doc: '',
					fieldType: match[2],
				},
			]
		})
}

// --- what a call returns ------------------------------------------------------

/** The type a call or a field gives back, by its name: `Bucket` for `Promise<Bucket<T>>`. */
export function returnType(entry: ApiEntry, language: ApiLanguage): string | undefined {
	const written = entry.fieldType ?? resultOf(firstLine(entry.code, language), language)
	if (written === undefined) {
		return undefined
	}
	let type = written.trim().replace(/^["']|["']$/g, '')
	for (;;) {
		const unwrapped =
			/^(?:Promise|Result|Option|Awaitable|AsyncIterator|Box|Arc)\s*[<[]\s*(.*)[>\]]$/s.exec(type)
		if (unwrapped === null) {
			break
		}
		type = (unwrapped[1] ?? '').split(/,(?![^<[]*[>\]])/)[0]?.trim() ?? ''
	}
	type = type.replace(/^&(?:mut\s+)?/, '').replace(/^\*/, '')
	const name = /^[\w.:]+/.exec(type)?.[0]
	if (name === undefined) {
		return undefined
	}
	const short = name.split(/\.|::/).at(-1) ?? name
	return short === 'Self' || short === 'this' ? entry.owner : short
}

// an overloaded method lists every form, and the first is the common one
function firstLine(code: string, language: ApiLanguage): string {
	if (language === 'bun') {
		return code.split('\n')[0] ?? ''
	}
	if (language === 'python') {
		const defs = code
			.split('\n')
			.filter(line => !line.startsWith('@'))
			.join('\n')
		const second = defs.indexOf('\ndef ', 1)
		return second === -1 ? defs : defs.slice(0, second)
	}
	return code
}

function resultOf(code: string, language: ApiLanguage): string | undefined {
	const open = paramsStart(code, language)
	if (open === -1) {
		return undefined
	}
	const close = matching(code, open)
	const rest = code.slice(close + 1)
	switch (language) {
		case 'rust':
			return /^\s*->\s*([\s\S]+?)(?:\nwhere|$)/.exec(rest)?.[1]
		case 'python':
			return /^\s*->\s*([^\n]+?)\s*:/.exec(rest)?.[1]
		case 'bun':
			return /^\s*:\s*(.+)$/.exec(rest)?.[1]
		case 'go': {
			const results = rest.trim()
			if (results.startsWith('(')) {
				return results.slice(1, matching(results, 0)).split(',')[0]
			}
			return results === '' ? undefined : results
		}
	}
}

// the parameters' opening paren: after a Go receiver and the name, past generics
function paramsStart(code: string, language: ApiLanguage): number {
	let at = 0
	if (language === 'go' && code.startsWith('func (')) {
		at = matching(code, 5) + 1
	}
	for (; at < code.length; at++) {
		const c = code[at]
		if (c === '<' || (c === '[' && (language === 'go' || language === 'python'))) {
			at = matching(code, at)
		} else if (c === '(') {
			return at
		}
	}
	return -1
}

function matching(text: string, open: number): number {
	const pairs: Record<string, string> = { '(': ')', '[': ']', '<': '>', '{': '}' }
	const opener = text[open] ?? ''
	const closer = pairs[opener]
	let depth = 0
	for (let at = open; at < text.length; at++) {
		if (text[at] === opener) {
			depth++
		} else if (text[at] === closer && !(closer === '>' && /[-=]/.test(text[at - 1] ?? ''))) {
			depth--
			if (depth === 0) {
				return at
			}
		}
	}
	return text.length
}

// --- the calls of one block ------------------------------------------------------

/** What a page knows of its variables, by language, across its blocks. */
export type Scope = Map<ApiLanguage, Map<string, string>>

type Receiver =
	| { kind: 'type'; name: string }
	| { kind: 'static'; name: string }
	| { kind: 'package'; name: string }

export interface BlockCalls {
	marks: CallMark[]
	problems: { line: number; message: string }[]
}

export function findCalls(
	code: string,
	language: ApiLanguage,
	index: ApiIndex,
	scope: Scope,
): BlockCalls {
	const api = index[language]
	const variables = scope.get(language) ?? new Map<string, string>([['store', 'Store']])
	scope.set(language, variables)

	const text = mask(code, language)
	const out: BlockCalls = { marks: [], problems: [] }
	const head = /[A-Za-z_]\w*/g
	for (let match = head.exec(text); match !== null; match = head.exec(text)) {
		const before = text.slice(0, match.index).trimEnd()
		if (/(\.|::)$/.test(before) || /\w$/.test(text.slice(0, match.index))) {
			continue
		}
		const chain = walk(text, match.index + match[0].length, match[0], language, api, variables, out)
		const target = assigned(text.slice(0, match.index), language)
		if (target !== undefined && chain.type !== undefined) {
			variables.set(target, chain.type)
		}
	}
	return out
}

function walk(
	text: string,
	from: number,
	name: string,
	language: ApiLanguage,
	api: LanguageIndex,
	variables: Map<string, string>,
	out: BlockCalls,
): { end: number; type: string | undefined } {
	let receiver = start(text, from, name, language, api, variables, out)
	let at = skipCall(text, from, language)
	for (;;) {
		const step = /^(\s*\n\s*|\s*)(\.|::|\?\.)\s*([A-Za-z_]\w*)/.exec(text.slice(at))
		if (step === null || receiver === undefined || step[3] === 'await') {
			if (step?.[3] === 'await') {
				at += step[0].length
				continue
			}
			break
		}
		const nameAt = at + step[0].length - (step[3] ?? '').length
		receiver = follow(receiver, step[3] ?? '', nameAt, text, language, api, out)
		at = skipCall(text, at + step[0].length, language)
	}
	return { end: at, type: receiver?.kind === 'type' ? receiver.name : undefined }
}

function start(
	text: string,
	from: number,
	name: string,
	language: ApiLanguage,
	api: LanguageIndex,
	variables: Map<string, string>,
	out: BlockCalls,
): Receiver | undefined {
	const known = variables.get(name)
	if (known !== undefined) {
		return { kind: 'type', name: known }
	}
	const next = text.slice(from).trimStart()
	if (language === 'rust' && next.startsWith('::') && api.types.has(name)) {
		return { kind: 'static', name }
	}
	if (language === 'go' && next.startsWith('.') && api.packages.has(name)) {
		return { kind: 'package', name }
	}
	const top = api.top.get(name)
	if (top !== undefined && language !== 'go' && /^[<(`]/.test(next) && isFunction(top.code)) {
		out.marks.push({ start: from - name.length, end: from, entry: top })
		const type = returnType(top, language)
		return type === undefined ? undefined : { kind: 'type', name: type }
	}
	return undefined
}

// a type's member, or the member of what it derefs to
function memberOf(api: LanguageIndex, type: string, name: string): ApiEntry | undefined {
	for (let at: string | undefined = type, hops = 0; at !== undefined && hops < 4; hops++) {
		const found = api.types.get(at)?.get(name)
		if (found !== undefined) {
			return found
		}
		at = api.derefs.get(at)
	}
	return undefined
}

// what a Rust type has from a trait it derives, which rustdoc lists apart from its methods
const derived = new Set(['default', 'clone', 'into', 'from', 'to_owned', 'to_string'])

// a function a program calls by its name alone, `open('./data')`, and not a type
function isFunction(code: string): boolean {
	return /^(declare )?(async )?(function|def|pub (async )?fn) /.test(code)
}

function follow(
	receiver: Receiver,
	name: string,
	at: number,
	text: string,
	language: ApiLanguage,
	api: LanguageIndex,
	out: BlockCalls,
): Receiver | undefined {
	const entry =
		receiver.kind === 'package'
			? api.top.get(`${receiver.name}.${name}`)
			: memberOf(api, receiver.name, name)
	if (entry === undefined) {
		const known = receiver.kind === 'package' || api.types.has(receiver.name)
		if (known && !(language === 'rust' && derived.has(name))) {
			const owner = receiver.kind === 'package' ? `package ${receiver.name}` : receiver.name
			out.problems.push({
				line: text.slice(0, at).split('\n').length,
				message: `${name}: ${owner} has no ${name} in the ${language} API reference`,
			})
		}
		return undefined
	}
	out.marks.push({ start: at, end: at + name.length, entry })
	const type = returnType(entry, language)
	return type === undefined ? undefined : { kind: 'type', name: type }
}

/** Past a call's generics and arguments, Rust's `?` and a TypeScript `!`, from just after a name. */
function skipCall(text: string, from: number, language: ApiLanguage): number {
	let at = from
	if (language === 'rust' && text.startsWith('::<', at)) {
		at = matching(text, at + 2) + 1
	} else if ((language === 'bun' && text[at] === '<') || (language === 'go' && text[at] === '[')) {
		const close = matching(text, at)
		if (text[close + 1] === '(' || text[close + 1] === '`') {
			at = close + 1
		}
	}
	if (text[at] === '(') {
		at = matching(text, at) + 1
	} else if (language === 'bun' && text[at] === '`') {
		at = text.indexOf('`', at + 1) + 1
	}
	while (text[at] === '?' || text[at] === '!') {
		at++
	}
	return at
}

// the variable a statement assigns its chain to, from the text before the chain
function assigned(before: string, language: ApiLanguage): string | undefined {
	const line = before.slice(before.lastIndexOf('\n') + 1)
	const patterns: Record<ApiLanguage, RegExp> = {
		rust: /let\s+(?:mut\s+)?(\w+)\s*(?::[^=]*)?=\s*&?$/,
		bun: /(?:const|let|var|using)\s+(\w+)\s*(?::[^=]*)?=\s*(?:await\s+)?$/,
		go: /^\s*(\w+)(?:\s*,\s*\w+)*\s*:?=\s*&?$/,
		python: /^\s*(\w+)\s*(?::[^=]*)?=\s*(?:await\s+)?$/,
	}
	return patterns[language].exec(line)?.[1]
}

/**
 * The code with its comments and strings blanked, every offset and line kept,
 * so that a call written in a comment or a string is not one.
 */
export function mask(code: string, language: ApiLanguage): string {
	const out = code.split('')
	const lineComment = language === 'python' ? '#' : '//'
	const quotes = language === 'rust' ? ['"'] : language === 'go' ? ['"', '`'] : ['"', "'", '`']
	const blank = (from: number, to: number) => {
		for (let at = from; at < to; at++) {
			if (out[at] !== '\n') {
				out[at] = ' '
			}
		}
	}
	let at = 0
	while (at < code.length) {
		if (code.startsWith(lineComment, at)) {
			const end = code.indexOf('\n', at)
			const stop = end === -1 ? code.length : end
			blank(at, stop)
			at = stop
		} else if (language !== 'python' && code.startsWith('/*', at)) {
			const end = code.indexOf('*/', at + 2)
			const stop = end === -1 ? code.length : end + 2
			blank(at, stop)
			at = stop
		} else if (quotes.includes(code[at] ?? '')) {
			const quote = code.startsWith('"""', at) ? '"""' : (code[at] ?? '')
			let end = at + quote.length
			while (end < code.length && !code.startsWith(quote, end)) {
				end += code[end] === '\\' && quote !== '`' ? 2 : 1
			}
			// a template keeps its quotes, so that sql`…` still reads as a call
			blank(at + quote.length, end)
			at = end + quote.length
		} else {
			at++
		}
	}
	return out.join('')
}

// --- a page's blocks ------------------------------------------------------------

/**
 * A page's highlighter that links each call of its blocks to the reference,
 * and the problems its blocks met: a call is an `a.api` whose link the build
 * checks like any other, carrying its signature and doc for the popover.
 */
export function pageCalls(
	index: ApiIndex,
	file: string,
	highlight: Highlight,
): { highlight: Highlight; problems: string[] } {
	const scope: Scope = new Map()
	const problems: string[] = []
	const wrapped: Highlight = (code, lang) => {
		const language = apiLanguageOf(lang)
		if (language === undefined) {
			return highlight(code, lang)
		}
		const found = findCalls(code, language, index, scope)
		for (const problem of found.problems) {
			problems.push(`a ${lang} block's line ${problem.line}: ${problem.message}`)
		}
		return highlight(
			code,
			lang,
			found.marks.map(mark => ({
				start: mark.start,
				end: mark.end,
				tagName: 'a',
				alwaysWrap: true,
				properties: {
					className: ['api'],
					href: `${posix.relative(posix.dirname(file), mark.entry.file)}#${mark.entry.anchor}`,
					...signature(mark.entry, language),
					dataDoc: mark.entry.doc,
					dataName: mark.entry.name,
					dataPage: mark.entry.file.replace(/^docs\/reference\//, '').replace(/\.md$/, ''),
				},
			})),
		)
	}
	return { highlight: wrapped, problems }
}

/**
 * The form of a call the popover shows: an overloaded method's first, and how
 * many more the reference lists. Python's forms lose their `@overload` and
 * their bodies, TypeScript's are one a line.
 */
export function signature(
	entry: ApiEntry,
	language: ApiLanguage,
): { dataSig: string; dataForms?: number } {
	if (language === 'bun') {
		const [first = '', ...rest] = entry.code.split('\n')
		return { dataSig: first, ...(rest.length > 0 ? { dataForms: rest.length } : {}) }
	}
	if (language === 'python') {
		const forms = entry.code
			.split('\n')
			.filter(line => line.trim() !== '@overload')
			.join('\n')
			.split(/\n(?=(?:async )?def )/)
			.map(form => form.replace(/:\s*\.\.\.\s*$/, ''))
		return {
			dataSig: forms[0] ?? '',
			...(forms.length > 1 ? { dataForms: forms.length - 1 } : {}),
		}
	}
	return { dataSig: entry.code }
}
