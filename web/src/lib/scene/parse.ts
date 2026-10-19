import { icons } from './icons'

/**
 * A scene: a short animation written as text in a ```scene fence, which GitHub
 * and an agent read as text and the site plays beside the code it runs.
 *
 *     code ts                                        the program, its lines highlighted as steps run
 *       const charges = store.once('charges')
 *     end
 *     actor buyer "Buyer" at 8,50 icon=user          who is on stage, where in percent, how it looks
 *     link buyer api                                 a cable; `cut`, and `unlink` to take it away
 *     set kv "charges: —"                            an actor's line of state; `add` appends, `strike` crosses out
 *     mark us home                                   home, copy or down
 *
 *     step "The buyer clicks Pay twice." line=3-4    a caption, and the code's lines it runs
 *       send buyer -> api "POST /pay"                a packet along an arc; `lost`, `ghost`, `color=ok`
 *       & send buyer -> api2 "POST /pay"             `&` runs with the action before it
 *       pulse kv | say api "waiting…" | wait 400
 *
 * Everything before the first step is how the scene starts.
 */

export interface Actor {
	id: string
	label: string
	x: number
	y: number
	icon: string
}

export type Verb =
	| 'send'
	| 'pulse'
	| 'say'
	| 'wait'
	| 'set'
	| 'add'
	| 'strike'
	| 'mark'
	| 'link'
	| 'unlink'
	| 'cut'

export interface Action {
	verb: Verb
	/** runs with the action before it */
	together: boolean
	/** the actor it is about, or a packet's sender */
	a: string
	/** the second actor, a text or a duration in milliseconds; a packet's receiver */
	b: string
	/** a packet's label */
	label: string
	color?: 'ok' | 'bad'
	lost?: boolean
	ghost?: boolean
}

export interface Step {
	caption: string
	/** the code's lines this step runs, first and last, counted from 1 */
	lines?: [number, number]
	actions: Action[]
}

export interface Scene {
	code?: { lang: string; text: string }
	actors: Actor[]
	setup: Action[]
	steps: Step[]
}

export class SceneError extends Error {
	constructor(
		readonly line: number,
		message: string,
	) {
		// a check of the whole scene has no one line to name
		super(line > 0 ? `scene line ${line}: ${message}` : `scene: ${message}`)
	}
}

const marks = new Set(['home', 'copy', 'down'])

export function parseScene(text: string): Scene {
	const scene: Scene = { actors: [], setup: [], steps: [] }
	const lines = text.split(/\r?\n/)
	let code: string[] | undefined
	let codeLang = ''

	for (const [index, raw] of lines.entries()) {
		const at = index + 1
		if (code !== undefined) {
			if (raw.trim() === 'end') {
				scene.code = { lang: codeLang, text: dedent(code) }
				code = undefined
			} else {
				code.push(raw)
			}
			continue
		}
		const line = raw.trim()
		if (line === '' || line.startsWith('#')) {
			continue
		}
		const together = line.startsWith('&')
		const words = tokens(together ? line.slice(1) : line)
		const verb = words[0] ?? ''

		if (verb === 'code') {
			codeLang = words[1] ?? 'text'
			code = []
		} else if (verb === 'actor') {
			scene.actors.push(actor(words, at))
		} else if (verb === 'step') {
			scene.steps.push(step(words, at))
		} else {
			const action = parseAction(verb, words, together, at)
			;(scene.steps.at(-1)?.actions ?? scene.setup).push(action)
		}
	}

	if (code !== undefined) {
		throw new SceneError(lines.length, 'a `code` block has no `end`')
	}
	check(scene)
	return scene
}

function actor(words: string[], at: number): Actor {
	const [, id, label] = words
	const where = words[words.indexOf('at') + 1]
	const position = where?.split(',').map(Number) ?? []
	const [x, y] = position
	if (id === undefined || label === undefined || x === undefined || y === undefined) {
		throw new SceneError(at, 'an actor is `actor <id> "<label>" at <x>,<y> icon=<name>`')
	}
	if (![x, y].every(n => Number.isFinite(n) && n >= 0 && n <= 100)) {
		throw new SceneError(at, `${id} stands at ${where}: x and y are percents, 0 to 100`)
	}
	const icon = option(words, 'icon') ?? 'server'
	if (icons[icon] === undefined) {
		throw new SceneError(
			at,
			`${id} wears icon ${icon}, which is not one of ${Object.keys(icons).join(', ')}`,
		)
	}
	return { id, label, x, y, icon }
}

