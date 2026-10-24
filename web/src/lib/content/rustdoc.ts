import { execFileSync } from 'node:child_process'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'

import type { Item } from './reference'

/**
 * The Rust API pages' items, read from the JSON rustdoc writes for
 * `crates/tinystore`. The JSON is unstable, so it is asked of the pinned
 * toolchain through RUSTC_BOOTSTRAP rather than of a nightly: the format
 * moves only when the toolchain does, and `formatVersion` says which one this
 * reader was written for.
 *
 * An engine adds its calls to `Store` from its own module, so a method of
 * `Store` belongs to the engine whose directory its source is in:
 * `Store::bucket` is written in `src/kv/bucket.rs` and is KV's.
 */

const formatVersion = 61

// biome-ignore lint/suspicious/noExplicitAny: rustdoc's JSON is unstable and untyped, and formatVersion pins its shape
type Json = any

/**
 * Where each engine's items are: the modules it owns, the directories its
 * `Store` methods come from, and the files its `aio::Store` methods come
 * from. The async API's module of an engine, `aio::kv`, is listed on that
 * engine's page, its names written with their path, `aio::kv::Bucket`, so
 * that they read apart from the blocking `Bucket` beside them.
 */
export const rustSources: Record<string, { modules: string[]; dirs: string[]; aio: string[] }> = {
	store: { modules: ['', 'aio'], dirs: ['store.rs', 'transaction.rs'], aio: ['aio/store.rs'] },
	sql: { modules: ['sql', 'aio::sql'], dirs: ['sql/'], aio: ['aio/sql.rs', 'aio/sql/'] },
	kv: {
		modules: ['kv', 'aio::kv'],
		dirs: ['kv/bucket.rs', 'kv/counters.rs', 'kv/once.rs', 'kv/quota.rs', 'kv/rate_limit.rs'],
		aio: ['aio/kv.rs', 'aio/kv/'],
	},
	jobs: { modules: ['jobs', 'aio::jobs'], dirs: ['jobs/'], aio: ['aio/jobs.rs', 'aio/jobs/'] },
	blobs: {
		modules: ['blobs', 'aio::blobs'],
		dirs: ['blobs/'],
		aio: ['aio/blobs.rs', 'aio/blobs/'],
	},
	search: {
		modules: ['search', 'aio::search'],
		dirs: ['search/'],
		aio: ['aio/search.rs', 'aio/search/'],
	},
	channels: {
		modules: ['channels', 'aio::channels'],
		dirs: ['channels/'],
		aio: ['aio/channels.rs', 'aio/channels/'],
	},
	locks: {
		modules: ['locks', 'aio::locks'],
		dirs: ['kv/locks/'],
		aio: ['aio/locks.rs', 'aio/locks/'],
	},
	config: {
		modules: ['config', 'aio::config'],
		dirs: ['kv/config/'],
		aio: ['aio/config.rs', 'aio/config/'],
	},
	records: {
		modules: ['records', 'console', 'aio::records'],
		dirs: ['records/', 'console/'],
		aio: ['aio/records.rs', 'aio/records/'],
	},
	metrics: {
		modules: ['metrics', 'aio::metrics'],
		dirs: ['metrics/'],
		aio: ['aio/metrics.rs', 'aio/metrics/'],
	},
	backup: {
		modules: ['backup', 'aio::backup'],
		dirs: ['backup/'],
		aio: ['aio/backup.rs', 'aio/backup/'],
	},
}

// modules another engine owns, or that no page shows: the runtime's own and replication's
const notInModule: Record<string, string[]> = { kv: ['config', 'locks'] }
const unlisted = new Set(['engine', 'pipe', 'replication', 'typos'])

interface Doc {
	root: number
	format_version: number
	index: Record<string, Entry>
}

interface Entry {
	id: number
	crate_id: number
	name: string | null
	span: { filename: string } | null
	visibility: 'public' | 'default' | 'crate' | { restricted: unknown }
	docs: string | null
	inner: Record<string, Json>
}

