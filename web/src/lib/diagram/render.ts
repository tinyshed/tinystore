import { createRequire } from 'node:module'
import ELK, { type ElkExtendedEdge, type ElkNode } from 'elkjs/lib/elk-api.js'
import type { Element, ElementContent } from 'hast'
import { s } from 'hastscript'

import type { Diagram, Edge, Node, Shape } from './parse'

/**
 * A diagram as an SVG, laid out by ELK's layered algorithm when the site is
 * built: boxes in ranks along the diagram's direction, arrows routed at right
 * angles around them, groups drawn around their boxes. The SVG names no
 * colour: its classes take the site's tokens, so both themes draw it.
 */

// sizes in the SVG's own units, which the page shows one to one up to its width
const lineHeight = 20
const padX = 14
const padY = 12
/**
 * What each character of a box's words takes: Geist at 14px and weight 550, as
 * the stylesheet sets `.diagram-text`, measured in a browser with a canvas's
 * `measureText`. A character not here, a Cyrillic letter, takes the widest.
 * Labels are Geist Mono at 11.5px, every character as wide.
 */
const glyphs =
	' !"#$%&\'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~·−…—→'
const advances = [
	3.35, 3.29, 5.15, 7.48, 9.09, 11.4, 9.28, 2.67, 4.17, 4.17, 5.96, 7.9, 3.07, 5.85, 3.07, 7.01,
	9.49, 5.82, 8.9, 8.83, 8.9, 9.09, 8.54, 7.48, 8.88, 8.57, 4.26, 4.26, 7.66, 7.64, 7.66, 8.05,
	13.08, 9.79, 9.67, 10.07, 9.87, 8.57, 8.36, 10.07, 10.04, 3.99, 8.57, 9.3, 8.19, 12.54, 10.46,
	10.6, 9.25, 10.51, 9.59, 9.25, 8.06, 9.74, 9.79, 13.72, 9.06, 8.46, 7.97, 5.15, 6.69, 5.15, 6.22,
	7.83, 3.68, 8.02, 8.61, 8.01, 8.61, 8.16, 5.89, 8.6, 8.34, 3.68, 4.14, 8.67, 4.06, 12.43, 8.34,
	8.34, 8.61, 8.61, 5.63, 7.63, 5.85, 8.27, 8.01, 11.68, 8.64, 7.87, 7.84, 5.57, 3.91, 5.57, 7.32,
	3.07, 7.45, 8.86, 12.73, 12.08,
]
const widest = Math.max(...advances)
const monoWidth = 6.9

function wordsWidth(line: string): number {
	let width = 0
	for (const char of line) {
		const at = glyphs.indexOf(char)
		width += at < 0 ? widest : (advances[at] as number)
	}
	return width
}
const radius = 8

const elk = new ELK({ workerFactory: () => new (inProcessWorker())() })

/**
 * ELK's layout without a thread of its own. Its worker's module exports the
 * worker only where no `self` is defined, and takes `self` for a Web Worker's
 * global otherwise; Bun defines one, Node does not. It is hidden while the
 * module loads, so that the tests under Bun and the build under Node both get it.
 */
function inProcessWorker(): new () => Worker {
	const global = globalThis as { self?: unknown }
	const self = Object.getOwnPropertyDescriptor(global, 'self')
	delete global.self
	try {
		return createRequire(import.meta.url)('elkjs/lib/elk-worker.min.js').Worker
	} finally {
		if (self) Object.defineProperty(global, 'self', self)
	}
}

/** A box's width and height, its label's lines inside its padding. */
function measure(label: string, shape: Shape): { width: number; height: number } {
	const lines = label.split('\n')
	const width = Math.max(...lines.map(wordsWidth)) + 2 * padX
	const height = lines.length * lineHeight + 2 * padY
	switch (shape) {
		case 'file':
			return { width, height: height + 12 }
		default:
			return { width: Math.max(width, 80), height }
	}
}

function labelSize(text: string): { width: number; height: number } {
	const lines = text.split('\n')
	return {
		width: Math.max(...lines.map(line => [...line].length)) * monoWidth + 8,
		height: lines.length * 16 + 4,
	}
}

const directions = { LR: 'RIGHT', TB: 'DOWN' } as const

/** Where ELK puts every box, group, arrow and label, in the SVG's units from its top left. */
export function layOut(diagram: Diagram): Promise<ElkNode> {
	return elk.layout(graphOf(diagram))
}

