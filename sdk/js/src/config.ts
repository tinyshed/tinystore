// A program's settings as one typed value, as plan/api/config.md has it: a
// declaration, then objects, files and the environment in the order the
// program writes them, each declaring again what came before, and the live
// settings' changes, which the store keeps and every process sees. The core
// reads the layers, checks the rules and keeps the changes. This module
// spells the declaration as data, reads the program's own files and
// variables, which belong to it and not to the server, and follows the config
// on one stream that every handle and watcher of it in the process shares.

import { readFile } from 'node:fs/promises'

import type { Connection, Idempotence, Link } from './connection.ts'
import { CancelledError, ConflictError, InvalidError, UnavailableError } from './errors.ts'
import { check, isSchema, type StandardSchemaV1 } from './schema.ts'
import { LostError, type Stream } from './session.ts'
import { type DurationText, ms, spelled } from './time.ts'
import type { Read } from './wire/codec.ts'
import {
	ConfigOpen,
	ConfigReset,
	ConfigUpdate,
	ConfigValue,
	Handle,
	methods,
} from './wire/protocol.ts'

// what marks a setting and a layer: text keys, so that a program's own keys
// never are one and the types say them in a published declaration
const mark = '~setting'
const layerMark = '~layer'

/** The tries a change makes while other changes race it. */
const attempts = 8

// The declaration as the core reads it: plan/api/config.md, and
// crates/tinystore/src/kv/config/data.rs, which reads this JSON.
type Kind =
	| 'text'
	| 'integer'
	| 'number'
	| 'boolean'
	| 'duration'
	| 'object'
	| 'any'
	| { list: Item }
type Item = ({ kind: Kind } & Rules) | { group: Group }
type Group = Record<string, Node>
type Node = Described | { group: Group }

interface Rules {
	min?: number
	max?: number
	oneOf?: string[]
}

interface Described extends Rules {
	kind: Kind
	default?: unknown
	required?: boolean
	live?: boolean
	secret?: boolean
}

/**
 * A setting its declaration says more of than a default would: its rules, a
 * secret, one a layer must give, or one that may change while the program
 * runs. `V` is what the program reads it as, `Live` whether it may change,
 * and `Given` whether it has a default or is required, as a declaration needs.
 */
export interface Setting<
	V,
	Live extends boolean = false,
	Given extends boolean = true,
	IsSecret extends boolean = false,
> {
	readonly [mark]: Described
	/** what the program reads it as; nothing at run time */
	readonly '~value'?: V
	readonly '~live'?: Live
	readonly '~given'?: Given
	readonly '~secret'?: IsSecret
}

/** A setting's text that prints as *** wherever it goes; `reveal` gives it. */
export class Secret {
	readonly #text: string

	constructor(text: string) {
		this.#text = text
	}

	/** The secret's text, where the program hands it to what needs it. */
	reveal(): string {
		return this.#text
	}

	toString(): string {
		return '***'
	}

	toJSON(): string {
		return '***'
	}

	[Symbol.for('nodejs.util.inspect.custom')](): string {
		return '***'
	}
}

/** What a program reads of a declaration: a setting as its value, a group as its settings. */
export type Value<D> =
	D extends Setting<infer V, boolean, boolean, boolean>
		? V
		: D extends string
			? string
			: D extends number
				? number
				: D extends boolean
					? boolean
					: D extends null
						? null
						: D extends readonly (infer I)[]
							? readonly Value<I>[]
							: D extends object
								? { readonly [K in keyof D]: Value<D[K]> }
								: never

type LiveIn<D> =
	D extends Setting<unknown, infer L, boolean, boolean>
		? L
		: D extends readonly unknown[]
			? false
			: D extends object
				? true extends { [K in keyof D]: LiveIn<D[K]> }[keyof D]
					? true
					: false
				: false

/** The live settings a change may name, each as the program reads it, at any depth. */
export type Changes<D> = {
	readonly [K in keyof D as LiveIn<D[K]> extends true ? K : never]?: D[K] extends Setting<
		infer V,
		true,
		boolean,
		boolean
	>
		? V
		: Changes<D[K]>
}