/** Writes the crate's JSON with the pinned toolchain and reads it. */
export function rustdoc(root: string): Doc {
	execFileSync(
		'cargo',
		[
			'rustdoc',
			'-q',
			'-p',
			'tinystore',
			'--lib',
			'--all-features',
			'--',
			'-Z',
			'unstable-options',
			'--output-format',
			'json',
		],
		{
			cwd: root,
			env: { ...process.env, RUSTC_BOOTSTRAP: '1' },
			stdio: ['ignore', 'ignore', 'inherit'],
		},
	)
	const doc = JSON.parse(readFileSync(join(root, 'target/doc/tinystore.json'), 'utf8')) as Doc
	if (doc.format_version !== formatVersion) {
		throw new Error(
			`rustdoc wrote format ${doc.format_version}, and rustdoc.ts reads ${formatVersion}: ` +
				'the toolchain moved, so check the printer against the new format and raise formatVersion',
		)
	}
	return doc
}

/** One engine's items: what its modules export, and the methods of `Store` its files add. */
export function rustItems(doc: Doc, engine: string): Item[] {
	const source = rustSources[engine]
	if (source === undefined) {
		throw new Error(`rustdoc.ts has no sources for the ${engine} engine`)
	}
	const reader = new Reader(doc)
	const items = source.modules.flatMap(path => reader.module(path, engine))
	const stores = [
		{ name: 'Store', members: reader.storeMethods('', source.dirs) },
		{ name: 'aio::Store', members: reader.storeMethods('aio', source.aio) },
	]
	if (engine === 'store') {
		for (const { name, members } of stores) {
			const struct = items.find(item => item.name === name)
			if (struct !== undefined) {
				struct.members = members
			}
		}
		return items
	}
	const impls = stores
		.filter(({ members }) => members.length > 0)
		.map(({ name, members }) => ({ name, code: `impl ${name}`, doc: '', members }))
	return [...impls, ...items]
}

class Reader {
	readonly #index: Record<string, Entry>
	readonly #root: Entry

	constructor(doc: Doc) {
		this.#index = doc.index
		this.#root = this.#entry(doc.root)
	}

	/**
	 * The public items of `tinystore::<path>`, a submodule's under its name:
	 * `memory::Budget`. The root and `aio` list their own items alone, since
	 * each engine's page lists its module, and the async API's names carry
	 * their path. An engine's module of the async API that rustdoc did not
	 * write, hidden until its engine's calls are built, lists nothing.
	 */
	module(path: string, engine: string): Item[] {
		const module = path === '' ? this.#root : this.#find(this.#root, path)
		if (module === undefined && path.startsWith('aio::')) {
			return []
		}
		if (module === undefined) {
			throw new Error(`rustdoc wrote no public module tinystore::${path}`)
		}
		const skip = new Set([...(notInModule[engine] ?? []), ...(path === '' ? unlisted : [])])
		const prefix = path === 'aio' || path.startsWith('aio::') ? `${path}::` : ''
		return this.#items(module, prefix, skip, path === '' || path === 'aio')
	}

