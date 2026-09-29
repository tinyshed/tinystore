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
	const view = new DataView(bytes.buffer, bytes.byteOffset + at, headerSize)
	return {
		length: view.getUint32(0, true),
		kind: view.getUint8(4),
		flags: view.getUint8(5),
		method: view.getUint16(6, true),
		stream: view.getUint32(8, true),
	}
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
	const view = new DataView(bytes.buffer)
	view.setUint32(0, body.length, true)
	view.setUint8(4, kind)
	view.setUint8(5, flags)
	view.setUint16(6, method, true)
	view.setUint32(8, stream, true)
	bytes.set(body, headerSize)
	return bytes
}

/** A CREDIT granting n bytes on a stream. */
export function credit(stream: number, n: number): Uint8Array {
	const body = new Uint8Array(4)
	new DataView(body.buffer).setUint32(0, n, true)
	return frame(Kind.credit, 0, 0, stream, body)
}

export function granted(body: Uint8Array): number {
	return new DataView(body.buffer, body.byteOffset, 4).getUint32(0, true)
}
