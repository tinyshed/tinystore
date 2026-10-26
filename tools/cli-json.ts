// `just cli-json`: the command line's help as data, and what each command
// prints, for the site's highlighting and explain.
//
// | written                              | from                                                         |
// |--------------------------------------|--------------------------------------------------------------|
// | web/src/lib/content/cli.json         | `tinystore help --all --json`: the help's table in help.rs   |
// | web/src/lib/content/cli-prints.json  | each command's first example that runs, run on a demo store  |
//
// The CLI's test `the_sites_copy_of_the_help_is_the_help` fails when the
// first differs from the help. The second is what a run printed, so it is
// regenerated, not checked: a time or a pid in it changes each run. A
// command a script cannot run, one that serves, listens, or needs a
// program's own config, keeps the words in `unrun` below.

import { spawn, spawnSync } from 'node:child_process'
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { words } from '../web/src/lib/content/words'

const root = new URL('..', import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1')
const content = `${root}/web/src/lib/content`

const built = spawnSync('cargo', ['build', '-q', '-p', 'tinystore-cli'], { cwd: root, stdio: 'inherit' })
if (built.status !== 0) process.exit(built.status ?? 1)
const binary = join(root, 'target', 'debug', process.platform === 'win32' ? 'tinystore.exe' : 'tinystore')

const help = run(['help', '--all', '--json'], root).stdout
writeFileSync(`${content}/cli.json`, help)
console.log('wrote web/src/lib/content/cli.json')

type Command = { name: string; examples: string[] }
const commands: Command[] = JSON.parse(help).commands

/** What a command a script cannot run prints, as its guide shows it. */
const unrun: Record<string, string> = {
	serve: [
		'  tinystore 0.2.0 · ./data',
		'',
		'  ● local    programs on this machine     pipe tinystore-5434ed53 · found through data/server/SERVE',
		'  ○ remote   off                          --listen tls://<host>:<port> serves other machines',
		'',
		'  ready in 14 ms · Ctrl+C stops · tinystore logs ./data -f · tinystore status ./data',
	].join('\n'),
	mcp: '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","serverInfo":{"name":"tinystore"}}}',
	completion: "# tinystore's Tab completion for bash: eval \"$(tinystore completion bash)\"\n_tinystore() {\n    …",
	witness: '● witness  listening on 0.0.0.0:7445 · 3 nodes',
	'node status': 'file        this node  entries  epoch  applied\nkv.db       home       1-412    3      412',
	'node promote': '● ./data  kv.db is the home now, at epoch 4',
	'node demote': '● ./data  kv.db is a copy again, of epoch 4',
	'lock hold': "(./backup.sh's own output, while the key is held, TINYSTORE_FENCE=33075587763424675636468537884673)",
	'channels subscribe': 'tinystore channels subscribe: listening on chat-events\n{"kind":"reload"}\nroom-42  {"seen":7}',
	'config explain': [
		'config chat · version 3',
		'  model.max_tokens  2048     a change, version 3, 2026-10-26 14:03:05 UTC  over 1024 from the declaration',
		'  model.name        "small"  the declaration',
		'  region: required, given by the program alone',
	].join('\n'),
	'config update': 'updated config chat to version 4: model.max_tokens=2048',
	'config reset': 'reset model.max_tokens of config chat: version 5',
	search: '● ./data  1 index\n  articles  1204 documents',
	logs: '11:02:14.502 WARN  api  slow request  requestId=7f3a ms=1200\n11:02:15.911 ERROR api  payment failed  orderId=981',
}

const work = mkdtempSync(join(tmpdir(), 'tinystore-cli-json-'))
try {
	await demo(work)
	const prints: Record<string, { example: string; prints: string }> = {}
	for (const command of commands) {
		prints[command.name] = printed(command, work)
	}
	writeFileSync(`${content}/cli-prints.json`, `${JSON.stringify(prints, null, 2)}\n`)
	console.log('wrote web/src/lib/content/cli-prints.json')
} finally {
	run(['stop', './data'], work)
	rmSync(work, { recursive: true, force: true })
}

/** The command's first example that runs and says something, and what it printed. */
function printed(command: Command, at: string): { example: string; prints: string } {
	const first = command.examples[0] ?? ''
	if (command.name in unrun) return { example: first, prints: unrun[command.name] ?? '' }
	let fallback: { example: string; prints: string } | undefined
	for (const example of command.examples) {
		// `< file` feeds stdin, and `>> file` is left out: what would go there is shown
		const [line, input] = (example.split(/ >>? /)[0] ?? '').split(' < ')
		const argv = words(line ?? '')
		if (argv[0] !== 'tinystore' || /[|>]/.test(line ?? '')) continue
		const ran = run(argv.slice(1), at, input && readFileSync(join(at, input.trim())))
		const prints = shown(ran.stdout || ran.stderr, at)
		const answer = { example, prints }
		if (ran.status === 0 && prints && !/holds no|keeps no|no .* yet/.test(prints)) return answer
		fallback ??= answer
	}
	return fallback ?? { example: first, prints: '' }
}

/** What a run printed as a reader sees it: the demo's folder as ./, a dozen lines at most. */
function shown(text: string, at: string): string {
	const folder = at.replaceAll('\\', '/')
	const lines = text.replaceAll('\r\n', '\n').replaceAll(at, '.').replaceAll(folder, '.').trimEnd().split('\n')
	return (lines.length > 12 ? [...lines.slice(0, 12), '…'] : lines).join('\n')
}

function run(args: string[], cwd: string, input?: Buffer | string) {
	const ran = spawnSync(binary, args, { cwd, input, encoding: 'utf8', timeout: 30_000 })
	return { status: ran.status, stdout: ran.stdout ?? '', stderr: ran.stderr ?? '' }
}

/** A store at `at/data` that holds what the examples name, written by the command line itself. */
async function demo(at: string) {
	const made = (args: string[], input?: string) => {
		const ran = run(args, at, input)
		if (ran.status !== 0) throw new Error(`tinystore ${args.join(' ')}: ${ran.stderr}`)
	}
	mkdirSync(join(at, 'migrations'))
	writeFileSync(join(at, 'migrations', '0001_users.sql'), 'create table users (id integer primary key, email text);\n')
	// avatar.png is left for `files get` to write, and `files put` to read after it
	writeFileSync(join(at, 'seed.png'), Buffer.from('89504e470d0a1a0a', 'hex'))
	writeFileSync(join(at, 'backup.sh'), '#!/bin/sh\necho backed up\n')
	await stored(at)
	made(['migrate', './data', 'app', './migrations'])
	writeFileSync(
		join(at, 'migrations', '0002_orders.sql'),
		'create table orders (id integer primary key, user_id integer not null, total integer not null);\n',
	)
	made(['kv', './data', 'set', 'sessions', '42', '{"user": 42, "seen": "2026-10-18"}', '--as', 'json'])
	made(['kv', './data', 'set', 'sessions', '43', '{"user": 43, "seen": "2026-10-19"}', '--as', 'json'])
	// welcome-42 is left for `jobs add` to add, and `jobs cancel` to cancel after it
	made(['jobs', './data', 'add', 'emails', '{"to": "ada@example.com"}', '--id', 'welcome-40', '--delay', '1h'])
	made(['jobs', './data', 'add', 'emails', '{"to": "bob@example.com"}', '--id', 'welcome-41', '--delay', '2h'])
	copyFileSync(join(at, 'seed.png'), join(at, 'seed-43.png'))
	made(['files', './data', 'put', 'avatars', 'users/42/avatar.png', './seed.png'])
	made(['files', './data', 'put', 'avatars', 'users/43/avatar.png', './seed-43.png'])
	made(['events', './data', 'append', 'audit', 'role.changed', 'user:=42', 'role=admin'])
	made(['events', './data', 'append', 'audit', 'login', 'user:=43'])
	made(['sql', './data', 'app', "insert into users (email) values ('ada@example.com'), ('bob@example.com')", '--write'])
	writeFileSync(join(at, 'scrape.txt'), scrape())
	made(['metrics', './data', 'ingest', '--keep', '30d'], readFileSync(join(at, 'scrape.txt'), 'utf8'))
}

/** An hour of round trips to three targets, a sample a minute, and a counter of requests. */
function scrape(): string {
	const now = Date.now()
	const lines = ['# TYPE ping_rtt_ms gauge', '# TYPE http_requests_total counter']
	const base: Record<string, number> = { finland: 31, poland: 23, sweden: 40 }
	for (let minute = 59; minute >= 0; minute--) {
		const at = now - minute * 60_000
		for (const [target, rtt] of Object.entries(base)) {
			const value = (rtt + ((minute * 7 + target.length) % 5) * 0.4).toFixed(1)
			lines.push(`ping_rtt_ms{target="${target}"} ${value} ${at}`)
		}
		lines.push(`http_requests_total{route="/api/chat"} ${1200 + (59 - minute) * 37} ${at}`)
	}
	return `${lines.join('\n')}\n`
}

/** Makes the store: a server opens it, and stops once its SERVE is there. */
async function stored(at: string) {
	const serving = spawn(binary, ['serve', './data', '--local', '--quiet'], { cwd: at, stdio: 'ignore' })
	const deadline = Date.now() + 15_000
	while (!existsSync(join(at, 'data', 'server', 'SERVE')) && Date.now() < deadline) {
		await new Promise(resolve => setTimeout(resolve, 50))
	}
	run(['stop', './data'], at)
	await new Promise(resolve => serving.on('exit', resolve))
}