	/** The inherent methods of `Store` in `tinystore::<path>` whose source is in one of `dirs`, under `src/`. */
	storeMethods(path: string, dirs: string[]): Item[] {
		const module = path === '' ? this.#root : this.#child(this.#root, path)
		const store = (module.inner.module.items as number[])
			.map(id => this.#throughUse(this.#entry(id)))
			.find(entry => entry?.name === 'Store' && 'struct' in entry.inner)
		if (store === undefined) {
			throw new Error(`rustdoc wrote no struct Store in tinystore${path === '' ? '' : `::${path}`}`)
		}
		const mine = (entry: Entry) => {
			const file = (entry.span?.filename ?? '').replace(/\\/g, '/')
			const at = file.indexOf('crates/tinystore/src/')
			const local = at === -1 ? '' : file.slice(at + 'crates/tinystore/src/'.length)
			return dirs.some(dir => (dir.endsWith('/') ? local.startsWith(dir) : local === dir))
		}
		return this.#methods(store, path === '' ? 'Store' : `${path}::Store`)
			.filter(({ entry }) => mine(entry))
			.map(({ item }) => item)
	}

	#items(module: Entry, prefix: string, skip: Set<string>, rootOnly: boolean): Item[] {
		const items: Item[] = []
		for (const id of module.inner.module.items as number[]) {
			const entry = this.#entry(id)
			if (entry.visibility !== 'public') {
				continue
			}
			if ('use' in entry.inner) {
				items.push(...this.#reexport(entry, prefix, skip, rootOnly))
				continue
			}
			if ('module' in entry.inner) {
				if (!rootOnly && !skip.has(entry.name ?? '')) {
					items.push(...this.#items(entry, `${prefix}${entry.name}::`, new Set(), false))
				}
				continue
			}
			const item = this.#item(entry, prefix)
			if (item !== undefined) {
				items.push(item)
			}
		}
		return items
	}

	#reexport(entry: Entry, prefix: string, skip: Set<string>, rootOnly: boolean): Item[] {
		const use = entry.inner.use as { name: string; id: number | null; is_glob: boolean }
		const target = use.id === null ? undefined : this.#index[String(use.id)]
		if (target === undefined || target.crate_id !== 0 || skip.has(use.name)) {
			return []
		}
		if ('module' in target.inner) {
			return use.is_glob ? this.#items(target, prefix, skip, rootOnly) : []
		}
		const item = this.#item({ ...target, name: use.name }, prefix)
		return item === undefined ? [] : [item]
	}

	#item(entry: Entry, prefix: string): Item | undefined {
		const name = `${prefix}${entry.name}`
		const doc = docOf(entry.docs)
		const [kind, inner] = Object.entries(entry.inner)[0] ?? []
		const printed = this.#code(kind ?? '', inner, entry.name ?? '')
		if (printed === undefined) {
			return undefined
		}
		const members =
			kind === 'struct' || kind === 'enum' || kind === 'trait' ? this.#members(entry, name) : []
		const target = kind === 'struct' ? this.#derefTarget(entry) : undefined
		const code = target === undefined ? printed : `${printed}\n\nimpl Deref<Target = ${target}>`
		return { name, code, doc, ...(members.length > 0 ? { members } : {}) }
	}

