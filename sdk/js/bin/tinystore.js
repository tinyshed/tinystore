#!/usr/bin/env node
// The tinystore command, as this package installs it: the binary its platform's
// package carries, run with these arguments on this console, its exit code
// this process's. `bunx @tinyshed/tinystore status ./data` and `npx @tinyshed/tinystore` need
// nothing else installed. Plain JavaScript, so that Node and Bun both run it.

import { spawn } from 'node:child_process'
import { createRequire } from 'node:module'
import { constants } from 'node:os'

const executable = process.platform === 'win32' ? 'tinystore.exe' : 'tinystore'
const platform = `@tinyshed/tinystore-${process.platform}-${process.arch}`

function packagedBinary() {
	try {
		return createRequire(import.meta.url).resolve(`${platform}/bin/${executable}`)
	} catch {
		return undefined
	}
}

// Not PATH's tinystore: that may be this very command, which would run itself.
const binary = process.env.TINYSTORE_BIN || packagedBinary()
if (binary === undefined) {
	process.stderr.write(
		`tinystore: ${platform}, which carries the binary, was not installed; ` +
			'install tinystore again without --no-optional, or set TINYSTORE_BIN\n',
	)
	process.exit(1)
}

const child = spawn(binary, process.argv.slice(2), { stdio: 'inherit' })
// A Ctrl+C reaches the binary as it reaches this process, and the binary ends
// on its own, letting its SERVE and its lock go, while this one waits. A signal
// sent to this process alone is passed on, but not on Windows, where passing
// one on ends the binary outright.
for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
	process.on(signal, () => {
		if (process.platform !== 'win32') {
			child.kill(signal)
		}
	})
}
child.on('error', err => {
	process.stderr.write(`tinystore: ${binary}: ${err.message}\n`)
	process.exit(1)
})
child.on('exit', (code, signal) => {
	process.exit(code ?? 128 + (constants.signals[signal] ?? 0))
})
