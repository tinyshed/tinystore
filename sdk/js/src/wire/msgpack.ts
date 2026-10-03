// The MessagePack profile of docs/wire.md, written for this SDK rather than
// taken from a library: a general one accepts a float 32, an extension, a key
// twice or a value nested nine deep, which the profile refuses, and rounds a
// 64-bit integer unless told. An encoder writes the shortest form of every
// integer, length and count and every float in eight bytes, so that a vector
// compares byte for byte; a decoder takes any valid form.

import { InvalidError, ProtocolError } from '../errors.ts'

const NIL = 0xc0
const FALSE = 0xc2
const TRUE = 0xc3
const BIN8 = 0xc4
const BIN16 = 0xc5
const BIN32 = 0xc6
const FLOAT64 = 0xcb
const UINT8 = 0xcc
const UINT16 = 0xcd
const UINT32 = 0xce
const UINT64 = 0xcf
const INT8 = 0xd0
const INT16 = 0xd1
const INT32 = 0xd2
const INT64 = 0xd3
const STR8 = 0xd9
const STR16 = 0xda
const STR32 = 0xdb
const ARRAY16 = 0xdc
const ARRAY32 = 0xdd
const MAP16 = 0xde
const MAP32 = 0xdf
const FIXMAP = 0x80
const FIXARRAY = 0x90
const FIXSTR = 0xa0
const NEGFIX = 0xe0

/** How deeply maps and arrays may nest in a message. */
export const maxDepth = 8

const encoder = new TextEncoder()

// fatal refuses bytes that are not UTF-8, and ignoreBOM keeps a U+FEFF that
// begins a str as the character it is, where the default drops it
const decoder = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true })

const MAX_SAFE = BigInt(Number.MAX_SAFE_INTEGER)
const MIN_SAFE = BigInt(Number.MIN_SAFE_INTEGER)

/** Writes one message, its buffer growing as it must. */
export class Writer {
	#bytes: Uint8Array
	#view: DataView
	#length = 0

	constructor(capacity = 128) {
		this.#bytes = new Uint8Array(capacity)
		this.#view = new DataView(this.#bytes.buffer)
	}

	get length(): number {
		return this.#length
	}

	/** A copy of what was written. */
	bytes(): Uint8Array {
		return this.#bytes.slice(0, this.#length)
	}

	/** Makes room for n bytes at the end and says where they begin. */
	#room(n: number): number {
		const at = this.#length
		const need = at + n
		if (need > this.#bytes.length) {
			let size = this.#bytes.length * 2
			while (size < need) {
				size *= 2
			}
			const grown = new Uint8Array(size)
			grown.set(this.#bytes.subarray(0, at))
			this.#bytes = grown
			this.#view = new DataView(grown.buffer)
		}
		this.#length = need
		return at
	}