	// a type that derefs to another has that one's methods too, as a table its query's
	#derefTarget(entry: Entry): string | undefined {
		const [kind] = Object.keys(entry.inner)
		const impls = (entry.inner[kind ?? '']?.impls ?? []) as number[]
		for (const id of impls) {
			const impl = this.#entry(id).inner.impl
			if (shortPath(impl.trait?.path ?? '') !== 'Deref') {
				continue
			}
			for (const itemId of impl.items as number[]) {
				const item = this.#entry(itemId)
				if (item.name === 'Target' && 'assoc_type' in item.inner) {
					return new Types().type(item.inner.assoc_type.type)
				}
			}
		}
		return undefined
	}

	#code(kind: string, inner: Json, name: string): string | undefined {
		const t = new Types()
		switch (kind) {
			case 'function':
				return t.function(name, inner)
			case 'struct':
				return this.#struct(name, inner, t)
			case 'enum':
				return this.#enum(name, inner, t)
			case 'trait':
				return `pub trait ${name}${t.generics(inner.generics)}`
			case 'type_alias':
				return `pub type ${name}${t.generics(inner.generics)} = ${t.type(inner.type)}`
			case 'constant':
				return `pub const ${name}: ${t.type(inner.type)}`
			case 'static':
				return `pub static ${name}: ${t.type(inner.type)}`
			case 'macro':
				return `macro_rules! ${name}`
			default:
				return undefined
		}
	}

	#struct(name: string, inner: Json, t: Types): string {
		const head = `pub struct ${name}${t.generics(inner.generics)}`
		const kind = inner.kind
		if (kind === 'unit') {
			return head
		}
		if ('tuple' in kind) {
			const fields = (kind.tuple as (number | null)[]).map(id =>
				id === null ? '_' : `pub ${t.type(this.#entry(id).inner.struct_field)}`,
			)
			return `${head}(${fields.join(', ')})`
		}
		const fields = (kind.plain.fields as number[])
			.map(id => this.#entry(id))
			.filter(field => field.visibility === 'public')
			.map(field => `    pub ${field.name}: ${t.type(field.inner.struct_field)},`)
		if (fields.length === 0) {
			return `${head} { /* private fields */ }`
		}
		return `${head} {\n${fields.join('\n')}\n}`
	}

	#enum(name: string, inner: Json, t: Types): string {
		const variants = (inner.variants as number[]).map(id => {
			const variant = this.#entry(id)
			const kind = variant.inner.variant.kind
			if (kind === 'plain') {
				return `    ${variant.name},`
			}
			if ('tuple' in kind) {
				const fields = (kind.tuple as (number | null)[]).map(field =>
					field === null ? '_' : t.type(this.#entry(field).inner.struct_field),
				)
				return `    ${variant.name}(${fields.join(', ')}),`
			}
			const fields = (kind.struct.fields as number[]).map(field => {
				const entry = this.#entry(field)
				return `${entry.name}: ${t.type(entry.inner.struct_field)}`
			})
			return `    ${variant.name} { ${fields.join(', ')} },`
		})
		return `pub enum ${name}${t.generics(inner.generics)} {\n${variants.join('\n')}\n}`
	}

	// a type's inherent methods, or a trait's own
	#members(entry: Entry, name: string): Item[] {
		if ('trait' in entry.inner) {
			const t = new Types()
			return (entry.inner.trait.items as number[])
				.map(id => this.#entry(id))
				.filter(member => 'function' in member.inner)
				.map(member => ({
					name: `${name}.${member.name}`,
					code: t.function(member.name ?? '', member.inner.function, ''),
					doc: docOf(member.docs),
				}))
		}
		return this.#methods(entry, name).map(({ item }) => item)
	}

	#methods(entry: Entry, name: string): { entry: Entry; item: Item }[] {
		const [kind] = Object.keys(entry.inner)
		const impls = (entry.inner[kind ?? '']?.impls ?? []) as number[]
		const t = new Types()
		const out: { entry: Entry; item: Item }[] = []
		for (const id of impls) {
			const impl = this.#entry(id).inner.impl
			if (impl.trait !== null || impl.is_synthetic || impl.blanket_impl !== null) {
				continue
			}
			for (const memberId of impl.items as number[]) {
				const member = this.#entry(memberId)
				if (member.visibility !== 'public') {
					continue
				}
				// `Level::WARN`: a constant of the type, called as its member is
				const code =
					'function' in member.inner
						? t.function(member.name ?? '', member.inner.function)
						: 'assoc_const' in member.inner
							? `pub const ${member.name}: ${t.type(member.inner.assoc_const.type)}`
							: undefined
				if (code === undefined) {
					continue
				}
				out.push({
					entry: member,
					item: { name: `${name}.${member.name}`, code, doc: docOf(member.docs) },
				})
			}
		}
		return out
	}

	#child(module: Entry, path: string): Entry {
		const child = this.#find(module, path)
		if (child === undefined) {
			throw new Error(`rustdoc wrote no public module tinystore::${path}`)
		}
		return child
	}

	#find(module: Entry, path: string): Entry | undefined {
		let current = module
		for (const part of path.split('::')) {
			const next = (current.inner.module.items as number[])
				.map(id => this.#entry(id))
				.map(entry => this.#throughUse(entry))
				.find(entry => entry?.name === part && 'module' in entry.inner)
			if (next === undefined) {
				return undefined
			}
			current = next
		}
		return current
	}

	// `pub use kv::config;` at the root is the module config
	#throughUse(entry: Entry): Entry | undefined {
		if (!('use' in entry.inner)) {
			return entry
		}
		const use = entry.inner.use as { name: string; id: number | null }
		const target = use.id === null ? undefined : this.#index[String(use.id)]
		return target === undefined ? undefined : { ...target, name: use.name }
	}

	#entry(id: number): Entry {
		const entry = this.#index[String(id)]
		if (entry === undefined) {
			throw new Error(`rustdoc's JSON names item ${id} and does not hold it`)
		}
		return entry
	}
}

