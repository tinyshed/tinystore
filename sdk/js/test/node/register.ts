// Runs the Bun suites under Node, as they are: `node --test --import
// ./test/node/register.ts` answers each `bun:test` import with bun-test.ts,
// and loads the preload Bun's bunfig names. No global Bun is made, since the
// SDK reads one to tell which runtime it is in.

import { registerHooks } from 'node:module'

const shim = new URL('./bun-test.ts', import.meta.url).href

registerHooks({
	resolve: (specifier, context, next) =>
		specifier === 'bun:test' ? { url: shim, shortCircuit: true } : next(specifier, context),
})

// the suites read the core's library from TINYSTORE_LIBRARY, which under Node is the addon
if (process.env.TINYSTORE_ADDON !== undefined) {
	process.env.TINYSTORE_LIBRARY = process.env.TINYSTORE_ADDON
}

await import('../binary.ts')
