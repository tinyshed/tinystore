/**
 * A diagram: a ```mermaid fence of a flowchart, which GitHub draws as it is and
 * the site lays out and draws in its own colours when it is built.
 *
 *     flowchart LR                               the direction: LR or TB
 *       app[Your program] -->|frames| core       a box, and an arrow with a label
 *       core[(sqlite)]                           shapes: [box] (round) ([pill]) [(file)]
 *       a ==> b --- c                            an arrow that stands out, and a line without one
 *       subgraph engines [The engines]           a group of boxes, to its `end`
 *         kv & jobs                              `&` names several at once
 *       end
 *
 * Only this much of Mermaid is read, what the docs' diagrams use: a line it
 * does not know fails the page rather than drawing something other than what
 * GitHub draws.
 */

export type Direction = 'LR' | 'TB'
export type Shape = 'box' | 'round' | 'pill' | 'file'
export type Stroke = 'solid' | 'thick'

export interface Node {
	id: string
	label: string
	shape: Shape
	/** the group it is drawn in, none at the top */
	group?: string
}

export interface Edge {
	from: string
	to: string
	label?: string
	stroke: Stroke
	arrow: boolean
}

export interface Group {
	id: string
	label: string
	parent?: string
}

export interface Diagram {
	direction: Direction
	nodes: Node[]
	edges: Edge[]
	groups: Group[]
}

export class DiagramError extends Error {
	constructor(
		readonly line: number,
		message: string,
	) {
		super(`diagram line ${line}: ${message}`)
	}
}

// the brackets of each shape, the longest first so that `[(` is not read as `[`
const shapes: [string, string, Shape][] = [
	['([', '])', 'pill'],
	['[(', ')]', 'file'],
	['[', ']', 'box'],
	['(', ')', 'round'],
]

// an arrow, its label written `-->|label|`, between two ends
const arrows: [RegExp, Stroke, boolean][] = [
	[/^==>/, 'thick', true],
	[/^-->/, 'solid', true],
	[/^---/, 'solid', false],
]

export function parseDiagram(source: string): Diagram {
	const lines = source.split('\n')
	const header = lines.findIndex(line => line.trim() !== '' && !line.trim().startsWith('%%'))
	const start = /^flowchart\s+(LR|TB)\s*$/.exec(lines[header]?.trim() ?? '')
	if (start === null) {
		throw new DiagramError(header + 1, 'a diagram starts with `flowchart LR` or `flowchart TB`')
	}
	const diagram: Diagram = {
		direction: start[1] as Direction,
		nodes: [],
		edges: [],
		groups: [],
	}
	const open: string[] = []

	for (let index = header + 1; index < lines.length; index++) {
		const line = (lines[index] ?? '').trim()
		const at = index + 1
		if (line === '' || line.startsWith('%%')) continue
		if (line === 'end') {
			if (open.pop() === undefined) throw new DiagramError(at, '`end` closes no `subgraph`')
			continue
		}
		const sub = /^subgraph\s+([\w-]+)(?:\s*\[(.+)\])?\s*$/.exec(line)
		if (sub) {
			const id = sub[1] as string
			diagram.groups.push({ id, label: unquote(sub[2] ?? id), parent: open.at(-1) })
			open.push(id)
			continue
		}
		statement(diagram, line, at, open.at(-1))
	}
	if (open.length > 0) {
		throw new DiagramError(lines.length, `subgraph ${open.at(-1)} has no \`end\``)
	}
	for (const edge of diagram.edges) {
		for (const end of [edge.from, edge.to]) {
			if (!diagram.nodes.some(node => node.id === end)) {
				throw new DiagramError(0, `${end} is in an arrow and is no box`)
			}
		}
	}
	return diagram
}

/** One line of boxes and arrows: `a[A] -->|x| b & c --> d`. */
function statement(diagram: Diagram, line: string, at: number, group: string | undefined): void {
	let rest = line
	let previous: string[] | undefined
	let pending: Omit<Edge, 'from' | 'to'> | undefined
	while (rest !== '') {
		const ends: string[] = []
		for (;;) {
			const [id, after] = node(diagram, rest, at, group)
			ends.push(id)
			rest = after.trimStart()
			if (!rest.startsWith('&')) break
			rest = rest.slice(1).trimStart()
		}
		if (previous !== undefined && pending !== undefined) {
			for (const from of previous) {
				for (const to of ends) diagram.edges.push({ from, to, ...pending })
			}
		}
		previous = ends
		pending = undefined
		if (rest === '') break
		const [arrow, after] = edge(rest, at)
		pending = arrow
		rest = after.trimStart()
		if (rest === '') throw new DiagramError(at, 'an arrow leads to no box')
	}
}

/** A box's id and, the first time, its label and shape; and what follows it. */
function node(
	diagram: Diagram,
	text: string,
	at: number,
	group: string | undefined,
): [string, string] {
	const id = /^[\w-]+/.exec(text)?.[0]
	if (id === undefined)
		throw new DiagramError(at, `a box's id is a word, not ${JSON.stringify(text.slice(0, 20))}`)
	let rest = text.slice(id.length)
	let shaped: { label: string; shape: Shape } | undefined
	for (const [open, close, shape] of shapes) {
		if (!rest.startsWith(open)) continue
		const end = closing(rest, open.length, close)
		if (end < 0) throw new DiagramError(at, `${id}${open}… has no ${close}`)
		shaped = { label: unquote(rest.slice(open.length, end).trim()), shape }
		rest = rest.slice(end + close.length)
		break
	}
	const known = diagram.nodes.find(node => node.id === id)
	if (known === undefined) {
		diagram.nodes.push({ id, label: shaped?.label ?? id, shape: shaped?.shape ?? 'box', group })
	} else if (shaped !== undefined) {
		known.label = shaped.label
		known.shape = shaped.shape
	}
	return [id, rest]
}

// where a shape's closing bracket is, past a quoted label that may hold one
function closing(text: string, from: number, close: string): number {
	const body = text.slice(from)
	if (body.trimStart().startsWith('"')) {
		const quote = text.indexOf('"', text.indexOf('"', from) + 1)
		return quote < 0 ? -1 : text.indexOf(close, quote)
	}
	return text.indexOf(close, from)
}

function edge(text: string, at: number): [Omit<Edge, 'from' | 'to'>, string] {
	for (const [pattern, stroke, arrow] of arrows) {
		const match = pattern.exec(text)
		if (!match) continue
		let rest = text.slice(match[0].length)
		let label: string | undefined
		const piped = /^\|([^|]*)\|/.exec(rest)
		if (piped) {
			label = unquote((piped[1] as string).trim())
			rest = rest.slice(piped[0].length)
		}
		return [{ stroke, arrow, ...(label ? { label } : {}) }, rest]
	}
	throw new DiagramError(at, `${JSON.stringify(text.slice(0, 12))} is no arrow: -->, ==> or ---`)
}

// a label's quotes off, and Mermaid's <br> as a line break
function unquote(label: string): string {
	const bare = label.startsWith('"') && label.endsWith('"') ? label.slice(1, -1) : label
	return bare.replace(/<br\s*\/?>/g, '\n')
}