	// room first: in this.#bytes[this.#room(1)] the old buffer is read before
	// #room grows it, and a byte at 128, 256, 512… went into the old one
	#byte(b: number): void {
		const at = this.#room(1)
		this.#bytes[at] = b
	}

	nil(): void {
		this.#byte(NIL)
	}

	bool(v: boolean): void {
		this.#byte(v ? TRUE : FALSE)
	}

	/**
	 * Writes a value from 0 in the unsigned family's shortest form:
	 *
	 *     7 → 07    300 → cd 01 2c    70000 → ce 00 01 11 70
	 */
	uint(v: number | bigint): void {
		if (typeof v === 'bigint') {
			if (v < 0n || v > 0xffff_ffff_ffff_ffffn) {
				throw new InvalidError(`${v} is not an unsigned 64-bit integer`)
			}
			if (v > MAX_SAFE) {
				const at = this.#room(9)
				this.#bytes[at] = UINT64
				this.#view.setBigUint64(at + 1, v)
				return
			}
			v = Number(v)
		}
		if (!Number.isSafeInteger(v) || v < 0) {
			throw new InvalidError(
				`${v} is not an unsigned integer a number holds exactly; give a bigint`,
			)
		}
		if (v <= 0x7f) {
			this.#byte(v)
		} else if (v <= 0xff) {
			const at = this.#room(2)
			this.#bytes[at] = UINT8
			this.#bytes[at + 1] = v
		} else if (v <= 0xffff) {
			const at = this.#room(3)
			this.#bytes[at] = UINT16
			this.#view.setUint16(at + 1, v)
		} else if (v <= 0xffff_ffff) {
			const at = this.#room(5)
			this.#bytes[at] = UINT32
			this.#view.setUint32(at + 1, v)
		} else {
			const at = this.#room(9)
			this.#bytes[at] = UINT64
			this.#view.setBigUint64(at + 1, BigInt(v))
		}
	}

	/**
	 * Writes a value from 0 as uint does, and a negative one in the signed
	 * family's shortest form:
	 *
	 *     -1 → ff    -33 → d0 df    -300 → d1 fe d4
	 */
	int(v: number | bigint): void {
		if (typeof v === 'bigint') {
			if (v >= 0n) {
				this.uint(v)
				return
			}
			if (v < -0x8000_0000_0000_0000n) {
				throw new InvalidError(`${v} is not a 64-bit integer`)
			}
			if (v < MIN_SAFE) {
				const at = this.#room(9)
				this.#bytes[at] = INT64
				this.#view.setBigInt64(at + 1, v)
				return
			}
			v = Number(v)
		}
		if (!Number.isSafeInteger(v)) {
			throw new InvalidError(`${v} is not an integer a number holds exactly; give a bigint`)
		}
		if (v >= 0) {
			this.uint(v)
		} else if (v >= -32) {
			this.#byte(v & 0xff)
		} else if (v >= -0x80) {
			const at = this.#room(2)
			this.#bytes[at] = INT8
			this.#view.setInt8(at + 1, v)
		} else if (v >= -0x8000) {
			const at = this.#room(3)
			this.#bytes[at] = INT16
			this.#view.setInt16(at + 1, v)
		} else if (v >= -0x8000_0000) {
			const at = this.#room(5)
			this.#bytes[at] = INT32
			this.#view.setInt32(at + 1, v)
		} else {
			const at = this.#room(9)
			this.#bytes[at] = INT64
			this.#view.setBigInt64(at + 1, BigInt(v))
		}
	}

	/** Writes a float 64 with its bits: -0 → cb 80 00 00 00 00 00 00 00. */
	float(v: number): void {
		const at = this.#room(9)
		this.#bytes[at] = FLOAT64
		this.#view.setFloat64(at + 1, v)
	}

	/**
	 * Writes text as UTF-8. A string holding a lone surrogate has no UTF-8 to
	 * write and is refused, rather than changed into U+FFFD.
	 */
	str(s: string): void {
		if (!s.isWellFormed()) {
			throw new InvalidError('a string with a lone surrogate, which UTF-8 cannot spell')
		}
		// room for the longest UTF-8 the string may take behind the longest
		// header that length needs; the header shrinks to its shortest after
		const most = s.length * 3
		const header = headerOf(most)
		const at = this.#room(header + most)
		const { written } = encoder.encodeInto(s, this.#bytes.subarray(at + header))
		const shortest = headerOf(written)
		if (shortest !== header) {
			this.#bytes.copyWithin(at + shortest, at + header, at + header + written)
		}
		this.#length = at + shortest + written
		this.#strHeader(at, written)
	}

	#strHeader(at: number, n: number): void {
		if (n <= 31) {
			this.#bytes[at] = FIXSTR | n
		} else if (n <= 0xff) {
			this.#bytes[at] = STR8
			this.#bytes[at + 1] = n
		} else if (n <= 0xffff) {
			this.#bytes[at] = STR16
			this.#view.setUint16(at + 1, n)
		} else {
			this.#bytes[at] = STR32
			this.#view.setUint32(at + 1, n)
		}
	}

	bin(b: Uint8Array): void {
		const n = b.length
		let at: number
		if (n <= 0xff) {
			at = this.#room(2 + n)
			this.#bytes[at] = BIN8
			this.#bytes[at + 1] = n
			at += 2
		} else if (n <= 0xffff) {
			at = this.#room(3 + n)
			this.#bytes[at] = BIN16
			this.#view.setUint16(at + 1, n)
			at += 3
		} else {
			at = this.#room(5 + n)
			this.#bytes[at] = BIN32
			this.#view.setUint32(at + 1, n)
			at += 5
		}
		this.#bytes.set(b, at)
	}

	array(n: number): void {
		this.#count(n, FIXARRAY, ARRAY16, ARRAY32)
	}

	map(n: number): void {
		this.#count(n, FIXMAP, MAP16, MAP32)
	}

	#count(n: number, fix: number, two: number, four: number): void {
		if (n <= 15) {
			this.#byte(fix | n)
		} else if (n <= 0xffff) {
			const at = this.#room(3)
			this.#bytes[at] = two
			this.#view.setUint16(at + 1, n)
		} else {
			const at = this.#room(5)
			this.#bytes[at] = four
			this.#view.setUint32(at + 1, n)
		}
	}
}

