import { mount, unmount } from 'svelte'
import type { Attachment } from 'svelte/attachments'

/**
 * Turns each ```explain fence of HTML that arrives through {@html} into the
 * command explain, the block the build wrote, a command highlighted, left
 * for a reader without scripts. The explain and the help it reads load only
 * on a page that has one.
 */
export function explains(html: string): Attachment<HTMLElement> {
	return node => {
		void html
		const figures = [...node.querySelectorAll<HTMLElement>('figure.explain[data-command]')]
		if (figures.length === 0) return
		let mounted: ReturnType<typeof mount>[] = []
		let gone = false
		void import('./explain/Explain.svelte').then(({ default: Explain }) => {
			if (gone) return
			mounted = figures.map(figure => {
				const command = figure.dataset.command ?? ''
				figure.replaceChildren()
				return mount(Explain, { target: figure, props: { command } })
			})
		})
		return () => {
			gone = true
			for (const explain of mounted) void unmount(explain)
		}
	}
}
