// An application's settings, as Go's kv.Config makes them: the defaults, then
// a file's values, then the environment, then what update kept, which the
// server keeps field by field in kv.db and sends to every store watching the
// config. `value` is read from memory and is new after each change.

import type { Link } from './connection.ts'
import { ClosedError, InvalidError } from './errors.ts'
import { checkName, handleOn } from './handles.ts'
import { check, type StandardSchemaV1 } from './schema.ts'
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

export interface ConfigOptions<T> {
	/** a file's values over the defaults, as Bun reads YAML, JSON or TOML: `import file from './config.yaml'` */
	file?: DeepPartial<T>
	/** the variables' prefix: 'APP' reads APP_PORT; none when absent */
	prefix?: string
	/** a field's own variable, by its path: `{ dbUrl: 'DATABASE_URL' }`; false reads none */
	env?: Record<string, string> | false
	/** paths set by the defaults, the file or the environment alone: never kept, shown as *** */
	secret?: readonly string[]
	/** checks the config each time it is made: zod's, valibot's or another Standard Schema */
	schema?: StandardSchemaV1<unknown, T>
}

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

/** A config: open it with `store.kv.config(name, defaults, options)`. */
export class Config<T extends object> {
	readonly name: string
	readonly #link: Link
	readonly #open: Uint8Array
	readonly #leaves: Map<string, unknown>
	readonly #base: Record<string, unknown>
	readonly #from: Map<string, string>
	readonly #secret: Set<string>
	readonly #schema: StandardSchemaV1<unknown, T> | undefined
	readonly #listeners = new Set<(value: T) => void>()
	#kept = new Map<string, string>()
	#ignored = new Map<string, string>()
	#value: T
	#stream: Stream | undefined
	#closed = false

	/** Configs come from `store.kv.config`, which waits for the first state. */
	constructor(link: Link, name: string, defaults: T, options: ConfigOptions<T>) {
		checkName(name, 'config')
		this.#link = link
		this.name = name
		this.#open = KvBucket.encode({ name, config: true })
		this.#leaves = new Map(leavesOf(defaults))
		this.#schema = options.schema
		this.#secret = new Set(options.secret ?? [])
		for (const path of this.#secret) {
			this.#leaf(path)
		}
		this.#from = new Map([...this.#leaves.keys()].map(path => [path, 'default']))
		this.#base = layered(this.#leaves, defaults, options, this.#from)
		this.#value = deepFreeze(structuredClone(this.#base)) as T
	}

	/** The config now, frozen: update changes it. */
	get value(): T {
		return this.#value
	}

	/**
	 * Changes the fields change names, at any depth; each one that changed is
	 * checked, kept, and seen by every store watching the config. A change the
	 * schema refuses, or one of a secret, is InvalidError and keeps nothing.
	 */
	async update(change: DeepPartial<T>): Promise<void> {
		const next = merged(structuredClone(this.#value) as Record<string, unknown>, change)
		const set: string[] = []
		for (const path of this.#leaves.keys()) {
			const now = JSON.stringify(pathIn(next, path))
			if (now === JSON.stringify(pathIn(this.#value, path))) {
				continue
			}
			if (this.#secret.has(path)) {
				throw new InvalidError(
					`config ${this.name}: ${path} is a secret, set by the defaults, the file or the environment alone`,
				)
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
			if (![...this.#leaves.keys()].some(leaf => under(leaf, path))) {
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
		return [...this.#leaves.keys()].map(path => {
			const kept = this.#kept.has(path) && !this.#ignored.has(path)
			const source: Source = {
				path,
				value: this.#secret.has(path)
					? '***'
					: (JSON.stringify(pathIn(this.#value, path)) ?? 'null'),
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
		let next = withKept(this.#base, this.#leaves, kept, ignored)
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
		if (this.#schema === undefined) {
			return undefined
		}
		const checked = await check(this.#schema, value)
		return 'issues' in checked ? checked.issues : undefined
	}

	#leaf(path: string): unknown {
		if (!this.#leaves.has(path)) {
			throw new InvalidError(`config ${this.name} has no field ${path}`)
		}
		return this.#leaves.get(path)
	}
}

/** The defaults, the file and the environment, each over the one before. */
function layered<T>(
	leaves: Map<string, unknown>,
	defaults: T,
	options: ConfigOptions<T>,
	from: Map<string, string>,
): Record<string, unknown> {
	const base = structuredClone(defaults) as Record<string, unknown>
	if (options.file !== undefined) {
		for (const [path, value] of leavesOf(options.file)) {
			const wanted = leaves.get(path)
			if (!leaves.has(path)) {
				continue
			}
			if (!fits(value, wanted)) {
				throw new InvalidError(
					`the file's ${path}: ${JSON.stringify(value)} is no ${kindOf(wanted)}`,
				)
			}
			setPath(base, path, value)
			from.set(path, 'file')
		}
	}
	if (options.env === false) {
		return base
	}
	for (const [path, wanted] of leaves) {
		const name = options.env?.[path] ?? envName(options.prefix ?? '', path)
		const text = process.env[name]
		if (text !== undefined) {
			setPath(base, path, fromEnv(text, wanted, name))
			from.set(path, `env ${name}`)
		}
	}
	return base
}

/** The base with each kept value that fits its field over it. */
function withKept(
	base: Record<string, unknown>,
	leaves: Map<string, unknown>,
	kept: Map<string, string>,
	ignored: Map<string, string>,
): Record<string, unknown> {
	const next = structuredClone(base)
	for (const [path, spelled] of kept) {
		if (!leaves.has(path)) {
			ignored.set(path, 'the config has no such field')
			continue
		}
		let value: unknown
		try {
			value = JSON.parse(spelled)
		} catch {
			ignored.set(path, `${spelled} is no JSON`)
			continue
		}
		if (!fits(value, leaves.get(path))) {
			ignored.set(path, `${spelled} is no ${kindOf(leaves.get(path))}`)
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
function fromEnv(text: string, wanted: unknown, name: string): unknown {
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
		return trimmed === '' ? [] : trimmed.split(',').map(part => fromEnv(part.trim(), item, name))
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
		typeof value === 'object' && value !== null && !Array.isArray(value) && !(value instanceof Date)
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
