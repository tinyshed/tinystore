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

// Where a 64-bit integer or a float is laid out. A DataView over each
// message's own bytes cost more than the message: asking a small array for
// its buffer, or for a view of a part of it, makes the engine move it to one,
// for every call and answer. So a small part is read as a copy, and short
// text is written a byte at a time.
const eight = new DataView(new ArrayBuffer(8))
const eightBytes = new Uint8Array(eight.buffer)

const MAX_SAFE = BigInt(Number.MAX_SAFE_INTEGER)
const MIN_SAFE = BigInt(Number.MIN_SAFE_INTEGER)

/** Writes one message, its buffer growing as it must. */
export class Writer {
	#bytes: Uint8Array
	#length = 0

	constructor(capacity = 128) {
		this.#bytes = new Uint8Array(capacity)
	}

	get length(): number {
		return this.#length
	}

	/** A copy of what was written. */
	bytes(): Uint8Array {
		return this.#bytes.slice(0, this.#length)
	}

	/** Where the bytes are written, with room past them: the same array until a write outgrows it. */
	get buffer(): Uint8Array {
		return this.#bytes
	}

	/** Makes room for n bytes the caller writes into the buffer, and says where they begin. */
	reserve(n: number): number {
		return this.#room(n)
	}

	/** Writes bytes as they are, a message another writer made. */
	raw(bytes: Uint8Array): void {
		const at = this.#room(bytes.length)
		this.#bytes.set(bytes, at)
	}

	/** Forgets what was written past a length. */
	truncate(length: number): void {
		this.#length = length
	}

	/** Writes a map's count of 15 pairs or fewer in the byte reserved for it. */
	mapAt(at: number, n: number): void {
		this.#bytes[at] = FIXMAP | n
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
			grown.set(this.#bytes)
			this.#bytes = grown
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

	/** Writes two bytes at a place, the high one first; a byte keeps the low eight bits. */
	#two(at: number, v: number): void {
		this.#bytes[at] = v >>> 8
		this.#bytes[at + 1] = v
	}

	#four(at: number, v: number): void {
		this.#bytes[at] = v >>> 24
		this.#bytes[at + 1] = v >>> 16
		this.#bytes[at + 2] = v >>> 8
		this.#bytes[at + 3] = v
	}

	/** Writes the low 64 bits of an integer: a negative one in two's complement. */
	#eight(at: number, v: bigint): void {
		eight.setBigUint64(0, v)
		this.#bytes.set(eightBytes, at)
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
				this.#eight(at + 1, v)
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
			this.#two(at + 1, v)
		} else if (v <= 0xffff_ffff) {
			const at = this.#room(5)
			this.#bytes[at] = UINT32
			this.#four(at + 1, v)
		} else {
			const at = this.#room(9)
			this.#bytes[at] = UINT64
			this.#eight(at + 1, BigInt(v))
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
				this.#eight(at + 1, v)
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
			this.#bytes[at + 1] = v
		} else if (v >= -0x8000) {
			const at = this.#room(3)
			this.#bytes[at] = INT16
			this.#two(at + 1, v)
		} else if (v >= -0x8000_0000) {
			const at = this.#room(5)
			this.#bytes[at] = INT32
			this.#four(at + 1, v)
		} else {
			const at = this.#room(9)
			this.#bytes[at] = INT64
			this.#eight(at + 1, BigInt(v))
		}
	}

	/** Writes a float 64 with its bits: -0 → cb 80 00 00 00 00 00 00 00. */
	float(v: number): void {
		const at = this.#room(9)
		this.#bytes[at] = FLOAT64
		eight.setFloat64(0, v)
		this.#bytes.set(eightBytes, at + 1)
	}

	/**
	 * Writes text as UTF-8. A string holding a lone surrogate has no UTF-8 to
	 * write and is refused, rather than changed into U+FFFD.
	 */
	str(s: string): void {
		if (s.length <= shortText && this.#ascii(s)) {
			return
		}
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

	/** Writes text of ASCII alone, a byte a character, or nothing: it says which. */
	#ascii(s: string): boolean {
		const n = s.length
		const header = headerOf(n)
		const start = this.#length
		const at = this.#room(header + n) + header
		for (let i = 0; i < n; i++) {
			const c = s.charCodeAt(i)
			if (c > 0x7f) {
				this.#length = start
				return false
			}
			this.#bytes[at + i] = c
		}
		this.#strHeader(start, n)
		return true
	}

	#strHeader(at: number, n: number): void {
		if (n <= 31) {
			this.#bytes[at] = FIXSTR | n
		} else if (n <= 0xff) {
			this.#bytes[at] = STR8
			this.#bytes[at + 1] = n
		} else if (n <= 0xffff) {
			this.#bytes[at] = STR16
			this.#two(at + 1, n)
		} else {
			this.#bytes[at] = STR32
			this.#four(at + 1, n)
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
			this.#two(at + 1, n)
			at += 3
		} else {
			at = this.#room(5 + n)
			this.#bytes[at] = BIN32
			this.#four(at + 1, n)
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
			this.#two(at + 1, n)
		} else {
			const at = this.#room(5)
			this.#bytes[at] = four
			this.#four(at + 1, n)
		}
	}
}