/** The diagram as ELK's graph: groups as nodes holding their boxes, every edge at the top. */
function graphOf(diagram: Diagram): ElkNode {
	const nodeOf = (node: Node): ElkNode => ({ id: node.id, ...measure(node.label, node.shape) })
	// where a box or a group first appears in the source: ELK reads its children's order as the source's
	const firstOf = (group: string): number =>
		Math.min(
			...diagram.nodes.flatMap((node, index) => (node.group === group ? [index] : [])),
			...diagram.groups.filter(inner => inner.parent === group).map(inner => firstOf(inner.id)),
		)
	const childrenOf = (parent: string | undefined): ElkNode[] =>
		[
			...diagram.groups
				.filter(group => group.parent === parent)
				.map(group => ({ at: firstOf(group.id), node: groupOf(group.id, group.label) })),
			...diagram.nodes.flatMap((node, at) =>
				node.group === parent ? [{ at, node: nodeOf(node) }] : [],
			),
		]
			.sort((a, b) => a.at - b.at)
			.map(child => child.node)
	const groupOf = (id: string, label: string): ElkNode => ({
		id,
		labels: [{ text: label, ...labelSize(label) }],
		layoutOptions: { 'elk.padding': '[top=34,left=16,bottom=16,right=16]' },
		children: childrenOf(id),
	})
	return {
		id: 'root',
		layoutOptions: {
			'elk.algorithm': 'layered',
			'elk.direction': directions[diagram.direction],
			'elk.hierarchyHandling': 'INCLUDE_CHILDREN',
			'elk.edgeRouting': 'ORTHOGONAL',
			'elk.layered.spacing.nodeNodeBetweenLayers': '36',
			'elk.spacing.nodeNode': '20',
			'elk.spacing.edgeLabel': '6',
			'elk.layered.spacing.edgeNodeBetweenLayers': '24',
			// a cycle is broken at the arrow that goes back against the source's order, so a flow
			// starts where its author began it, and boxes keep that order where the ranks allow
			'elk.layered.cycleBreaking.strategy': 'MODEL_ORDER',
			'elk.layered.considerModelOrder.strategy': 'NODES_AND_EDGES',
			'elk.edgeLabels.placement': 'CENTER',
			// a box over the boxes it leads to stands in their middle, not over the first
			'elk.layered.nodePlacement.bk.fixedAlignment': 'BALANCED',
			'elk.json.shapeCoords': 'ROOT',
			'elk.json.edgeCoords': 'ROOT',
		},
		children: childrenOf(undefined),
		edges: diagram.edges.map(
			(edge, index): ElkExtendedEdge => ({
				id: `e${index}`,
				sources: [edge.from],
				targets: [edge.to],
				labels: edge.label ? [{ text: edge.label, ...labelSize(edge.label) }] : [],
			}),
		),
	}
}

/**
 * The diagram as an SVG element, its description for those who cannot see it.
 * `id` names its arrowheads, so that two diagrams of a page keep their own.
 */
export async function renderDiagram(
	diagram: Diagram,
	description: string,
	id: string,
): Promise<Element> {
	const laid = await layOut(diagram)
	const width = Math.ceil((laid.width ?? 0) + 4)
	const height = Math.ceil((laid.height ?? 0) + 4)

	const groups: ElementContent[] = []
	const boxes: ElementContent[] = []
	const walk = (node: ElkNode) => {
		for (const child of node.children ?? []) {
			const group = diagram.groups.find(group => group.id === child.id)
			if (group) groups.push(drawGroup(child, group.label))
			const box = diagram.nodes.find(box => box.id === child.id)
			if (box) boxes.push(drawNode(child, box))
			walk(child)
		}
	}
	walk(laid)
	const edges = (laid.edges ?? []).map((edge, index) =>
		drawEdge(edge, diagram.edges[index] as Edge, id),
	)

	return s(
		'svg.diagram',
		{
			viewBox: `-2 -2 ${width} ${height}`,
			width,
			height,
			// the whole picture in the column, never wider than itself; a tap enlarges one that shrank
			style: `width: min(100%, ${width}px)`,
			role: 'img',
			ariaLabel: description,
			xmlns: 'http://www.w3.org/2000/svg',
		},
		[
			s('defs', [head(`${id}-arrow`, 'diagram-head'), head(`${id}-accent`, 'diagram-head-accent')]),
			s('g', groups),
			s(
				'g',
				edges.map(edge => edge.line),
			),
			s('g', boxes),
			s(
				'g',
				edges.flatMap(edge => edge.label),
			),
		],
	)
}

