// An application's settings, as Go's kv.Config makes them: the defaults, then
// each layer in the order given, then what update kept, which the server
// keeps field by field in kv.db and sends to every store watching the config.
// `value` is read from memory and is new after each change.

import { readFile } from 'node:fs/promises'

import type { Link } from './connection.ts'
import { ClosedError, InvalidError } from './errors.ts'
import { checkName, handleOn } from './handles.ts'
import { check, isSchema, type StandardSchemaV1 } from './schema.ts'
import type { Stream } from './session.ts'
import { KvBucket, KvCall, KvConfigure, KvKept, methods } from './wire/messages.ts'

/** A value of T with any of its fields, at any depth, left out. */
export type DeepPartial<T> = {
	[K in keyof T]?: T[K] extends readonly unknown[]
		? T[K]
		: T[K] extends object
			? DeepPartial<T[K]>
			: T[K]
}

type Kind = 'string' | 'number' | 'boolean'

/** A field of a config's defaults made by `secret` or `required`, in place of its value. */
export class Setting<V> {
	readonly kind: Kind
	/** absent for a field a layer has to give */
	readonly fallback: V | undefined
	readonly secret: boolean
	/** its own variable, read in place of the prefix and its path */
	readonly variable: string | undefined

	constructor(kind: Kind, fallback: V | undefined, secret: boolean, variable: string | undefined) {
		this.kind = kind
		this.fallback = fallback
		this.secret = secret
		this.variable = variable
	}
}

/**
 * A secret: never kept by update, and shown as *** by sources(). It reads
 * its own variable when given one, and is required without a default.
 *
 * ```ts
 * db: { url: secret('DATABASE_URL'), pool: 10 }
 * ```
 */
export function secret(variable?: string, fallback?: string): Setting<string> {
	return new Setting('string', fallback, true, variable)
}

/** A field a layer has to give, or the config does not open: a string unless kind says. */
export function required(): Setting<string>
export function required(kind: StringConstructor): Setting<string>
export function required(kind: NumberConstructor): Setting<number>
export function required(kind: BooleanConstructor): Setting<boolean>
export function required(
	kind: StringConstructor | NumberConstructor | BooleanConstructor = String,
): Setting<string | number | boolean> {
	return new Setting<string | number | boolean>(
		kind === Number ? 'number' : kind === Boolean ? 'boolean' : 'string',
		undefined,
		false,
		undefined,
	)
}

/** The environment as a layer of a config, which `fromEnv` makes. */
export class FromEnv {
	readonly prefix: string
	readonly files: readonly string[]

	constructor(prefix: string, files: readonly string[]) {
		this.prefix = prefix
		this.files = files
	}
}

/**
 * The environment as a config's layer: `fromEnv('APP')` reads db.pool from
 * APP_DB_POOL, and `fromEnv()` from DB_POOL. The .env files given are read
 * first, the process's own variables over them, a missing file skipped.
 */
export function fromEnv(prefix = '', ...files: string[]): FromEnv {
	return new FromEnv(prefix, files)
}

/** A config's check, which `validate` makes. */
export class Validate {
	readonly schema: StandardSchemaV1

	constructor(schema: StandardSchemaV1) {
		this.schema = schema
	}
}

/** Checks a config each time it is made, with zod's, valibot's or another Standard Schema. */
export function validate(schema: StandardSchemaV1): Validate {
	return new Validate(schema)
}

/** A config's value, as its defaults make it: each Setting its value's type. */
export type ConfigValue<D extends object> = { [K in keyof D]: FieldValue<D[K]> }

type FieldValue<F> =
	F extends Setting<infer V>
		? V
		: F extends readonly unknown[] | Date
			? F
			: F extends object
				? ConfigValue<F>
				: F

/** What a config takes after its defaults, each over the one before. */
export type ConfigLayer<V> = DeepPartial<V> | FromEnv | Validate

/** Where a field's value came from. */
export interface Source {
	path: string
	/** its JSON, or *** for a secret */
	value: string
	/** 'default', 'file', 'env NAME' or 'kept' */
	from: string
	/** why a kept value is left out, when it is */
	ignored?: string
}

interface Field {
	/** its default, or '', 0 or false for one without */
	sample: unknown
	secret: boolean
	required: boolean
	variable: string | undefined
}