/** An object in code over the declaration: any of its settings, at any depth, a secret as text. */
export type Overlay<D> = {
	readonly [K in keyof D]?: D[K] extends Setting<infer V, boolean, boolean, boolean>
		? V extends Secret
			? string
			: V
		: D[K] extends readonly unknown[]
			? Value<D[K]>
			: D[K] extends object
				? Overlay<D[K]>
				: Value<D[K]>
}

/** A file a config reads: TOML, YAML or JSON by its extension. */
export interface FileLayer {
	readonly [layerMark]: 'file'
	readonly path: string
	readonly optional: boolean
}

/** The environment a config reads. */
export interface EnvLayer {
	readonly [layerMark]: 'env'
	readonly prefix: string
	/** a setting's own variable, by its path */
	readonly vars: Readonly<Record<string, string>>
	readonly files: readonly string[]
	readonly from: Readonly<Record<string, string | undefined>> | undefined
}

/** A layer of a config's list: an object in code, a file, the environment, or nothing. */
export type Layer<D> = FileLayer | EnvLayer | Overlay<D> | false | null | undefined

export interface FileOptions {
	/** a file that may be absent: then it declares nothing, and explain says so */
	optional?: boolean | undefined
}

/** Settings' own variables by their paths: `{ port: 'PORT', db: { url: 'DATABASE_URL' } }`. */
export interface Vars {
	readonly [key: string]: string | Vars
}

export interface EnvOptions {
	/** a setting's variable of its own, in place of its prefixed one */
	vars?: Vars | undefined
	/** .env files, whose variables count where the process has none; Bun reads .env itself */
	files?: readonly string[] | undefined
	/** these variables in place of the process's, the only environment read, as a test gives them */
	from?: Readonly<Record<string, string | undefined>> | undefined
}

export interface ConfigOptions<V> {
	/**
	 * the program's own check of the whole value, at open and before a change
	 * is kept: a Standard Schema such as zod's or valibot's, or a function
	 * that throws
	 */
	validate?: StandardSchemaV1 | ((value: V) => unknown) | undefined
	/** refuses to open while a kept change no longer fits, rather than ignoring it and saying so */
	strict?: boolean | undefined
}

export interface UpdateOptions {
	/** keeps the change only while the config is at this version, else ConflictError */
	ifVersion?: number | undefined
}

/** A setting's line of explain. */
export interface ExplainLine {
	readonly path: string
	/** as JSON, a duration spelled, a secret as *** */
	readonly value: string
	/** the layer that gave it: APP_PORT, app.yaml, layer 2, the declaration, a change */
	readonly from: string
	/** the values it went over, newest first */
	readonly over: string
}

/** Every setting's line, for console.table, and what was read and not used. */
export type Explained = readonly ExplainLine[] & { readonly notes: readonly string[] }

type Token = StringConstructor | NumberConstructor | BooleanConstructor
type Of<T> = T extends StringConstructor
	? string
	: T extends NumberConstructor
		? number
		: T extends BooleanConstructor
			? boolean
			: never
type Primitive = string | number | boolean
type Widened<V> = V extends string
	? string
	: V extends number
		? number
		: V extends boolean
			? boolean
			: V extends readonly (infer I)[]
				? Widened<I>[]
				: V

/** The bounds of a number: the smallest and the largest it may be. */
export interface Bounds {
	min?: number | undefined
	max?: number | undefined
}

/** The bounds of a duration, each a duration's text. */
export interface DurationBounds {
	min?: DurationText | undefined
	max?: DurationText | undefined
}

/** A list's default and how many items it holds at least and at most. */
export interface ListOptions<V> extends Bounds {
	default?: readonly V[] | undefined
}

function setting<V, L extends boolean = false, G extends boolean = true, S extends boolean = false>(
	described: Described,
): Setting<V, L, G, S> {
	return Object.freeze({ [mark]: described }) as unknown as Setting<V, L, G, S>
}

