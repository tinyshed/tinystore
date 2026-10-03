import type { Attachment } from 'svelte/attachments'

import { setLanguage } from './prefs'
import { track } from './track'

/**
 * The code blocks' buttons, for HTML that arrives through {@html}: one
 * listener on the container answers every block in it, the language for the
 * whole site, a file within its block, a copy of what is shown.
 */
export const codeBlocks: Attachment<HTMLElement> = node => {
	const onClick = (event: MouseEvent) => {
		const target = event.target instanceof Element ? event.target : null

		const language = target?.closest<HTMLElement>('.code-lang')
		if (language?.dataset.lang !== undefined) {
			setLanguage(language.dataset.lang)
			track('language', language.dataset.lang)
			return
		}

		const file = target?.closest<HTMLElement>('.code-file')
		if (file !== null && file !== undefined) {
			showFile(file)
			return
		}

		const copy = target?.closest<HTMLElement>('[data-copy]')
		if (copy !== null && copy !== undefined) {
			void copyShown(copy)
		}
	}

	node.addEventListener('click', onClick)
	return () => node.removeEventListener('click', onClick)
}

function showFile(tab: HTMLElement): void {
	const variant = tab.closest('.code-variant')
	const shown = tab.dataset.file
	for (const other of variant?.querySelectorAll<HTMLElement>('.code-file') ?? []) {
		other.setAttribute('aria-selected', String(other.dataset.file === shown))
	}
	for (const pane of variant?.querySelectorAll<HTMLElement>('pre[data-file]') ?? []) {
		pane.hidden = pane.dataset.file !== shown
	}
}

async function copyShown(button: HTMLElement): Promise<void> {
	const variant = button.closest<HTMLElement>('.code-variant')
	const text =
		button.dataset.copyText ??
		variant?.querySelector('pre[data-file]:not([hidden])')?.textContent ??
		''

	try {
		await navigator.clipboard.writeText(text)
	} catch {
		return
	}
	button.dataset.copied = ''
	setTimeout(() => delete button.dataset.copied, 1600)
	track('copy-code', variant?.dataset.variant)
}