/**
 * A field's variable: the prefix and its path in upper snake case, as Go's and
 * Python's configs name it.
 *
 *     'APP', 'limits.maxRps' → APP_LIMITS_MAX_RPS
 */
export function envName(prefix: string, path: string): string {
	const chars = [...path]
	let name = ''
	chars.forEach((c, i) => {
		if (c === '.' || c === '-') {
			name += '_'
			return
		}
		const before = chars[i - 1]
		const after = chars[i + 1]
		if (i > 0 && isUpper(c) && before !== undefined) {
			const lowerAfter = after !== undefined && isLower(after)
			if (isLower(before) || isDigit(before) || (isUpper(before) && lowerAfter)) {
				name += '_'
			}
		}
		name += c.toUpperCase()
	})
	return prefix === '' ? name : `${prefix}_${name}`
}

const isUpper = (c: string) => c !== c.toLowerCase() && c === c.toUpperCase()
const isLower = (c: string) => c !== c.toUpperCase() && c === c.toLowerCase()
const isDigit = (c: string) => c >= '0' && c <= '9'

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

/** A config: open it with `store.kv.config(name, defaults, ...layers)`. */
export class Config<T extends object> {
	readonly name: string
	readonly #link: Link
	readonly #open: Uint8Array
	readonly #fields: Map<string, Field>
	readonly #base: Record<string, unknown>
	readonly #from: Map<string, string>
	readonly #schemas: StandardSchemaV1[] = []
	readonly #listeners = new Set<(value: T) => void>()
	/** the last fromEnv's prefix, for a missing field's variable */
	#prefix: string | undefined
	#kept = new Map<string, string>()
	#ignored = new Map<string, string>()
	#value = {} as T
	#stream: Stream | undefined
	#closed = false

	/** Configs come from `store.kv.config`, which lays their layers and waits for the first state. */
	constructor(link: Link, name: string, defaults: object) {
		checkName(name, 'config')
		this.#link = link
		this.name = name
		this.#open = KvBucket.encode({ name, config: true })
		this.#fields = new Map(leavesOf(defaults).map(([path, leaf]) => [path, fieldOf(leaf)]))
		this.#base = valuesOf(defaults as Record<string, unknown>)
		this.#from = new Map(
			[...this.#fields].filter(([, field]) => !field.required).map(([path]) => [path, 'default']),
		)
	}

	/** The config now, frozen: update changes it. */
	get value(): T {
		return this.#value
	}

