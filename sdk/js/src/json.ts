// JSON as JSON.parse reads it, but an integer past 2^53 a bigint, as an SQL
// answer's is: a number would round it, and an id of 19 digits must come back
// as it went in.

/** Reads `text`, one JSON value; what is no JSON throws a SyntaxError, as JSON.parse does. */
export function parseJson(text: string): unknown {
	const reading = new Reading(text)
	const value = reading.value()
	reading.space()
	if (reading.at !== text.length) {
		throw new SyntaxError(`JSON past its value at ${reading.at}`)
	}
	return value
}

const number = /-?(?:0|[1-9]\d*)(\.\d+)?([eE][+-]?\d+)?/y

class Reading {
	at = 0
	readonly #text: string

	constructor(text: string) {
		this.#text = text
	}

	space(): void {
		while (this.at < this.#text.length && ' \t\n\r'.includes(this.#text[this.at] as string)) {
			this.at++
		}
	}

	value(): unknown {
		this.space()
		const first = this.#text[this.at]
		switch (first) {
			case '{':
				return this.#object()
			case '[':
				return this.#array()
			case '"':
				return this.#string()
		}
		for (const [word, value] of [
			['true', true],
			['false', false],
			['null', null],
		] as const) {
			if (this.#text.startsWith(word, this.at)) {
				this.at += word.length
				return value
			}
		}
		return this.#number()
	}

	#object(): Record<string, unknown> {
		const object: Record<string, unknown> = {}
		this.at++
		this.space()
		if (this.#text[this.at] === '}') {
			this.at++
			return object
		}
		for (;;) {
			this.space()
			const key = this.#string()
			this.space()
			this.#expect(':')
			object[key] = this.value()
			this.space()
			if (this.#text[this.at] === '}') {
				this.at++
				return object
			}
			this.#expect(',')
		}
	}

	#array(): unknown[] {
		const array: unknown[] = []
		this.at++
		this.space()
		if (this.#text[this.at] === ']') {
			this.at++
			return array
		}
		for (;;) {
			array.push(this.value())
			this.space()
			if (this.#text[this.at] === ']') {
				this.at++
				return array
			}
			this.#expect(',')
		}
	}

	/** A string, its escapes read by JSON.parse, which knows them all. */
	#string(): string {
		this.#expect('"', false)
		let end = this.at + 1
		while (end < this.#text.length && this.#text[end] !== '"') {
			end += this.#text[end] === '\\' ? 2 : 1
		}
		if (end >= this.#text.length) {
			throw new SyntaxError(`a JSON string not ended, from ${this.at}`)
		}
		const read = JSON.parse(this.#text.slice(this.at, end + 1)) as string
		this.at = end + 1
		return read
	}

	#number(): number | bigint {
		number.lastIndex = this.at
		const found = number.exec(this.#text)
		if (found === null) {
			throw new SyntaxError(`no JSON value at ${this.at}`)
		}
		this.at += found[0].length
		const read = Number(found[0])
		const whole = found[1] === undefined && found[2] === undefined
		return whole && !Number.isSafeInteger(read) ? BigInt(found[0]) : read
	}

	#expect(char: string, skip = true): void {
		if (this.#text[this.at] !== char) {
			throw new SyntaxError(`JSON wants ${char} at ${this.at}`)
		}
		if (skip) {
			this.at++
		}
	}
}