function head(id: string, className: string): Element {
	return s(
		'marker',
		{
			id,
			viewBox: '0 0 10 10',
			refX: 9,
			refY: 5,
			// the same head on a thick arrow as on a thin one
			markerUnits: 'userSpaceOnUse',
			markerWidth: 9,
			markerHeight: 9,
			orient: 'auto-start-reverse',
		},
		[s(`path.${className}`, { d: 'M 0 0 L 10 5 L 0 10 z' })],
	)
}

function drawGroup(group: ElkNode, label: string): Element {
	const { x = 0, y = 0, width = 0, height = 0 } = group
	return s('g.diagram-group', [
		s('rect', { x, y, width, height, rx: 14 }),
		s('text.diagram-group-label', { x: x + 16, y: y + 22 }, label),
	])
}

function drawNode(box: ElkNode, node: Node): Element {
	const { x = 0, y = 0, width = 0, height = 0 } = box
	const cx = x + width / 2
	const cy = y + height / 2
	return s(`g.diagram-node.diagram-${node.shape}`, [
		shapeOf(node.shape, x, y, width, height),
		text(node.label, cx, cy),
	])
}

function shapeOf(shape: Shape, x: number, y: number, width: number, height: number): Element {
	switch (shape) {
		case 'round':
			return s('rect', { x, y, width, height, rx: 16 })
		case 'pill':
			return s('rect', { x, y, width, height, rx: height / 2 })
		case 'file': {
			// a cylinder: SQLite's file, the shape every diagram gives a database
			const ry = 6
			return s('path', {
				d:
					`M ${x} ${y + ry} a ${width / 2} ${ry} 0 0 1 ${width} 0 v ${height - 2 * ry} ` +
					`a ${width / 2} ${ry} 0 0 1 ${-width} 0 z M ${x} ${y + ry} a ${width / 2} ${ry} 0 0 0 ${width} 0`,
			})
		}
		default:
			return s('rect', { x, y, width, height, rx: 10 })
	}
}

function text(
	label: string,
	cx: number,
	cy: number,
	className = 'diagram-text',
	size = lineHeight,
): Element {
	const lines = label.split('\n')
	const top = cy - ((lines.length - 1) * size) / 2
	return s(
		`text.${className}`,
		{ x: cx, y: top, textAnchor: 'middle', dominantBaseline: 'central' },
		lines.map((line, index) => s('tspan', { x: cx, y: top + index * size }, line)),
	)
}

/** An edge's path through its bends, each corner rounded, and its label, which a halo of the page's colour sets off the lines. */
function drawEdge(
	edge: ElkExtendedEdge,
	given: Edge,
	id: string,
): { line: Element; label: ElementContent[] } {
	const section = edge.sections?.[0]
	const points = section
		? [section.startPoint, ...(section.bendPoints ?? []), section.endPoint]
		: []
	const marker = given.stroke === 'thick' ? `${id}-accent` : `${id}-arrow`
	const line = s(`path.diagram-edge.diagram-${given.stroke}`, {
		d: rounded(points),
		markerEnd: given.arrow ? `url(#${marker})` : undefined,
	})
	const label = (edge.labels ?? []).map(
		({ x = 0, y = 0, width = 0, height = 0, text: words = '' }) =>
			text(words, x + width / 2, y + height / 2, 'diagram-label', 16),
	)
	return { line, label }
}

function rounded(points: { x: number; y: number }[]): string {
	if (points.length === 0) return ''
	const [first, ...rest] = points as [{ x: number; y: number }, ...{ x: number; y: number }[]]
	let d = `M ${first.x} ${first.y}`
	for (let index = 0; index < rest.length; index++) {
		const point = rest[index] as { x: number; y: number }
		const before = (index === 0 ? first : rest[index - 1]) as { x: number; y: number }
		const after = rest[index + 1]
		if (after === undefined) {
			d += ` L ${point.x} ${point.y}`
			continue
		}
		const r = Math.min(radius, distance(before, point) / 2, distance(point, after) / 2)
		const into = toward(point, before, r)
		const out = toward(point, after, r)
		d += ` L ${into.x} ${into.y} Q ${point.x} ${point.y} ${out.x} ${out.y}`
	}
	return d
}

const distance = (a: { x: number; y: number }, b: { x: number; y: number }) =>
	Math.hypot(a.x - b.x, a.y - b.y)

function toward(from: { x: number; y: number }, to: { x: number; y: number }, by: number) {
	const length = distance(from, to) || 1
	return {
		x: from.x + ((to.x - from.x) / length) * by,
		y: from.y + ((to.y - from.y) / length) * by,
	}
}