function describedOf(value: unknown): Described | undefined {
	return typeof value === 'object' && value !== null && mark in value
		? (value as { [mark]: Described })[mark]
		: undefined
}

function layerOf(value: unknown): string | undefined {
	return typeof value === 'object' && value !== null && layerMark in value
		? (value as { [layerMark]: string })[layerMark]
		: undefined
}

function kindOf(token: Token): Kind {
	return token === String ? 'text' : token === Number ? 'number' : 'boolean'
}

function bounds(rules: Bounds | undefined): Rules {
	const out: Rules = {}
	if (rules?.min !== undefined) {
		out.min = rules.min
	}
	if (rules?.max !== undefined) {
		out.max = rules.max
	}
	return out
}

/**
 * A setting that prints as *** wherever it goes and that only a layer gives:
 * `secret(String)` must be given, `secret('')` has a default.
 */
export function secret(given: StringConstructor | string): Setting<Secret, false, true, true> {
	if (given === String) {
		return setting({ kind: 'text', secret: true, required: true })
	}
	if (typeof given !== 'string') {
		throw new InvalidError('a secret is text: secret(String), or secret with its default text')
	}
	return setting({ kind: 'text', secret: true, default: given })
}

/** A setting with no default: a layer must give it, or the config does not open. */
export function required<T extends Token>(type: T): Setting<Of<T>>
export function required<V>(rule: Setting<V, false, false>): Setting<V>
export function required(
	given: Token | Setting<unknown, boolean, boolean, boolean>,
): Setting<unknown> {
	const inner = typeof given === 'function' ? { kind: kindOf(given) } : describedOf(given)
	if (inner === undefined) {
		throw new InvalidError('required takes String, Number, Boolean, or a rule without its default')
	}
	if (inner.secret === true) {
		throw new InvalidError('a secret is required already: secret(String)')
	}
	if (inner.live === true) {
		throw new InvalidError('live is the outermost mark: live(required(…))')
	}
	const { default: _default, ...rest } = inner
	return setting({ ...rest, required: true })
}

/**
 * A setting that may change while the program runs: its change is kept in
 * the store, goes over every layer until it is reset, and reaches every
 * process. `live(100)`, `live(int(100, { min: 0 }))`, `live(required(String))`.
 */
export function live<V>(rule: Setting<V, false, true>): Setting<V, true>
export function live<const V extends Primitive | readonly Primitive[]>(
	value: V,
): Setting<Widened<V>, true>
export function live(given: unknown): Setting<unknown, true> {
	if (isPlainObject(given) && describedOf(given) === undefined && Object.keys(given).length > 0) {
		throw new InvalidError('a mark goes on a setting, not on a group: mark each of its settings')
	}
	const inner = describedOf(given) ?? declaredValue(given, 'live')
	if (inner.secret === true) {
		throw new InvalidError('a secret is never live')
	}
	if (inner.live === true) {
		throw new InvalidError('live twice')
	}
	return setting({ ...inner, live: true })
}

/** A whole number in its bounds: `int(8080, { min: 1, max: 65535 })`; `int({ min: 1 })` goes in required. */
export function int(rules?: Bounds): Setting<number, false, false>
export function int(fallback: number, rules?: Bounds): Setting<number>
export function int(first?: number | Bounds, rules?: Bounds): Setting<number, boolean, boolean> {
	if (typeof first === 'number') {
		return setting({ kind: 'integer', default: first, ...bounds(rules) })
	}
	return setting({ kind: 'integer', ...bounds(first) })
}

/**
 * One of a set of texts, its default first: `oneOf('info', ['debug', 'warn', 'error'])`
 * reads as 'info' | 'debug' | 'warn' | 'error'; `oneOf(['eu', 'us'])` goes in required.
 */
