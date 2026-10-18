// What the suites take from bun:test, over node:test and Jest's expect: the
// same names, Bun's third argument a test's timeout, and its skipIf.

import {
	after,
	before,
	afterEach as nodeAfterEach,
	beforeEach as nodeBeforeEach,
	describe as nodeDescribe,
	test as nodeTest,
} from 'node:test'
import { expect as jestExpect } from 'expect'

type Body = () => unknown
type Options = { timeout?: number }

function options(timeout: number | Options | undefined): Options {
	return typeof timeout === 'number' ? { timeout } : (timeout ?? {})
}

function testOf(skip: boolean) {
	return (name: string, body: Body, timeout?: number | Options) =>
		nodeTest(name, { ...options(timeout), skip }, async () => {
			await body()
		})
}

export const test = Object.assign(testOf(false), {
	skip: testOf(true),
	skipIf: (skip: unknown) => testOf(Boolean(skip)),
	if: (run: unknown) => testOf(!run),
})
export const it = test

function describeOf(skip: boolean) {
	return (name: string, body: () => void) => nodeDescribe(name, { skip }, body)
}

export const describe = Object.assign(describeOf(false), {
	skip: describeOf(true),
	skipIf: (skip: unknown) => describeOf(Boolean(skip)),
	if: (run: unknown) => describeOf(!run),
})

export const beforeAll = (body: Body) => before(async () => void (await body()))
export const afterAll = (body: Body) => after(async () => void (await body()))
export const beforeEach = (body: Body) => nodeBeforeEach(async () => void (await body()))
export const afterEach = (body: Body) => nodeAfterEach(async () => void (await body()))

jestExpect.extend({
	toStartWith: (received: string, prefix: string) => ({
		pass: typeof received === 'string' && received.startsWith(prefix),
		message: () => `expected ${JSON.stringify(received)} to start with ${JSON.stringify(prefix)}`,
	}),
	toEndWith: (received: string, suffix: string) => ({
		pass: typeof received === 'string' && received.endsWith(suffix),
		message: () => `expected ${JSON.stringify(received)} to end with ${JSON.stringify(suffix)}`,
	}),
})

/** Bun's expect takes a message as its second argument, which Jest's refuses. */
export const expect = Object.assign(
	(value: unknown, _message?: string) => jestExpect(value),
	jestExpect,
) as unknown as typeof import('bun:test').expect

/** A method watched in place, as Bun's spyOn: its calls' results, and its restore. */
export function spyOn<T extends object, K extends keyof T>(object: T, method: K) {
	const original = object[method] as (...args: unknown[]) => unknown
	const results: { type: 'return' | 'throw'; value: unknown }[] = []
	object[method] = function (this: unknown, ...args: unknown[]) {
		try {
			const value = original.apply(this, args)
			results.push({ type: 'return', value })
			return value
		} catch (err) {
			results.push({ type: 'throw', value: err })
			throw err
		}
	} as T[K]
	return {
		mock: { results },
		mockRestore: () => {
			object[method] = original as T[K]
		},
	}
}