	/** Lays each layer over the defaults, the one before it under it, and checks what is required. */
	async lay(layers: readonly unknown[]): Promise<void> {
		for (const layer of layers) {
			if (layer instanceof FromEnv) {
				await this.#layEnvironment(layer)
			} else if (layer instanceof Validate) {
				this.#schemas.push(layer.schema)
			} else if (isSchema(layer)) {
				throw new InvalidError(
					`config ${this.name}: a schema checks the config as validate(schema)`,
				)
			} else if (plain(layer)) {
				this.#layFile(layer)
			} else {
				throw new InvalidError(
					`config ${this.name}: a layer is a file's values, fromEnv() or validate(), not ${String(layer)}`,
				)
			}
		}
		for (const [path, field] of this.#fields) {
			if (field.required && missing(pathIn(this.#base, path))) {
				throw new InvalidError(`config ${this.name}: ${this.#requiredText(path, field)}`)
			}
		}
		this.#value = deepFreeze(structuredClone(this.#base)) as T
	}

	/**
	 * Changes the fields change names, at any depth; each one that changed is
	 * checked, kept, and seen by every store watching the config. A change the
	 * schema refuses, one of a secret, or one leaving a required field empty, is
	 * InvalidError and keeps nothing.
	 */
	async update(change: DeepPartial<T>): Promise<void> {
		const next = merged(structuredClone(this.#value) as Record<string, unknown>, change)
		const set: string[] = []
		for (const [path, field] of this.#fields) {
			const now = JSON.stringify(pathIn(next, path))
			if (now === JSON.stringify(pathIn(this.#value, path))) {
				continue
			}
			if (field.secret) {
				throw new InvalidError(
					`config ${this.name}: ${path} is a secret, set by the defaults, a file or the environment alone`,
				)
			}
			if (field.required && missing(pathIn(next, path))) {
				throw new InvalidError(`config ${this.name}: ${path} is required`)
			}
			set.push(path, now)
		}
		if (set.length === 0) {
			return
		}
		await this.#checked(next)
		await this.#send(set, [])
	}

	/** Forgets what update kept for paths, each a field or a group of them; all of it without paths. */
	async reset(...paths: string[]): Promise<void> {
		for (const path of paths) {
			if (![...this.#fields.keys()].some(leaf => under(leaf, path))) {
				throw new InvalidError(`config ${this.name} has no field ${path}`)
			}
		}
		const reset = [...this.#kept.keys()].filter(
			kept => paths.length === 0 || paths.some(path => under(kept, path)),
		)
		if (reset.length > 0) {
			await this.#send([], reset)
		}
	}

	/** Calls fn with the config now and after each change, whoever made it; the function it returns stops that. */
	watch(fn: (value: T) => void): () => void {
		this.#listeners.add(fn)
		fn(this.#value)
		return () => this.#listeners.delete(fn)
	}

	/** Where each field's value came from, and why a kept value is left out. */
	sources(): Source[] {
		return [...this.#fields].map(([path, field]) => {
			const kept = this.#kept.has(path) && !this.#ignored.has(path)
			const source: Source = {
				path,
				value: field.secret ? '***' : (JSON.stringify(pathIn(this.#value, path)) ?? 'null'),
				from: kept ? 'kept' : (this.#from.get(path) ?? 'default'),
			}
			const ignored = this.#ignored.get(path)
			return ignored === undefined ? source : { ...source, ignored }
		})
	}

	/** Follows the config's changes until the store closes; resolves once the first state is read. */
	start(): Promise<void> {
		return new Promise((resolve, reject) => {
			void this.#follow(resolve, reject)
		})
	}

	/** Stops following the config, as the store does when it closes. */
	stop(): void {
		this.#closed = true
		this.#stream?.cancel()
	}

	#layFile(values: Record<string, unknown>): void {
		for (const [path, value] of leavesOf(values)) {
			const field = this.#fields.get(path)
			if (field === undefined) {
				continue
			}
			if (!fits(value, field.sample)) {
				throw new InvalidError(
					`the file's ${path}: ${JSON.stringify(value)} is no ${kindOf(field.sample)}`,
				)
			}
			setPath(this.#base, path, value)
			this.#from.set(path, 'file')
		}
	}

	async #layEnvironment(layer: FromEnv): Promise<void> {
		const variables: Record<string, string | undefined> = {}
		for (const file of layer.files) {
			Object.assign(variables, await readDotenv(file))
		}
		Object.assign(variables, process.env)
		for (const [path, field] of this.#fields) {
			const name = field.variable ?? envName(layer.prefix, path)
			const text = variables[name]
			if (text !== undefined) {
				setPath(this.#base, path, readVariable(text, field.sample, name))
				this.#from.set(path, `env ${name}`)
			}
		}
		this.#prefix = layer.prefix
	}

	#requiredText(path: string, field: Field): string {
		if (this.#prefix === undefined) {
			return `${path} is required`
		}
		return `${path} is required: set ${field.variable ?? envName(this.#prefix, path)}`
	}

	async #follow(ready: () => void, failed: (err: unknown) => void): Promise<void> {
		let first = true
		for (let pause = 100; !this.#closed; pause = Math.min(pause * 2, 5000)) {
			try {
				await this.#link.run('read', async connection => {
					const handle = await handleOn(connection, methods['kv.open'], this.#open)
					const stream = await connection.session.open(
						methods['kv.watch'],
						KvCall.encode({ handle }),
						true,
					)
					this.#stream = stream
					await stream.next()
					for (;;) {
						const event = await stream.next()
						stream.consumed(event.body.length)
						if (event.end) {
							return
						}
						await this.#apply(KvKept.decode(event.body).fields ?? [])
						pause = 100
						if (first) {
							first = false
							ready()
						}
					}
				})
			} catch (err) {
				if (first) {
					failed(err)
					return
				}
				if (this.#closed || err instanceof ClosedError) {
					return
				}
			}
			await new Promise(resolve => setTimeout(resolve, pause))
		}
	}

	/** Keeps set, path and JSON each, and forgets reset, then makes the config with them. */
	async #send(set: string[], reset: string[]): Promise<void> {
		await this.#link.run('write', async connection => {
			const handle = await handleOn(connection, methods['kv.open'], this.#open)
			const body = KvConfigure.encode({
				handle,
				set: set.length > 0 ? set : undefined,
				reset: reset.length > 0 ? reset : undefined,
			})
			await connection.session.call(methods['kv.configure'], body)
		})
		const kept = new Map(this.#kept)
		for (let i = 0; i + 1 < set.length; i += 2) {
			kept.set(set[i] as string, set[i + 1] as string)
		}
		for (const path of reset) {
			kept.delete(path)
		}
		await this.#build(kept)
	}

	/** Takes up the kept fields a watch sent. */
	async #apply(fields: string[]): Promise<void> {
		const kept = new Map<string, string>()
		for (let i = 0; i + 1 < fields.length; i += 2) {
			kept.set(fields[i] as string, fields[i + 1] as string)
		}
		await this.#build(kept)
	}

	/** Makes the config from its base and what is kept, leaving out what no longer fits, and tells the watchers. */
	async #build(kept: Map<string, string>): Promise<void> {
		const ignored = new Map<string, string>()
		let next = withKept(this.#base, this.#fields, kept, ignored)
		let issues = await this.#issues(next)
		if (issues !== undefined && kept.size > ignored.size) {
			for (const path of kept.keys()) {
				ignored.set(path, ignored.get(path) ?? `the config fails with it: ${issues}`)
			}
			next = structuredClone(this.#base)
			issues = await this.#issues(next)
		}
		if (issues !== undefined) {
			throw new InvalidError(`config ${this.name}: ${issues}`)
		}
		const changed = JSON.stringify(next) !== JSON.stringify(this.#value)
		this.#kept = kept
		this.#ignored = ignored
		this.#value = deepFreeze(next) as T
		if (changed) {
			for (const listener of this.#listeners) {
				listener(this.#value)
			}
		}
	}

	async #checked(value: Record<string, unknown>): Promise<void> {
		const issues = await this.#issues(value)
		if (issues !== undefined) {
			throw new InvalidError(`config ${this.name}: ${issues}`)
		}
	}

	async #issues(value: Record<string, unknown>): Promise<string | undefined> {
		for (const schema of this.#schemas) {
			const checked = await check(schema, value)
			if ('issues' in checked) {
				return checked.issues
			}
		}
		return undefined
	}
}

function fieldOf(leaf: unknown): Field {
	if (!(leaf instanceof Setting)) {
		return { sample: leaf, secret: false, required: false, variable: undefined }
	}
	return {
		sample: leaf.fallback ?? { string: '', number: 0, boolean: false }[leaf.kind],
		secret: leaf.secret,
		required: leaf.fallback === undefined,
		variable: leaf.variable,
	}
}

/** The defaults' values: a Setting's default, and nothing for one without. */
function valuesOf(tree: Record<string, unknown>): Record<string, unknown> {
	const values: Record<string, unknown> = {}
	for (const [key, inner] of Object.entries(tree)) {
		if (inner instanceof Setting) {
			if (inner.fallback !== undefined) {
				values[key] = inner.fallback
			}
		} else {
			values[key] = plain(inner) ? valuesOf(inner) : structuredClone(inner)
		}
	}
	return values
}

/** A required field's value no layer gave, or gave empty. */
function missing(value: unknown): boolean {
	return value === undefined || value === ''
}

async function readDotenv(path: string): Promise<Record<string, string>> {
	let text: string
	try {
		text = await readFile(path, 'utf8')
	} catch (err) {
		if ((err as NodeJS.ErrnoException).code === 'ENOENT') {
			return {}
		}
		throw err
	}
	try {
		return parseDotenv(text)
	} catch (err) {
		throw new InvalidError(`${path}: ${(err as Error).message}`)
	}
}

/** The base with each kept value that fits its field over it; a secret is never taken from what was kept. */
function withKept(
	base: Record<string, unknown>,
	fields: Map<string, Field>,
	kept: Map<string, string>,
	ignored: Map<string, string>,
): Record<string, unknown> {
	const next = structuredClone(base)
	for (const [path, spelled] of kept) {
		const field = fields.get(path)
		if (field === undefined) {
			ignored.set(path, 'the config has no such field')
			continue
		}
		if (field.secret) {
			ignored.set(path, 'the field is a secret')
			continue
		}
		let value: unknown
		try {
			value = JSON.parse(spelled)
		} catch {
			ignored.set(path, `${spelled} is no JSON`)
			continue
		}
		if (!fits(value, field.sample)) {
			ignored.set(path, `${spelled} is no ${kindOf(field.sample)}`)
			continue
		}
		if (field.required && missing(value)) {
			ignored.set(path, 'the field is required')
			continue
		}
		setPath(next, path, value)
	}
	return next
}

/**
 * A variable's text as a value of its default's kind: a number, true or
 * false, a list split at its commas or given as JSON, and anything else as JSON.
 *
 *     8080 ← '3000' → 3000, ['x'] ← 'a.com, b.com' → ['a.com', 'b.com']
 */
function readVariable(text: string, wanted: unknown, name: string): unknown {
	const trimmed = text.trim()
	if (typeof wanted === 'string') {
		return text
	}
	if (typeof wanted === 'number') {
		const n = Number(trimmed)
		if (trimmed === '' || !Number.isFinite(n)) {
			throw new InvalidError(`${name}: ${JSON.stringify(text)} is no number`)
		}
		return n
	}
	if (typeof wanted === 'boolean') {
		if (['1', 't', 'T', 'TRUE', 'true', 'True'].includes(trimmed)) {
			return true
		}
		if (['0', 'f', 'F', 'FALSE', 'false', 'False'].includes(trimmed)) {
			return false
		}
		throw new InvalidError(`${name}: ${JSON.stringify(text)} is not true or false`)
	}
	if (Array.isArray(wanted) && !trimmed.startsWith('[')) {
		const item = wanted[0] ?? ''
		return trimmed === ''
			? []
			: trimmed.split(',').map(part => readVariable(part.trim(), item, name))
	}
	try {
		const value: unknown = JSON.parse(trimmed)
		if (fits(value, wanted)) {
			return value
		}
	} catch {
		// refused below, naming the variable
	}
	throw new InvalidError(`${name}: ${JSON.stringify(text)} is no ${kindOf(wanted)}`)
}

/** A value's fields to the last that is not a plain object, by their dotted paths. */
function leavesOf(value: unknown, parent = ''): [string, unknown][] {
	const leaves: [string, unknown][] = []
	for (const [key, inner] of Object.entries(value as Record<string, unknown>)) {
		const path = parent === '' ? key : `${parent}.${key}`
		if (plain(inner)) {
			leaves.push(...leavesOf(inner, path))
		} else {
			leaves.push([path, inner])
		}
	}
	return leaves
}

function plain(value: unknown): value is Record<string, unknown> {
	return (
		typeof value === 'object' &&
		value !== null &&
		!Array.isArray(value) &&
		!(value instanceof Date) &&
		!(value instanceof Setting) &&
		!(value instanceof FromEnv) &&
		!(value instanceof Validate)
	)
}

/** A value fits a field when it is of its default's kind; a default of null takes anything. */
function fits(value: unknown, wanted: unknown): boolean {
	if (wanted === null || wanted === undefined) {
		return true
	}
	if (Array.isArray(wanted)) {
		return Array.isArray(value)
	}
	return typeof value === typeof wanted && (typeof wanted !== 'object' || plain(value))
}

function kindOf(wanted: unknown): string {
	return Array.isArray(wanted) ? 'list' : typeof wanted
}

function pathIn(tree: unknown, path: string): unknown {
	let at = tree
	for (const name of path.split('.')) {
		if (!plain(at)) {
			return undefined
		}
		at = at[name]
	}
	return at
}

function setPath(tree: Record<string, unknown>, path: string, value: unknown): void {
	const names = path.split('.')
	let at = tree
	for (const name of names.slice(0, -1)) {
		const inner = at[name]
		if (!plain(inner)) {
			at[name] = {}
		}
		at = at[name] as Record<string, unknown>
	}
	at[names[names.length - 1] as string] = value
}

/** change merged into tree at every depth; a list is replaced whole. */
function merged(tree: Record<string, unknown>, change: unknown): Record<string, unknown> {
	for (const [key, value] of Object.entries(change as Record<string, unknown>)) {
		const inner = tree[key]
		tree[key] = plain(value) && plain(inner) ? merged(inner, value) : value
	}
	return tree
}

function under(path: string, given: string): boolean {
	return path === given || path.startsWith(`${given}.`)
}

function deepFreeze<V>(value: V): V {
	if (typeof value === 'object' && value !== null) {
		for (const inner of Object.values(value)) {
			deepFreeze(inner)
		}
		Object.freeze(value)
	}
	return value
}