/** The bytes a str's header takes for a length. */
function headerOf(n: number): number {
	if (n <= 31) {
		return 1
	}
	if (n <= 0xff) {
		return 2
	}
	return n <= 0xffff ? 3 : 5
}

/** The kind of the next value a Reader holds. */
export type Type = 'nil' | 'bool' | 'int' | 'float' | 'str' | 'bin' | 'array' | 'map' | 'invalid'

/** The type a value's first byte says. */
export function typeOf(b: number): Type {
	if (b <= 0x7f || b >= NEGFIX || (b >= UINT8 && b <= INT64)) {
		return 'int'
	}
	if ((b & 0xf0) === FIXMAP || b === MAP16 || b === MAP32) {
		return 'map'
	}
	if ((b & 0xf0) === FIXARRAY || b === ARRAY16 || b === ARRAY32) {
		return 'array'
	}
	if ((b & 0xe0) === FIXSTR || (b >= STR8 && b <= STR32)) {
		return 'str'
	}
	if (b >= BIN8 && b <= BIN32) {
		return 'bin'
	}
	if (b === NIL) {
		return 'nil'
	}
	if (b === FALSE || b === TRUE) {
		return 'bool'
	}
	// a float 32, an extension, or the byte never used
	return b === FLOAT64 ? 'float' : 'invalid'
}

const typeNames: Record<Type, string> = {
	nil: 'nil',
	bool: 'a bool',
	int: 'an integer',
	float: 'a float',
	str: 'a str',
	bin: 'a bin',
	array: 'an array',
	map: 'a map',
	invalid: 'an invalid type',
}

/**
 * Reads one message and checks the profile as it goes: a value the profile
 * refuses, one past the bytes left, or bytes after the message throw a
 * ProtocolError. What it returns of the body, a bin, is a view of the body.
 */
export class Reader {
	readonly #bytes: Uint8Array
	readonly #view: DataView
	#at = 0
	#depth = 0

	constructor(bytes: Uint8Array) {
		this.#bytes = bytes
		this.#view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)
	}

	/** Refuses bytes left after the one value a message is. */
	end(): void {
		if (this.#at !== this.#bytes.length) {
			throw fail(`${this.#bytes.length - this.#at} bytes after the message`)
		}
	}

	/** The next value's type, without reading it. */
	type(): Type {
		const b = this.#bytes[this.#at]
		return b === undefined ? 'invalid' : typeOf(b)
	}