export function oneOf<const O extends readonly [string, ...string[]]>(
	values: O,
): Setting<O[number], false, false>
export function oneOf<const T extends string, const O extends readonly string[]>(
	fallback: T,
	others: O,
): Setting<T | O[number]>
export function oneOf(
	first: string | readonly string[],
	others: readonly string[] = [],
): Setting<string, boolean, boolean> {
	if (typeof first === 'string') {
		return setting({ kind: 'text', default: first, oneOf: [first, ...others] })
	}
	return setting({ kind: 'text', oneOf: [...first] })
}

/** A duration, read as one is written: `duration('30s')`, `duration({ max: '1h' })` in required. */
export function duration(rules?: DurationBounds): Setting<DurationText, false, false>
export function duration(fallback: DurationText, rules?: DurationBounds): Setting<DurationText>
export function duration(
	first?: DurationText | DurationBounds,
	rules?: DurationBounds,
): Setting<DurationText, boolean, boolean> {
	const [fallback, given] = typeof first === 'string' ? [first, rules] : [undefined, first]
	const described: Described = { kind: 'duration' }
	if (given?.min !== undefined) {
		described.min = ms(given.min)
	}
	if (given?.max !== undefined) {
		described.max = ms(given.max)
	}
	return setting(fallback === undefined ? described : { ...described, default: fallback })
}

/**
 * A list: of `String`, `Number` or `Boolean`, of a rule, or of groups, each
 * item checked; its `min` and `max` count its items. A list written as a
 * value, `['localhost']`, needs no `list`.
 */
export function list<T extends Token>(
	item: T,
	options?: ListOptions<Of<T>>,
): Setting<readonly Of<T>[]>
export function list<V>(
	item: Setting<V, false, boolean>,
	options?: ListOptions<V>,
): Setting<readonly V[]>
export function list<G extends object>(
	item: G,
	options?: ListOptions<Value<G>>,
): Setting<readonly Value<G>[]>
export function list(item: unknown, options: ListOptions<unknown> = {}): Setting<unknown> {
	return setting({
		kind: { list: itemOf(item) },
		default: options.default ?? [],
		...bounds(options),
	})
}

function itemOf(item: unknown): Item {
	if (item === String || item === Number || item === Boolean) {
		return { kind: kindOf(item as Token) }
	}
	const inner = describedOf(item)
	if (inner !== undefined) {
		if (inner.live === true || inner.secret === true) {
			throw new InvalidError("a list's items are neither live nor secret: the list is one setting")
		}
		const { kind, min, max, oneOf } = inner
		return { kind, ...bounds({ min, max }), ...(oneOf === undefined ? {} : { oneOf }) }
	}
	if (isPlainObject(item)) {
		return { group: declaredGroup(item, '[]') }
	}
	throw new InvalidError(`a list of ${String(item)}: of String, Number, Boolean, a rule or a group`)
}

/** A file a config reads: TOML, YAML or JSON by its extension, a relative path the program's. */
export function file(path: string, options: FileOptions = {}): FileLayer {
	return Object.freeze({ [layerMark]: 'file' as const, path, optional: options.optional === true })
}

/**
 * The environment a config reads: `env('APP')` reads APP_ and each setting's
 * path in upper snake case, `APP_LIMITS_RPS` for limits.rps.
 */
export function env(prefix: string, options: EnvOptions = {}): EnvLayer {
	return Object.freeze({
		[layerMark]: 'env' as const,
		prefix,
		vars: flattened(options.vars ?? {}, ''),
		files: [...(options.files ?? [])],
		from: options.from,
	})
}

function flattened(vars: Vars, path: string): Record<string, string> {
	const out: Record<string, string> = {}
	for (const [key, value] of Object.entries(vars)) {
		const at = path === '' ? key : `${path}.${key}`
		if (typeof value === 'string') {
			out[at] = value
		} else {
			Object.assign(out, flattened(value, at))
		}
	}
	return out
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
	if (typeof value !== 'object' || value === null || Array.isArray(value)) {
		return false
	}
	const proto = Object.getPrototypeOf(value)
	return proto === Object.prototype || proto === null
}

