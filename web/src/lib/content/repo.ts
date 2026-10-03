import { execFileSync } from 'node:child_process'
import { existsSync, statSync } from 'node:fs'
import { posix, resolve } from 'node:path'

import { repository } from '../site'

/**
 * The checkout the site is built from. Vite, the prerender and the tests all
 * run in web/, and bundling moves every module elsewhere, so the working
 * directory is the one place that still says where the repository is.
 */
export function checkout(): string {
	return resolve(process.cwd(), '..')
}

/** Whether a path of the repository, posix and relative to its top, is a file, a directory or nothing. */
export function exists(root: string, path: string): 'file' | 'directory' | undefined {
	const absolute = resolve(root, path)
	if (!existsSync(absolute)) {
		return undefined
	}
	return statSync(absolute).isDirectory() ? 'directory' : 'file'
}

/**
 * Where a link written in `from` leads, as a path of the repository, or
 * undefined when it climbs out of it. `docs/kv.md` and `../server/README.md`
 * give `server/README.md`.
 */
export function resolveFrom(from: string, target: string): string | undefined {
	const joined = posix.normalize(posix.join(posix.dirname(from), target))
	if (joined === '..' || joined.startsWith('../') || posix.isAbsolute(joined)) {
		return undefined
	}
	return joined === '.' ? '' : joined.replace(/\/$/, '')
}

/** The commit a page's source links point at, so that a line anchor stays right. */
export function headCommit(root: string): string {
	return process.env.GITHUB_SHA ?? git(root, ['rev-parse', 'HEAD']) ?? 'main'
}

/** When a file last changed in the history, or undefined outside a checkout. */
export function lastChanged(root: string, path: string): string | undefined {
	return git(root, ['log', '-1', '--format=%cI', '--', path])
}

export function sourceUrl(path: string, commit: string, kind: 'file' | 'directory'): string {
	return `${repository}/${kind === 'directory' ? 'tree' : 'blob'}/${commit}/${path}`
}

export function editUrl(path: string): string {
	return `${repository}/edit/main/${path}`
}

function git(root: string, args: string[]): string | undefined {
	try {
		const out = execFileSync('git', args, {
			cwd: root,
			encoding: 'utf8',
			stdio: ['ignore', 'pipe', 'ignore'],
		})
		return out.trim() || undefined
	} catch {
		return undefined
	}
}