	#next(): number {
		const b = this.#bytes[this.#at]
		if (b === undefined) {
			throw fail('the body ends inside a value')
		}
		this.#at++
		return b
	}

	#take(n: number): number {
		const left = this.#bytes.length - this.#at
		if (n > left) {
			throw fail(`${n} bytes asked for where ${left} are left`)
		}
		const at = this.#at
		this.#at += n
		return at
	}

	/**
	 * Reads any encoding of an integer as a bigint; unsigned says it came in the
	 * unsigned family, so that a uint 64 past the int 64 range reads as its value
	 */
	#integer(want: string): bigint {
		const b = this.#next()
		if (b <= 0x7f) {
			return BigInt(b)
		}
		if (b >= NEGFIX) {
			return BigInt(b - 0x100)
		}
		switch (b) {
			case UINT8:
				return BigInt(this.#bytes[this.#take(1)]!)
			case UINT16:
				return BigInt(this.#view.getUint16(this.#take(2)))
			case UINT32:
				return BigInt(this.#view.getUint32(this.#take(4)))
			case UINT64:
				return this.#view.getBigUint64(this.#take(8))
			case INT8:
				return BigInt(this.#view.getInt8(this.#take(1)))
			case INT16:
				return BigInt(this.#view.getInt16(this.#take(2)))
			case INT32:
				return BigInt(this.#view.getInt32(this.#take(4)))
			case INT64:
				return this.#view.getBigInt64(this.#take(8))
		}
		throw fail(`${typeNames[typeOf(b)]} where ${want} belongs`)
	}

	/** Reads an unsigned integer in any encoding of its value, a bigint. */
	uint64(): bigint {
		const v = this.#integer('an unsigned integer')
		if (v < 0n) {
			throw fail(`${v} where an unsigned integer belongs`)
		}
		return v
	}

	/** Reads a signed 64-bit integer in any encoding of its value, a bigint. */
	int64(): bigint {
		const v = this.#integer('an integer')
		if (v > 0x7fff_ffff_ffff_ffffn) {
			throw fail(`${v} where an int64 belongs`)
		}
		return v
	}

	/** Reads an unsigned integer a number holds exactly. */
	uint(): number {
		const v = this.uint64()
		if (v > MAX_SAFE) {
			throw fail(`${v}, past what a number holds exactly, where a number belongs`)
		}
		return Number(v)
	}

	/** Reads an integer a number holds exactly. */
	int(): number {
		const v = this.int64()
		if (v > MAX_SAFE || v < MIN_SAFE) {
			throw fail(`${v}, past what a number holds exactly, where a number belongs`)
		}
		return Number(v)
	}

	/**
	 * Reads a float 64, or an integer a float 64 holds exactly, since a
	 * JavaScript encoder may write 3.0 as 3.
	 */
	float(): number {
		const t = this.type()
		if (t === 'float') {
			this.#at++
			return this.#view.getFloat64(this.#take(8))
		}
		if (t !== 'int') {
			throw fail(`${typeNames[t]} where a float belongs`)
		}
		const v = this.#integer('a float')
		const f = Number(v)
		// a float at 2^64 does not convert back to a uint 64, though BigInt reads it
		if (f >= 2 ** 64 || BigInt(f) !== v) {
			throw fail(`${v}, which a float 64 does not hold exactly`)
		}
		return f
	}

	bool(): boolean {
		const b = this.#next()
		if (b === TRUE) {
			return true
		}
		if (b !== FALSE) {
			throw fail(`${typeNames[typeOf(b)]} where a bool belongs`)
		}
		return false
	}

	/** Reads a nil if one is next and says so, leaving any other value. */
	nil(): boolean {
		if (this.type() === 'nil') {
			this.#at++
			return true
		}
		return false
	}

	/** Reads a str, which must be valid UTF-8. */
	str(): string {
		const b = this.#next()
		let n: number
		if ((b & 0xe0) === FIXSTR) {
			n = b & 0x1f
		} else if (b === STR8) {
			n = this.#bytes[this.#take(1)]!
		} else if (b === STR16) {
			n = this.#view.getUint16(this.#take(2))
		} else if (b === STR32) {
			n = this.#view.getUint32(this.#take(4))
		} else {
			throw fail(`${typeNames[typeOf(b)]} where a str belongs`)
		}
		const at = this.#take(n)
		try {
			return decoder.decode(this.#bytes.subarray(at, at + n))
		} catch {
			throw fail('a str that is not UTF-8')
		}
	}

	/** Reads a bin as a view of the body; an empty one is an empty array. */
	bin(): Uint8Array {
		const b = this.#next()
		let n: number
		if (b === BIN8) {
			n = this.#bytes[this.#take(1)]!
		} else if (b === BIN16) {
			n = this.#view.getUint16(this.#take(2))
		} else if (b === BIN32) {
			n = this.#view.getUint32(this.#take(4))
		} else {
			throw fail(`${typeNames[typeOf(b)]} where a bin belongs`)
		}
		const at = this.#take(n)
		return this.#bytes.subarray(at, at + n)
	}

	/**
	 * Reads an array's count and enters it; leave once its elements are read.
	 * A count past the bytes left is refused before anything is made of it,
	 * since every element takes at least a byte.
	 */
	array(): number {
		return this.#enter('array', FIXARRAY, ARRAY16, ARRAY32, 1)
	}

	/** Reads a map's count and enters it, each entry taking two bytes at least. */
	map(): number {
		return this.#enter('map', FIXMAP, MAP16, MAP32, 2)
	}

	leave(): void {
		this.#depth--
	}

	#enter(kind: Type, fix: number, two: number, four: number, each: number): number {
		const b = this.#next()
		let n: number
		if ((b & 0xf0) === fix) {
			n = b & 0x0f
		} else if (b === two) {
			n = this.#view.getUint16(this.#take(2))
		} else if (b === four) {
			n = this.#view.getUint32(this.#take(4))
		} else {
			throw fail(`${typeNames[typeOf(b)]} where ${typeNames[kind]} belongs`)
		}
		const left = this.#bytes.length - this.#at
		if (n * each > left) {
			throw fail(`${typeNames[kind]} of ${n} elements in ${left} bytes`)
		}
		if (this.#depth === maxDepth) {
			throw fail(`${typeNames[kind]} nested deeper than ${maxDepth}`)
		}
		this.#depth++
		return n
	}

	/**
	 * Reads a map with unsigned keys, a message or a field of one, calling each
	 * with its key and the value next: it reads the value, or returns false to
	 * have it skipped, well formed and within bounds. A key twice is refused.
	 */
	fields(each: (key: number) => boolean): void {
		const n = this.map()
		const seen = new Keys()
		for (let i = 0; i < n; i++) {
			const key = this.uint()
			if (!seen.add(key)) {
				throw fail(`key ${key} twice`)
			}
			const at = this.#at
			if (!each(key) || this.#at === at) {
				this.#at = at
				this.skip()
			}
		}
		this.leave()
	}

	/** Reads a map of names, an error's what or a series' labels, as fields does. */
	names(each: (name: string) => void): void {
		const n = this.map()
		const seen = new Set<string>()
		for (let i = 0; i < n; i++) {
			const name = this.str()
			if (seen.has(name)) {
				throw fail(`name ${JSON.stringify(name)} twice`)
			}
			seen.add(name)
			const at = this.#at
			each(name)
			if (this.#at === at) {
				this.skip()
			}
		}
		this.leave()
	}

	/** Reads an array, calling each with an element's index and the element next. */
	items(each: (index: number) => void): void {
		const n = this.array()
		for (let i = 0; i < n; i++) {
			const at = this.#at
			each(i)
			if (this.#at === at) {
				this.skip()
			}
		}
		this.leave()
	}

	/** Reads past one value, checking it is well formed as if it were read. */
	skip(): void {
		switch (this.type()) {
			case 'nil':
			case 'bool':
				this.#at++
				return
			case 'int':
				this.#integer('an integer')
				return
			case 'float':
				this.float()
				return
			case 'str':
				this.str()
				return
			case 'bin':
				this.bin()
				return
			case 'array':
				this.items(() => this.skip())
				return
			case 'map':
				this.#skipMap()
				return
		}
		throw fail(this.#describe())
	}

	// a map's keys are unsigned integers or names, as a message's maps are
	#skipMap(): void {
		const at = this.#at
		const n = this.map()
		const named = n > 0 && this.type() === 'str'
		this.#at = at
		this.#depth--
		if (named) {
			this.names(() => this.skip())
		} else {
			this.fields(() => false)
		}
	}

	#describe(): string {
		const b = this.#bytes[this.#at]
		if (b === undefined) {
			return 'the body ends where a value belongs'
		}
		if (b === 0xca) {
			return 'a float 32, which the profile does not allow'
		}
		if (b === 0xc1) {
			return 'the type byte 0xc1, which MessagePack never uses'
		}
		return `the extension type 0x${b.toString(16)}, which the profile does not allow`
	}
}

/** The keys a map has shown: those under 32 in bits, the rest in a set. */
class Keys {
	#low = 0
	#high: Set<number> | undefined

	add(key: number): boolean {
		if (key < 32) {
			const bit = 1 << key
			const fresh = (this.#low & bit) === 0
			this.#low |= bit
			return fresh
		}
		this.#high ??= new Set()
		if (this.#high.has(key)) {
			return false
		}
		this.#high.add(key)
		return true
	}
}

function fail(reason: string): ProtocolError {
	return new ProtocolError(`invalid message: ${reason}`)
}