function declaredGroup(value: Record<string, unknown>, path: string): Group {
	const group: Group = {}
	for (const [key, field] of Object.entries(value)) {
		group[key] = declaredNode(field, path === '' ? key : `${path}.${key}`)
	}
	return group
}

function declaredNode(value: unknown, path: string): Node {
	const described = describedOf(value)
	if (described !== undefined) {
		return described
	}
	if (isPlainObject(value) && Object.keys(value).length > 0) {
		return { group: declaredGroup(value, path) }
	}
	return declaredValue(value, path)
}

/** A setting a plain default declares: its kind is its default's. */
function declaredValue(value: unknown, path: string): Described {
	switch (typeof value) {
		case 'string':
			return { kind: 'text', default: value }
		case 'number':
			return { kind: 'number', default: value }
		case 'boolean':
			return { kind: 'boolean', default: value }
		case 'function':
			throw new InvalidError(
				`${path}: a type is no default; required(${(value as Token).name}) declares one a layer must give`,
			)
		case 'undefined':
			throw new InvalidError(`${path}: a setting has a default, or is required`)
	}
	if (value === null) {
		return { kind: 'any', default: null }
	}
	if (Array.isArray(value)) {
		return { kind: { list: plainItem(value[0]) }, default: value }
	}
	if (isPlainObject(value)) {
		return { kind: 'object', default: {} }
	}
	throw new InvalidError(`${path}: ${String(value)} is no default a config keeps`)
}

function plainItem(first: unknown): Item {
	switch (typeof first) {
		case 'string':
			return { kind: 'text' }
		case 'number':
			return { kind: 'number' }
		case 'boolean':
			return { kind: 'boolean' }
		default:
			return { kind: 'any' }
	}
}

/** Every setting's path, as the core names its variable. */
function paths(group: Group, path: string): string[] {
	return Object.entries(group).flatMap(([key, node]) => {
		const at = path === '' ? key : `${path}.${key}`
		return 'group' in node ? paths(node.group, at) : [at]
	})
}

/**
 * A setting's variable: the prefix and its path in upper snake case, as Go,
 * Python and the core name it (testdata/config/env.json).
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

/** A file's text, none when it does not exist. */
async function readText(path: string): Promise<string | undefined> {
	try {
		return await readFile(path, 'utf8')
	} catch (err) {
		if ((err as NodeJS.ErrnoException).code === 'ENOENT') {
			return undefined
		}
		throw new InvalidError(`${path}: ${(err as Error).message}`)
	}
}

/** A layer as the core reads it, its files and variables read here. */
async function layerData(
	layer: unknown,
	place: number,
	declared: Group,
): Promise<object | undefined> {
	if (layer === false || layer === null || layer === undefined) {
		return undefined
	}
	const kind = layerOf(layer)
	if (kind === 'file') {
		const { path, optional } = layer as FileLayer
		const text = await readText(path)
		return { file: { name: path, optional, ...(text === undefined ? {} : { text }) } }
	}
	if (kind === 'env') {
		return { env: await envData(layer as EnvLayer, declared) }
	}
	if (isPlainObject(layer)) {
		return { object: layer, place }
	}
	throw new InvalidError(`layer ${place}: an object, file(…) or env(…)`)
}

/**
 * What the environment gives, read here: the variables of every setting and
 * those with the prefix, never the rest of the process's, the files their
 * NAME_FILE names, and the .env files' text.
 */
async function envData(layer: EnvLayer, declared: Group): Promise<object> {
	const windows = process.platform === 'win32'
	const key = (name: string) => (windows ? name.toUpperCase() : name)
	const named = paths(declared, '').map(path => layer.vars[path] ?? envName(layer.prefix, path))
	const wanted = new Set(named.flatMap(name => [key(name), key(`${name}_FILE`)]))
	const start = key(`${layer.prefix}_`)
	const variables: Record<string, string> = {}
	for (const [name, value] of Object.entries(layer.from ?? process.env)) {
		const k = key(name)
		if (value !== undefined && (wanted.has(k) || (layer.prefix !== '' && k.startsWith(start)))) {
			variables[k] = value
		}
	}
	const dotenv: { name: string; text: string }[] = []
	for (const name of layer.files) {
		const text = await readText(name)
		if (text !== undefined) {
			dotenv.push({ name, text })
		}
	}
	const files: Record<string, string> = {}
	const namedFiles = [...Object.entries(variables), ...dotenv.flatMap(d => fileVariables(d.text))]
	for (const [name, path] of namedFiles) {
		if (name.endsWith('_FILE')) {
			const text = await readText(path)
			if (text !== undefined) {
				files[path] = text
			}
		}
	}
	return { prefix: layer.prefix, vars: layer.vars, variables, files, dotenv }
}

