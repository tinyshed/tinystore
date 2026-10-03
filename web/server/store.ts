import { mkdirSync } from 'node:fs'
import { open, type Store } from 'tinystore'

/**
 * The directory's sidecar, started when none runs, so that while the site
 * serves, `tinystore logs <dir> -f` shows its views as they arrive and
 * `tinystore mcp <dir>` lets an agent read them. The binary is TINYSTORE_BIN's,
 * or the one on PATH.
 */
export async function openStore(dir: string): Promise<Store> {
	mkdirSync(dir, { recursive: true })
	return open(dir)
}