/** Text up to this long is written a byte at a time while it is ASCII. */
const shortText = 64
/** A part of a message up to this long is read as a copy, a longer one as a view. */
const smallPart = 256

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
	#bytes: Uint8Array
	#at: number
	#end: number
	#depth = 0

	/** Reads a message that is the bytes from `at` to `end`: all of them unless said. */
	constructor(bytes: Uint8Array, at = 0, end = bytes.length) {
		this.#bytes = bytes
		this.#at = at
		this.#end = end
	}

	/** Turns to another message, for a reader kept from one answer to the next. */
	reset(bytes: Uint8Array, at: number, end: number): void {
		this.#bytes = bytes
		this.#at = at
		this.#end = end
		this.#depth = 0
	}

	/** Refuses bytes left after the one value a message is. */
	end(): void {
		if (this.#at !== this.#end) {
			throw fail(`${this.#end - this.#at} bytes after the message`)
		}
	}

	/** The next value's type, without reading it. */
	type(): Type {
		const b = this.#peek()
		return b === undefined ? 'invalid' : typeOf(b)
	}

	/** The next byte, or undefined at the message's end: past it lie another message's bytes. */
	#peek(): number | undefined {
		return this.#at < this.#end ? this.#bytes[this.#at] : undefined
	}

	#next(): number {
		const b = this.#peek()
		if (b === undefined) {
			throw fail('the body ends inside a value')
		}
		this.#at++
		return b
	}

	#take(n: number): number {
		const left = this.#end - this.#at
		if (n > left) {
			throw fail(`${n} bytes asked for where ${left} are left`)
		}
		const at = this.#at
		this.#at += n
		return at
	}

	/** Reads two bytes as an unsigned integer, the high one first. */
	#two(): number {
		const at = this.#take(2)
		return (this.#bytes[at]! << 8) | this.#bytes[at + 1]!
	}

	#four(): number {
		const at = this.#take(4)
		const bytes = this.#bytes
		return (
			((bytes[at]! << 24) | (bytes[at + 1]! << 16) | (bytes[at + 2]! << 8) | bytes[at + 3]!) >>> 0
		)
	}

	/** Lays the next eight bytes out to be read as 64 bits. */
	#eight(): DataView {
		const at = this.#take(8)
		for (let i = 0; i < 8; i++) {
			eightBytes[i] = this.#bytes[at + i]!
		}
		return eight
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
				return BigInt(this.#two())
			case UINT32:
				return BigInt(this.#four())
			case UINT64:
				return this.#eight().getBigUint64(0)
			case INT8:
				return BigInt((this.#bytes[this.#take(1)]! << 24) >> 24)
			case INT16:
				return BigInt((this.#two() << 16) >> 16)
			case INT32:
				return BigInt(this.#four() | 0)
			case INT64:
				return this.#eight().getBigInt64(0)
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
		// the one byte nearly every integer takes, read without a bigint
		const b = this.#peek()
		if (b !== undefined && b <= 0x7f) {
			this.#at++
			return b
		}
		const v = this.uint64()
		if (v > MAX_SAFE) {
			throw fail(`${v}, past what a number holds exactly, where a number belongs`)
		}
		return Number(v)
	}

	/** Reads an integer a number holds exactly. */
	int(): number {
		const b = this.#peek()
		if (b !== undefined && (b <= 0x7f || b >= NEGFIX)) {
			this.#at++
			return b <= 0x7f ? b : b - 0x100
		}
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
			return this.#eight().getFloat64(0)
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
			n = this.#two()
		} else if (b === STR32) {
			n = this.#four()
		} else {
			throw fail(`${typeNames[typeOf(b)]} where a str belongs`)
		}
		const at = this.#take(n)
		const stop = at + n
		try {
			return decoder.decode(
				n <= smallPart ? this.#bytes.slice(at, stop) : this.#bytes.subarray(at, stop),
			)
		} catch {
			throw fail('a str that is not UTF-8')
		}
	}

	/** Reads a bin as a view of the body; an empty one is an empty array. */
	bin(): Uint8Array {
		const n = this.#binLength()
		const at = this.#take(n)
		return this.#bytes.subarray(at, at + n)
	}

	/** Reads a bin as bytes of its own, which outlive the body. */
	bytes(): Uint8Array {
		const n = this.#binLength()
		const at = this.#take(n)
		return this.#bytes.slice(at, at + n)
	}

	#binLength(): number {
		const b = this.#next()
		if (b === BIN8) {
			return this.#bytes[this.#take(1)]!
		}
		if (b === BIN16) {
			return this.#two()
		}
		if (b === BIN32) {
			return this.#four()
		}
		throw fail(`${typeNames[typeOf(b)]} where a bin belongs`)
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
			n = this.#two()
		} else if (b === four) {
			n = this.#four()
		} else {
			throw fail(`${typeNames[typeOf(b)]} where ${typeNames[kind]} belongs`)
		}
		const left = this.#end - this.#at
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
				this.#take(this.#binLength())
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
		const b = this.#peek()
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