/** The NAME_FILE lines of a .env file's text, whose files the core cannot read. */
function fileVariables(text: string): [string, string][] {
	const found: [string, string][] = []
	for (const line of text.split('\n')) {
		const match = /^\s*(?:export\s+)?([\p{L}_][\p{L}\p{Nd}_]*_FILE)\s*=\s*(.*?)\s*$/u.exec(line)
		if (match !== null) {
			found.push([match[1]!, match[2]!.replace(/^(['"])(.*)\1$/, '$2')])
		}
	}
	return found
}

/** A value as the program reads it: a duration spelled, a secret wrapped, all of it frozen. */
function viewed(group: Group, value: unknown): unknown {
	const source = (isPlainObject(value) ? value : {}) as Record<string, unknown>
	const out: Record<string, unknown> = {}
	for (const [key, node] of Object.entries(group)) {
		out[key] = 'group' in node ? viewed(node.group, source[key]) : viewedSetting(node, source[key])
	}
	return Object.freeze(out)
}

function viewedSetting(described: Described, value: unknown): unknown {
	if (described.secret === true) {
		return new Secret(typeof value === 'string' ? value : '')
	}
	return viewedKind(described.kind, value)
}

function viewedKind(kind: Kind, value: unknown): unknown {
	if (kind === 'duration' && typeof value === 'number') {
		return spelled(value)
	}
	if (typeof kind === 'object' && Array.isArray(value)) {
		const item = kind.list
		const items = value.map(each =>
			'group' in item ? viewed(item.group, each) : viewedKind(item.kind, each),
		)
		return Object.freeze(items)
	}
	return frozen(value)
}

function frozen(value: unknown): unknown {
	if (typeof value === 'object' && value !== null && !Object.isFrozen(value)) {
		for (const inner of Object.values(value)) {
			frozen(inner)
		}
		Object.freeze(value)
	}
	return value
}

/** A partial value over a whole one: the groups it names merged, anything else replaced. */
function merged(value: unknown, changes: unknown): unknown {
	if (!isPlainObject(value) || !isPlainObject(changes)) {
		return changes
	}
	const out: Record<string, unknown> = { ...value }
	for (const [key, change] of Object.entries(changes)) {
		out[key] = merged(value[key], change)
	}
	return out
}

type Watcher<V> = (next: V, previous: V | undefined) => void
type Answer = Read<typeof ConfigValue.fields>

/**
 * A program's settings as one typed value, read from memory: open one with
 * `store.config(name, [declaration, ...layers])`.
 */
export class Config<D> {
	readonly name: string
	readonly #link: Link
	readonly #declared: Group
	readonly #open: Uint8Array
	readonly #check: ((value: unknown) => Promise<void>) | undefined
	readonly #handles = new WeakMap<Connection, Promise<number>>()
	readonly #watchers = new Set<Watcher<Value<D>>>()
	#value: Value<D>
	#version = 0
	#lines: ExplainLine[] = []
	#notes: string[] = []
	/** why validate refused the newest value the store sent, a change another program made */
	#refused: InvalidError | undefined
	#taking: Promise<void> = Promise.resolve()
	#newer: (() => void)[] = []
	#stream: Stream | undefined
	#stopped = false

	private constructor(
		link: Link,
		name: string,
		declared: Group,
		open: Uint8Array,
		check: ((value: unknown) => Promise<void>) | undefined,
	) {
		this.#link = link
		this.name = name
		this.#declared = declared
		this.#open = open
		this.#check = check
		this.#value = viewed(declared, {}) as Value<D>
	}

	/** Opens a config from its list: the declaration first, then its layers in order. */
	static async open<D>(
		link: Link,
		name: string,
		layers: readonly unknown[],
		options: ConfigOptions<Value<D>>,
	): Promise<Config<D>> {
		const [declaration, ...rest] = layers
		if (!isPlainObject(declaration)) {
			throw new InvalidError(`config ${name}: the list's first entry is its declaration, an object`)
		}
		const declared = declaredGroup(declaration, '')
		const data = await Promise.all(rest.map((layer, at) => layerData(layer, at + 2, declared)))
		const open = ConfigOpen.encode({
			name,
			declaration: JSON.stringify(declared),
			layers: JSON.stringify(data.filter(layer => layer !== undefined)),
			strict: options.strict === true,
		})
		const config = new Config<D>(link, name, declared, open, checkerOf(name, options.validate))
		await link.run('read', connection => config.#handleOn(connection))
		await config.#taking
		if (config.#refused !== undefined) {
			throw config.#refused
		}
		void config.#follow()
		return config
	}

	/** The settings as they are now: a read of memory, so read it where it is used. */
	get value(): Value<D> {
		return this.#value
	}

	/** The version of the changes the value holds, which `ifVersion` takes. */
	get version(): number {
		return this.#version
	}

	/**
	 * Changes live settings: the ones `changes` names are kept in the store, go
	 * over every layer until they are reset, and reach every handle of the
	 * config. The value they make passes validate first; all are kept or none.
	 */
	async update(changes: Changes<D>, options: UpdateOptions = {}): Promise<void> {
		for (let tries = 1; ; tries++) {
			const version = options.ifVersion ?? this.#version
			await this.#check?.(merged(this.#value, changes))
			const body = (handle: number) =>
				ConfigUpdate.encode({ handle, version, changes: JSON.stringify(changes) })
			try {
				await this.#call('write', methods['config.update'], body)
				return
			} catch (err) {
				if (
					!(err instanceof ConflictError) ||
					options.ifVersion !== undefined ||
					tries === attempts
				) {
					throw err
				}
				await this.#newerThan(version)
			}
		}
	}

	/** Drops the change kept of the setting or the group at `path`, or of every setting. */
	async reset(path = ''): Promise<void> {
		await this.#call('write', methods['config.reset'], handle =>
			ConfigReset.encode({ handle, path }),
		)
	}

	/**
	 * Calls `watcher` with the settings now, then with each change and the value
	 * before it, made in this process or another. It returns the call that stops it.
	 */
	watch(watcher: (next: Value<D>, previous: Value<D> | undefined) => void): () => void {
		this.#watchers.add(watcher)
		watcher(this.#value, undefined)
		return () => {
			this.#watchers.delete(watcher)
		}
	}

	/** Every setting with its value, the layer that gave it and what it went over. */
	explain(): Explained {
		const refused =
			this.#refused === undefined ? [] : [`a value validate refused: ${this.#refused.message}`]
		return Object.assign([...this.#lines], { notes: [...this.#notes, ...refused] })
	}

	/** Stops following the config: the store closes. */
	stop(): void {
		this.#stopped = true
		this.#watchers.clear()
		this.#stream?.cancel()
		for (const wake of this.#newer.splice(0)) {
			wake()
		}
	}

	/** Runs a call on the config's handle on the link's connection, and takes the value it answers. */
	async #call(
		idempotence: Idempotence,
		method: number,
		body: (handle: number) => Uint8Array,
	): Promise<void> {
		await this.#link.run(idempotence, async connection => {
			const handle = await this.#handleOn(connection)
			await this.#take(ConfigValue.decode(await connection.session.call(method, body(handle))))
		})
	}

	/** The config's handle on a connection, opened there first, its value taken. */
	#handleOn(connection: Connection): Promise<number> {
		let handle = this.#handles.get(connection)
		if (handle === undefined) {
			handle = connection.session.call(methods['config.open'], this.#open).then(async body => {
				const opened = ConfigValue.decode(body)
				await this.#take(opened)
				return opened.handle ?? 0
			})
			this.#handles.set(connection, handle)
			handle.catch(() => this.#handles.delete(connection))
		}
		return handle
	}

	/** Takes a value the store sent, in the order it came; an older one than this has is left. */
	#take(answer: Answer): Promise<void> {
		this.#taking = this.#taking.then(() => this.#accept(answer))
		return this.#taking
	}

	async #accept(answer: Answer): Promise<void> {
		const version = answer.version ?? 0
		if (version < this.#version || (version === this.#version && this.#lines.length > 0)) {
			return
		}
		const next = viewed(this.#declared, JSON.parse(answer.value ?? '{}')) as Value<D>
		try {
			await this.#check?.(next)
		} catch (err) {
			this.#refused = err instanceof InvalidError ? err : new InvalidError((err as Error).message)
			if (this.#lines.length > 0) {
				process.stderr.write(
					`version ${version}: ${this.#refused.message}; the value before it stays\n`,
				)
			}
			return
		}
		const previous = this.#value
		this.#refused = undefined
		this.#value = next
		this.#version = version
		this.#lines = (answer.lines ?? []).map(line => ({
			path: line.path ?? '',
			value: line.value ?? '',
			from: line.from ?? '',
			over: (line.over ?? []).join(', '),
		}))
		this.#notes = answer.notes ?? []
		for (const wake of this.#newer.splice(0)) {
			wake()
		}
		for (const watcher of this.#watchers) {
			try {
				watcher(next, previous)
			} catch (err) {
				process.stderr.write(`config ${this.name}: a watcher threw: ${(err as Error).message}\n`)
			}
		}
	}

	/** Waits until a version later than `version` came, or a second passed. */
	#newerThan(version: number): Promise<void> {
		if (this.#version > version || this.#stopped) {
			return Promise.resolve()
		}
		return new Promise(resolve => {
			const timer = setTimeout(resolve, 1000)
			this.#newer.push(() => {
				clearTimeout(timer)
				resolve()
			})
		})
	}

	/**
	 * Follows the config on one stream until the store closes: a lost
	 * connection or a server going away follows again on the next one.
	 */
	async #follow(): Promise<void> {
		while (!this.#stopped) {
			try {
				const stream = await this.#link.run('read', async connection => {
					const handle = await this.#handleOn(connection)
					const opened = await connection.session.open(
						methods['config.follow'],
						Handle.encode({ handle }),
						true,
					)
					await opened.next()
					return opened
				})
				this.#stream = stream
				if (this.#stopped) {
					stream.cancel()
					return
				}
				for (;;) {
					const event = await stream.next()
					stream.consumed(event.body.length)
					if (event.end) {
						break
					}
					await this.#take(ConfigValue.decode(event.body))
				}
			} catch (err) {
				if (this.#stopped) {
					return
				}
				const passing =
					err instanceof LostError ||
					err instanceof UnavailableError ||
					err instanceof CancelledError
				if (!passing) {
					await new Promise(resolve => setTimeout(resolve, 1000))
				}
			}
		}
	}
}

/** The check a config's validate makes of a whole value, throwing InvalidError when it refuses. */
function checkerOf<V>(
	name: string,
	validate: ConfigOptions<V>['validate'],
): ((value: unknown) => Promise<void>) | undefined {
	if (validate === undefined) {
		return undefined
	}
	if (isSchema(validate)) {
		return async value => {
			const checked = await check(validate, value)
			if ('issues' in checked) {
				throw new InvalidError(`config ${name}: ${checked.issues}`)
			}
		}
	}
	return async value => {
		try {
			await validate(value as V)
		} catch (err) {
			throw err instanceof InvalidError
				? err
				: new InvalidError(`config ${name}: ${(err as Error).message}`)
		}
	}
}
