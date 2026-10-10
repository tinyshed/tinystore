// A frame is a twelve-byte header, little-endian, and a body:
//
//     0             4      5       6          8             12
//     │ body length │ kind │ flags │ method   │ stream      │ body …
//       u32           u8     u8      u16        u32

import { ProtocolError } from '../errors.ts'

export const headerSize = 12

export const Kind = {
	hello: 1,
	welcome: 2,
	request: 3,
	response: 4,
	data: 5,
	cancel: 6,
	credit: 7,
	ping: 8,
	pong: 9,
	goaway: 10,
} as const

export type Kind = (typeof Kind)[keyof typeof Kind]

/** END marks the sender's last frame on its stream; ERROR, only beside END, an error's body. */
export const Flag = { end: 1, error: 2 } as const

export interface Header {
	length: number
	kind: number
	flags: number
	method: number
	stream: number
}

type StreamRule = 'none' | 'own' | 'either'

interface Rule {
	name: string
	stream: StreamRule
	flags: number
	/** the length a body must have; undefined when any will do */
	length?: number
}

const rules: Record<number, Rule> = {
	[Kind.hello]: { name: 'HELLO', stream: 'none', flags: 0 },
	[Kind.welcome]: { name: 'WELCOME', stream: 'none', flags: 0 },
	[Kind.request]: { name: 'REQUEST', stream: 'own', flags: Flag.end },
	[Kind.response]: { name: 'RESPONSE', stream: 'own', flags: Flag.end | Flag.error },
	[Kind.data]: { name: 'DATA', stream: 'own', flags: Flag.end | Flag.error },
	[Kind.cancel]: { name: 'CANCEL', stream: 'own', flags: 0, length: 0 },
	[Kind.credit]: { name: 'CREDIT', stream: 'either', flags: 0, length: 4 },
	[Kind.ping]: { name: 'PING', stream: 'none', flags: 0, length: 8 },
	[Kind.pong]: { name: 'PONG', stream: 'none', flags: 0, length: 8 },
	[Kind.goaway]: { name: 'GOAWAY', stream: 'none', flags: 0 },
}

export function kindName(kind: number): string {
	return rules[kind]?.name ?? `kind ${kind}`
}

/** Reads a header from its twelve bytes without checking it. */
export function parseHeader(bytes: Uint8Array, at = 0): Header {
	if (bytes.length - at < headerSize) {
		throw new ProtocolError(`a header of ${bytes.length - at} bytes`)
	}
	return {
		length: uint32At(bytes, at),
		kind: bytes[at + 4]!,
		flags: bytes[at + 5]!,
		method: bytes[at + 6]! | (bytes[at + 7]! << 8),
		stream: uint32At(bytes, at + 8),
	}
}

/** Writes a header where its twelve bytes were kept, once its body's length is known. */
export function putHeader(
	bytes: Uint8Array,
	at: number,
	kind: number,
	flags: number,
	method: number,
	stream: number,
	length: number,
): void {
	putUint32(bytes, at, length)
	bytes[at + 4] = kind
	bytes[at + 5] = flags
	bytes[at + 6] = method
	bytes[at + 7] = method >>> 8
	putUint32(bytes, at + 8, stream)
}

// Four bytes, the low one first. Not through a DataView: asking a small array
// for its buffer makes the engine move it to one, for every frame.
function uint32At(bytes: Uint8Array, at: number): number {
	return (
		(bytes[at]! | (bytes[at + 1]! << 8) | (bytes[at + 2]! << 16) | (bytes[at + 3]! << 24)) >>> 0
	)
}

function putUint32(bytes: Uint8Array, at: number, n: number): void {
	bytes[at] = n
	bytes[at + 1] = n >>> 8
	bytes[at + 2] = n >>> 16
	bytes[at + 3] = n >>> 24
}

/**
 * Refuses a header that breaks the protocol: an unknown kind, a flag its kind
 * does not define, ERROR without END, a method outside a REQUEST or none in
 * one, a stream where the kind names none or none where it names one, and a
 * body of a length the kind cannot have or past the agreed maximum.
 */
export function checkHeader(h: Header, maxBody: number): void {
	const rule = rules[h.kind]
	if (rule === undefined) {
		throw new ProtocolError(`a frame of kind ${h.kind}`)
	}
	const name = rule.name
	if ((h.flags & ~rule.flags) !== 0) {
		throw new ProtocolError(`flags 0x${h.flags.toString(16)} on a ${name}`)
	}
	if ((h.flags & Flag.error) !== 0 && (h.flags & Flag.end) === 0) {
		throw new ProtocolError(`ERROR without END on stream ${h.stream}`)
	}
	if (h.kind === Kind.request && h.method === 0) {
		throw new ProtocolError(`a REQUEST without a method on stream ${h.stream}`)
	}
	if (h.kind !== Kind.request && h.method !== 0) {
		throw new ProtocolError(`method 0x${h.method.toString(16)} on a ${name}`)
	}
	if ((rule.stream === 'none' && h.stream !== 0) || (rule.stream === 'own' && h.stream === 0)) {
		throw new ProtocolError(`stream ${h.stream} on a ${name}`)
	}
	if (rule.length !== undefined && h.length !== rule.length) {
		throw new ProtocolError(`a ${name} of ${h.length} bytes, not ${rule.length}`)
	}
	if (h.length > maxBody) {
		throw new ProtocolError(`a ${name} of ${h.length} bytes, ${maxBody} agreed`)
	}
}

/** Writes a frame: its header, the body's length its own, then the body. */
export function frame(
	kind: number,
	flags: number,
	method: number,
	stream: number,
	body: Uint8Array,
): Uint8Array {
	const bytes = new Uint8Array(headerSize + body.length)
	putHeader(bytes, 0, kind, flags, method, stream, body.length)
	bytes.set(body, headerSize)
	return bytes
}

/** A CREDIT granting n bytes on a stream. */
export function credit(stream: number, n: number): Uint8Array {
	const body = new Uint8Array(4)
	putUint32(body, 0, n)
	return frame(Kind.credit, 0, 0, stream, body)
}

export function granted(body: Uint8Array): number {
	return uint32At(body, 0)
}