/** Prints rustdoc's types as Rust spells them. */
class Types {
	function(name: string, fn: Json, visibility = 'pub '): string {
		const header = fn.header
		const qualifiers = [
			header.is_const ? 'const ' : '',
			header.is_async ? 'async ' : '',
			header.is_unsafe ? 'unsafe ' : '',
		].join('')
		const inputs = (fn.sig.inputs as [string, Json][]).map(([arg, type]) =>
			this.#argument(arg, type),
		)
		const output = fn.sig.output === null ? '' : ` -> ${this.type(fn.sig.output)}`
		const signature = `${visibility}${qualifiers}fn ${name}${this.generics(fn.generics)}`
		const oneLine = `${signature}(${inputs.join(', ')})${output}`
		const where = this.#where(fn.generics)
		if (oneLine.length <= 100) {
			return oneLine + where
		}
		return `${signature}(\n${inputs.map(input => `    ${input},`).join('\n')}\n)${output}${where}`
	}

	generics(generics: Json): string {
		const params = (generics.params as Json[])
			.filter(param => !param.kind.type?.is_synthetic && !param.kind.lifetime?.is_synthetic)
			.map(param => this.#param(param))
		return params.length === 0 ? '' : `<${params.join(', ')}>`
	}

	type(type: Json): string {
		if (typeof type === 'string') {
			return type === 'infer' ? '_' : type
		}
		const [kind, inner] = Object.entries(type)[0] as [string, Json]
		switch (kind) {
			case 'resolved_path':
				return `${shortPath(inner.path)}${this.#args(inner.args)}`
			case 'generic':
			case 'primitive':
				return inner
			case 'borrowed_ref':
				return `&${inner.lifetime === null ? '' : `${inner.lifetime} `}${inner.is_mutable ? 'mut ' : ''}${this.type(inner.type)}`
			case 'raw_pointer':
				return `*${inner.is_mutable ? 'mut' : 'const'} ${this.type(inner.type)}`
			case 'tuple':
				return `(${(inner as Json[]).map(t => this.type(t)).join(', ')}${inner.length === 1 ? ',' : ''})`
			case 'slice':
				return `[${this.type(inner)}]`
			case 'array':
				return `[${this.type(inner.type)}; ${inner.len}]`
			case 'impl_trait':
				return `impl ${this.bounds(inner)}`
			case 'dyn_trait':
				return `dyn ${(inner.traits as Json[]).map(t => this.#poly(t)).join(' + ')}${inner.lifetime === null ? '' : ` + ${inner.lifetime}`}`
			case 'qualified_path':
				return inner.trait === null
					? `${this.type(inner.self_type)}::${inner.name}`
					: `<${this.type(inner.self_type)} as ${shortPath(inner.trait.path)}>::${inner.name}`
			case 'function_pointer':
				return this.#pointer(inner)
			default:
				return '_'
		}
	}

	bounds(bounds: Json[]): string {
		return bounds.map(bound => this.#bound(bound)).join(' + ')
	}

	#argument(name: string, type: Json): string {
		if (name === 'self') {
			const self = type
			if (self.generic === 'Self') {
				return 'self'
			}
			if (self.borrowed_ref?.type?.generic === 'Self') {
				return `&${self.borrowed_ref.is_mutable ? 'mut ' : ''}self`
			}
			return `self: ${this.type(type)}`
		}
		return `${name}: ${this.type(type)}`
	}

	#param(param: Json): string {
		const kind = param.kind
		if (kind.lifetime !== undefined) {
			const outlives = kind.lifetime.outlives as string[]
			return outlives.length === 0 ? param.name : `${param.name}: ${outlives.join(' + ')}`
		}
		if (kind.const !== undefined) {
			return `const ${param.name}: ${this.type(kind.const.type)}`
		}
		const bounds = kind.type.bounds as Json[]
		return bounds.length === 0 ? param.name : `${param.name}: ${this.bounds(bounds)}`
	}