function step(words: string[], at: number): Step {
	const caption = words[1]
	if (caption === undefined) {
		throw new SceneError(at, 'a step is `step "<caption>"`, optionally with `line=3` or `line=3-5`')
	}
	const lines = option(words, 'line')
	if (lines === undefined) {
		return { caption, actions: [] }
	}
	const [from = Number.NaN, to = from] = lines.split('-').map(Number)
	if (!Number.isInteger(from) || !Number.isInteger(to) || from < 1 || to < from) {
		throw new SceneError(at, `line=${lines} is not a line or a range of lines`)
	}
	return { caption, lines: [from, to], actions: [] }
}

const verbs = new Set<Verb>([
	'send',
	'pulse',
	'say',
	'wait',
	'set',
	'add',
	'strike',
	'mark',
	'link',
	'unlink',
	'cut',
])

function parseAction(verb: string, words: string[], together: boolean, at: number): Action {
	if (!verbs.has(verb as Verb)) {
		throw new SceneError(at, `${verb} is not a verb a scene knows: ${[...verbs].join(', ')}`)
	}
	const color = option(words, 'color')
	if (color !== undefined && color !== 'ok' && color !== 'bad') {
		throw new SceneError(at, `color=${color}: a colour is ok or bad`)
	}
	const flags: Pick<Action, 'color' | 'lost' | 'ghost'> = {
		...(color === undefined ? {} : { color }),
		...(words.includes('lost') ? { lost: true } : {}),
		...(words.includes('ghost') ? { ghost: true } : {}),
	}
	if (verb === 'send') {
		if (words[2] !== '->') {
			throw new SceneError(at, 'a packet is `send <from> -> <to> "<label>"`')
		}
		return { verb, together, a: words[1] ?? '', b: words[3] ?? '', label: words[4] ?? '', ...flags }
	}
	return { verb: verb as Verb, together, a: words[1] ?? '', b: words[2] ?? '', label: '', ...flags }
}

// every actor named exists, every line run is in the code, and something happens
function check(scene: Scene): void {
	const ids = new Set(scene.actors.map(actor => actor.id))
	if (scene.steps.length === 0) {
		throw new SceneError(1, 'a scene has at least one `step`')
	}
	const named = (id: string, what: string) => {
		if (!ids.has(id)) {
			throw new SceneError(
				0,
				`${what} names ${id || 'no actor'}, and the scene has ${[...ids].join(', ')}`,
			)
		}
	}
	const actions = [...scene.setup, ...scene.steps.flatMap(step => step.actions)]
	for (const action of actions) {
		if (action.verb === 'wait') {
			if (!Number.isFinite(Number(action.a)))
				throw new SceneError(0, `wait ${action.a}: a wait is milliseconds`)
			continue
		}
		named(action.a, action.verb)
		if (['send', 'link', 'unlink', 'cut'].includes(action.verb)) {
			named(action.b, action.verb)
		}
		if (action.verb === 'mark' && !marks.has(action.b)) {
			throw new SceneError(0, `mark ${action.a} ${action.b}: a mark is home, copy or down`)
		}
	}
	const codeLines = scene.code?.text.split('\n').length ?? 0
	for (const step of scene.steps) {
		if (step.lines !== undefined && (scene.code === undefined || step.lines[1] > codeLines)) {
			throw new SceneError(
				0,
				`step “${step.caption}” runs line ${step.lines[1]}, and the code has ${codeLines}`,
			)
		}
	}
}

function tokens(line: string): string[] {
	return [...line.matchAll(/"([^"]*)"|(\S+)/g)].map(match => match[1] ?? match[2] ?? '')
}

function option(words: string[], name: string): string | undefined {
	const found = words.find(word => word.startsWith(`${name}=`))
	return found?.slice(name.length + 1)
}

function dedent(lines: string[]): string {
	const indents = lines
		.filter(line => line.trim() !== '')
		.map(line => /^\s*/.exec(line)?.[0].length ?? 0)
	const cut = Math.min(...indents)
	return lines.map(line => line.slice(cut)).join('\n')
}
