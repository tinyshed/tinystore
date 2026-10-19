import type { Attachment } from 'svelte/attachments'

/**
 * Enlarges the diagrams of HTML that arrives through {@html}: a click, or Enter
 * on a focused `figure.diagram`, opens the picture the reader sees at its own
 * size in a dialog that fills the screen, where it scrolls and a phone pinches
 * it. The build drew the SVG; this only shows it bigger.
 */
export function diagrams(html: string): Attachment<HTMLElement> {
	return node => {
		void html
		const open = (event: Event) => {
			const figure = (event.target as Element | null)?.closest<HTMLElement>('figure.diagram')
			if (figure === null || figure === undefined || figure.classList.contains('diagram-broken'))
				return
			if (event instanceof KeyboardEvent && event.key !== 'Enter') return
			enlarge(figure)
		}
		node.addEventListener('click', open)
		node.addEventListener('keydown', open)
		return () => {
			node.removeEventListener('click', open)
			node.removeEventListener('keydown', open)
		}
	}
}

function enlarge(figure: HTMLElement): void {
	// the twin the reader sees: the wide one on a desktop, the tall one on a phone
	const shown = [...figure.querySelectorAll<SVGSVGElement>('svg.diagram')].find(
		svg => getComputedStyle(svg).display !== 'none',
	)
	if (shown === undefined) return
	const picture = shown.cloneNode(true) as SVGSVGElement
	picture.removeAttribute('style')
	const dialog = document.createElement('dialog')
	dialog.className = 'diagram-zoom'
	dialog.setAttribute('aria-label', shown.getAttribute('aria-label') ?? 'A diagram')
	dialog.append(picture)
	dialog.addEventListener('click', () => dialog.close())
	dialog.addEventListener('close', () => {
		dialog.remove()
		figure.focus()
	})
	document.body.append(dialog)
	dialog.showModal()
}