	#where(generics: Json): string {
		const predicates = (generics.where_predicates as Json[]).flatMap(predicate => {
			if (predicate.bound_predicate !== undefined) {
				const bound = predicate.bound_predicate
				return [`${this.type(bound.type)}: ${this.bounds(bound.bounds)}`]
			}
			return []
		})
		return predicates.length === 0 ? '' : `\nwhere\n${predicates.map(p => `    ${p},`).join('\n')}`
	}

	#bound(bound: Json): string {
		if (typeof bound.outlives === 'string') {
			return bound.outlives
		}
		if (bound.trait_bound === undefined) {
			return '_'
		}
		const modifier = bound.trait_bound.modifier === 'maybe' ? '?' : ''
		return `${modifier}${this.#poly(bound.trait_bound)}`
	}

	#poly(poly: Json): string {
		const params = (poly.generic_params as Json[]).map(param => param.name)
		const binder = params.length === 0 ? '' : `for<${params.join(', ')}> `
		return `${binder}${shortPath(poly.trait.path)}${this.#args(poly.trait.args)}`
	}

	#args(args: Json): string {
		if (args === null || args === undefined) {
			return ''
		}
		if (args.parenthesized !== undefined) {
			const inputs = (args.parenthesized.inputs as Json[]).map(t => this.type(t)).join(', ')
			const output =
				args.parenthesized.output === null ? '' : ` -> ${this.type(args.parenthesized.output)}`
			return `(${inputs})${output}`
		}
		const angle = args.angle_bracketed
		if (angle === undefined) {
			return ''
		}
		const parts = [
			...(angle.args as Json[]).map(arg =>
				arg.type !== undefined ? this.type(arg.type) : (arg.lifetime ?? arg.const?.expr ?? '_'),
			),
			...(angle.constraints as Json[]).map(c =>
				c.binding.equality !== undefined
					? `${c.name} = ${this.type(c.binding.equality.type)}`
					: `${c.name}: ${this.bounds(c.binding.constraint)}`,
			),
		]
		return parts.length === 0 ? '' : `<${parts.join(', ')}>`
	}

	#pointer(pointer: Json): string {
		const inputs = (pointer.sig.inputs as [string, Json][]).map(([, t]) => this.type(t)).join(', ')
		const output = pointer.sig.output === null ? '' : ` -> ${this.type(pointer.sig.output)}`
		return `fn(${inputs})${output}`
	}
}

// `std::time::Duration` reads as `Duration`, as the code that uses it writes it
function shortPath(path: string): string {
	return path.split('::').at(-1) ?? path
}

// rustdoc runs a fence with no language, or with one of its own attributes, as Rust
function isRust(lang: string): boolean {
	return lang === '' || /^(rust|no_run|ignore|should_panic|compile_fail|edition\d+)\b/.test(lang)
}

/**
 * A doc comment as the site's markdown: an intra-doc link `[`Bucket`]` as
 * its code, a fence without a language as Rust, and the lines rustdoc hides
 * in an example, `# use …`, left out.
 */
export function docOf(docs: string | null): string {
	if (docs === null) {
		return ''
	}
	// the fence a line is in: none, Rust, or another language's
	let fence: 'none' | 'rust' | 'other' = 'none'
	return docs
		.split('\n')
		.flatMap(line => {
			const open = /^\s*```(.*)$/.exec(line)
			if (open !== null) {
				if (fence !== 'none') {
					fence = 'none'
					return [line]
				}
				fence = isRust(open[1]?.trim() ?? '') ? 'rust' : 'other'
				return [fence === 'rust' ? '```rust' : line]
			}
			if (fence === 'rust' && /^\s*#(\s|$)/.test(line)) {
				return []
			}
			return [fence === 'none' ? line.replace(/\[(`[^`\]]+`)\](?![([])/g, '$1') : line]
		})
		.join('\n')
		.trim()
}
