import { describe, expect, test } from 'bun:test'
import { readdirSync, readFileSync } from 'node:fs'
import { join, relative } from 'node:path'
import type { ElkNode } from 'elkjs/lib/elk.bundled.js'

import { drawDiagrams } from '../content/diagram'
import { type Diagram, DiagramError, parseDiagram } from './parse'
import { layOut } from './render'

const docs = join(import.meta.dir, '../../../../docs')

// the widest a diagram may be drawn: past it, the column shrinks its words below reading
const widest = 800

describe('a diagram', () => {
	test('reads boxes, shapes, labelled arrows and groups as GitHub draws them', () => {
		const diagram = parseDiagram(
			[
				'flowchart LR',
				'  subgraph ways [Frames]',
				'    pipe[the pipe<br>in process]',
				'  end',
				'  sdk([SDK]) -->|frames| pipe & core',
				'  core[(SQLite)]',
				'  rust ==>|"direct, calls"| core',
				'  a(too old) --- b',
			].join('\n'),
		)
		expect(diagram.direction).toBe('LR')
		expect(diagram.groups).toEqual([{ id: 'ways', label: 'Frames', parent: undefined }])
		expect(diagram.nodes.find(node => node.id === 'pipe')).toEqual({
			id: 'pipe',
			label: 'the pipe\nin process',
			shape: 'box',
			group: 'ways',
		})
		expect(diagram.nodes.find(node => node.id === 'core')?.shape).toBe('file')
		expect(diagram.edges).toContainEqual({
			from: 'sdk',
			to: 'core',
			label: 'frames',
			stroke: 'solid',
			arrow: true,
		})
		expect(diagram.edges).toContainEqual({
			from: 'rust',
			to: 'core',
			label: 'direct, calls',
			stroke: 'thick',
			arrow: true,
		})
		expect(diagram.edges).toContainEqual({ from: 'a', to: 'b', stroke: 'solid', arrow: false })
	})

	test('refuses what GitHub would draw otherwise, naming the line', () => {
		const refused = (source: string) => {
			try {
				parseDiagram(source)
			} catch (error) {
				if (error instanceof DiagramError) return error.message
				throw error
			}
			throw new Error(`${source} was read`)
		}
		expect(refused('sequenceDiagram\n  a->>b: hi')).toContain('starts with `flowchart')
		expect(refused('flowchart LR\n  a ~~> b')).toBe(
			'diagram line 2: "~~> b" is no arrow: -->, ==> or ---',
		)
		expect(refused('flowchart LR\n  subgraph x\n  a --> b')).toContain('subgraph x has no `end`')
		expect(refused('flowchart LR\n  a[open --> b')).toContain('a[… has no ]')
	})
})

/** Every ```mermaid fence of the docs, with the file and line it is on. */
function fences(): { where: string; source: string }[] {
	const found: { where: string; source: string }[] = []
	const walk = (dir: string) => {
		for (const entry of readdirSync(dir, { withFileTypes: true })) {
			const path = join(dir, entry.name)
			if (entry.isDirectory()) {
				if (entry.name !== 'reference') walk(path)
				continue
			}
			if (!entry.name.endsWith('.md')) continue
			const lines = readFileSync(path, 'utf8').split('\n')
			lines.forEach((line, index) => {
				if (line !== '```mermaid') return
				const end = lines.indexOf('```', index + 1)
				found.push({
					where: `${relative(docs, path)}:${index + 1}`,
					source: lines.slice(index + 1, end).join('\n'),
				})
			})
		}
	}
	walk(docs)
	return found
}

interface Box {
	id: string
	x: number
	y: number
	width: number
	height: number
}

function boxes(laid: ElkNode, diagram: Diagram): Box[] {
	const found: Box[] = []
	const walk = (node: ElkNode) => {
		for (const child of node.children ?? []) {
			if (diagram.nodes.some(box => box.id === child.id)) {
				found.push({
					id: child.id,
					x: child.x ?? 0,
					y: child.y ?? 0,
					width: child.width ?? 0,
					height: child.height ?? 0,
				})
			}
			walk(child)
		}
	}
	walk(laid)
	return found
}

type Point = { x: number; y: number }
type Segment = [Point, Point]

function segments(laid: ElkNode): { edge: number; segment: Segment }[] {
	return (laid.edges ?? []).flatMap((edge, index) => {
		const section = edge.sections?.[0]
		if (!section) return []
		const points = [section.startPoint, ...(section.bendPoints ?? []), section.endPoint]
		return points
			.slice(1)
			.map((point, at) => ({ edge: index, segment: [points[at] as Point, point] as Segment }))
	})
}

const inside = (box: Box, point: Point, margin = 1) =>
	point.x > box.x + margin &&
	point.x < box.x + box.width - margin &&
	point.y > box.y + margin &&
	point.y < box.y + box.height - margin

// an axis-aligned segment through a box's inside, its ends aside
function crosses(box: Box, [a, b]: Segment): boolean {
	const steps = 20
	for (let step = 1; step < steps; step++) {
		const t = step / steps
		if (inside(box, { x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t })) return true
	}
	return false
}

