import type { Element } from 'hast'
import { h } from 'hastscript'
import type { Code, Root } from 'mdast'
import { visit } from 'unist-util-visit'

import { type Diagram, DiagramError, parseDiagram } from '../diagram/parse'
import { renderDiagram } from '../diagram/render'

/**
 * Every ```mermaid fence of the tree drawn, by its source: ELK lays a diagram
 * out asynchronously, and the handlers that turn the tree into HTML are not,
 * so the figures are made first and the handler only takes its own. A diagram
 * that does not parse is a problem of the page, and shows its reason.
 */
export async function drawDiagrams(tree: Root, problems: string[]): Promise<Map<string, Element>> {
	const fences: Code[] = []
	visit(tree, 'code', node => {
		if (node.lang === 'mermaid') fences.push(node)
	})
	const drawn = new Map<string, Element>()
	for (const fence of fences) {
		if (drawn.has(fence.value)) continue
		drawn.set(fence.value, await figure(fence.value, problems))
	}
	return drawn
}

async function figure(source: string, problems: string[]): Promise<Element> {
	try {
		const diagram = parseDiagram(source)
		const id = idOf(source)
		const draw = (as: Diagram, suffix: string) =>
			renderDiagram(as, describe(as), `${id}${suffix}`).catch((error: unknown) => {
				// ELK's own failure names Java's classes; the page names the diagram
				throw new DiagramError(1, `the diagram could not be laid out: ${String(error)}`)
			})
		const wide = await draw(diagram, '')
		if (diagram.direction === 'TB') {
			return h('figure.diagram', { tabIndex: 0 }, [wide, hint()])
		}
		// a phone gets the same diagram top to bottom, as a scene turns on its side: both are in the page, and CSS shows one
		const narrow = await draw({ ...diagram, direction: 'TB' }, '-narrow')
		addClass(wide, 'diagram-wide')
		addClass(narrow, 'diagram-narrow')
		return h('figure.diagram', { tabIndex: 0 }, [wide, narrow, hint()])
	} catch (error) {
		if (!(error instanceof DiagramError)) throw error
		problems.push(error.message)
		return h('figure.diagram.diagram-broken', h('pre', error.message))
	}
}

/** What a reader who cannot see the picture hears: its boxes, then its arrows. */
function describe(diagram: Diagram): string {
	const label = (id: string) =>
		diagram.nodes.find(node => node.id === id)?.label.replace(/\n/g, ' ') ?? id
	const arrows = diagram.edges.map(
		edge =>
			`${label(edge.from)} to ${label(edge.to)}${edge.label ? ` (${edge.label.replace(/\n/g, ' ')})` : ''}`,
	)
	return `A diagram: ${arrows.join('; ')}.`
}

/** A short id of a diagram's source, the same in every build, for its arrowheads' ids. */
function idOf(source: string): string {
	let hash = 2166136261
	for (let index = 0; index < source.length; index++) {
		hash = Math.imul(hash ^ source.charCodeAt(index), 16777619)
	}
	return `diagram-${(hash >>> 0).toString(36)}`
}

function addClass(element: Element, name: string): void {
	const classes = element.properties.className
	element.properties.className = [...(Array.isArray(classes) ? classes : []), name]
}

// a phone shows a wide diagram small: the caption says a tap shows it whole
const hint = () => h('figcaption.diagram-hint', 'Tap to enlarge')