// two right-angled segments that cross: one horizontal, one vertical, each through the other's span
function intersect([a, b]: Segment, [c, d]: Segment): boolean {
	const horizontal = (p: Point, q: Point) => Math.abs(p.y - q.y) < 0.5
	const [h, v] = horizontal(a, b)
		? [
				[a, b],
				[c, d],
			]
		: [
				[c, d],
				[a, b],
			]
	const [h1, h2] = h as [Point, Point]
	const [v1, v2] = v as [Point, Point]
	if (!horizontal(h1, h2) || horizontal(v1, v2)) return false
	const within = (n: number, p: number, q: number) =>
		n > Math.min(p, q) + 0.5 && n < Math.max(p, q) - 0.5
	return within(v1.x, h1.x, h2.x) && within(h1.y, v1.y, v2.y)
}

describe('every diagram of the docs', () => {
	const all = fences()

	test('there are diagrams to check', () => {
		expect(all.length).toBeGreaterThanOrEqual(8)
	})

	for (const { where, source } of all) {
		describe(where, () => {
			const diagram = parseDiagram(source)
			const shapes = [
				diagram,
				...(diagram.direction === 'LR' ? [{ ...diagram, direction: 'TB' as const }] : []),
			]

			for (const shape of shapes) {
				const laid = layOut(shape)

				test(`${shape.direction}: fits the column`, async () => {
					expect((await laid).width ?? 0).toBeLessThanOrEqual(widest)
				})

				test(`${shape.direction}: no box over another, no label over a box`, async () => {
					const placed = boxes(await laid, shape)
					const labels = ((await laid).edges ?? []).flatMap(edge => edge.labels ?? [])
					for (const box of placed) {
						for (const other of placed) {
							if (other === box) continue
							const apart =
								box.x + box.width <= other.x ||
								other.x + other.width <= box.x ||
								box.y + box.height <= other.y ||
								other.y + other.height <= box.y
							expect(apart, `${box.id} and ${other.id} overlap`).toBe(true)
						}
						for (const label of labels) {
							const corners = [
								{ x: label.x ?? 0, y: label.y ?? 0 },
								{ x: (label.x ?? 0) + (label.width ?? 0), y: (label.y ?? 0) + (label.height ?? 0) },
							]
							expect(
								corners.some(corner => inside(box, corner, 2)),
								`"${label.text}" is over ${box.id}`,
							).toBe(false)
						}
					}
				})

				test(`${shape.direction}: no arrow through a box it does not join`, async () => {
					const placed = boxes(await laid, shape)
					for (const { edge, segment } of segments(await laid)) {
						const { from, to } = shape.edges[edge] as { from: string; to: string }
						for (const box of placed) {
							if (box.id === from || box.id === to) continue
							expect(crosses(box, segment), `${from} → ${to} runs through ${box.id}`).toBe(false)
						}
					}
				})

				test(`${shape.direction}: no two arrows cross`, async () => {
					const all = segments(await laid)
					const crossings = all.flatMap((one, index) =>
						all
							.slice(index + 1)
							.filter(other => other.edge !== one.edge && intersect(one.segment, other.segment)),
					)
					expect(crossings.length).toBe(0)
				})

				test(`${shape.direction}: boxes of a rank keep the source's order`, async () => {
					const placed = boxes(await laid, shape)
					const across = shape.direction === 'TB' ? 'x' : 'y'
					const along = across === 'x' ? 'y' : 'x'
					const ranks = Map.groupBy(placed, box =>
						Math.round(box[along] + box[across === 'x' ? 'height' : 'width'] / 2),
					)
					for (const rank of ranks.values()) {
						const order = shape.nodes
							.map(node => node.id)
							.filter(id => rank.some(box => box.id === id))
						const drawn = [...rank].sort((a, b) => a[across] - b[across]).map(box => box.id)
						expect(drawn).toEqual(order)
					}
				})
			}
		})
	}
})

describe('a page with a diagram', () => {
	test('names a diagram that does not parse as a problem of the page, which fails the build', async () => {
		const problems: string[] = []
		const tree = {
			type: 'root' as const,
			children: [{ type: 'code' as const, lang: 'mermaid', value: 'flowchart LR\n  a -.-> b' }],
		}
		const drawn = await drawDiagrams(tree, problems)
		expect(problems).toEqual(['diagram line 2: "-.-> b" is no arrow: -->, ==> or ---'])
		expect(drawn.get('flowchart LR\n  a -.-> b')?.properties.className).toContain('diagram-broken')
	})

	test('draws an LR diagram twice, its TB twin for a phone', async () => {
		const drawn = await drawDiagrams(
			{
				type: 'root',
				children: [{ type: 'code', lang: 'mermaid', value: 'flowchart LR\n  a --> b' }],
			},
			[],
		)
		const figure = drawn.get('flowchart LR\n  a --> b')
		const svgs = figure?.children.filter(
			child => child.type === 'element' && child.tagName === 'svg',
		)
		expect(svgs).toHaveLength(2)
	})
})
