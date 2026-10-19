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

		const call = target?.closest<HTMLAnchorElement>('a.api')
		if (call !== null && call !== undefined && !opensElsewhere(event)) {
			event.preventDefault()
			showSignature(call)
			return
		}

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

// a modified click opens the reference itself, as any link does
function opensElsewhere(event: MouseEvent): boolean {
	return event.metaKey || event.ctrlKey || event.shiftKey || event.button !== 0
}

let popover: HTMLElement | undefined

/**
 * The call's signature and doc under it, with a link to its reference; a
 * second click on the same call, a click elsewhere or Escape closes it.
 */
function showSignature(call: HTMLAnchorElement): void {
	const open = popover?.dataset.for === call.href && popover.isConnected
	closeSignature()
	if (open) {
		return
	}

	const box = document.createElement('div')
	box.className = 'api-pop'
	box.dataset.for = call.href
	box.setAttribute('role', 'dialog')
	box.setAttribute('aria-label', call.textContent ?? 'signature')

	const sig = document.createElement('pre')
	sig.textContent = call.dataset.sig ?? ''
	box.append(sig)
	if (call.dataset.doc) {
		const doc = document.createElement('p')
		doc.textContent = call.dataset.doc.replaceAll('`', '')
		box.append(doc)
	}
	const more = document.createElement('a')
	more.href = call.href
	more.textContent = 'Open in the API reference'
	box.append(more)

	document.body.append(box)
	place(box, call)
	popover = box
	track('signature', call.textContent ?? undefined)

	setTimeout(() => {
		document.addEventListener('click', onOutside)
		document.addEventListener('keydown', onEscape)
	})
}

function place(box: HTMLElement, call: HTMLElement): void {
	const at = call.getBoundingClientRect()
	const width = Math.min(560, document.documentElement.clientWidth - 32)
	box.style.width = `${width}px`
	const left = Math.min(Math.max(16, at.left), document.documentElement.clientWidth - width - 16)
	box.style.left = `${left + window.scrollX}px`
	box.style.top = `${at.bottom + window.scrollY + 6}px`
}

function closeSignature(): void {
	popover?.remove()
	popover = undefined
	document.removeEventListener('click', onOutside)
	document.removeEventListener('keydown', onEscape)
}

function onOutside(event: MouseEvent): void {
	if (!(event.target instanceof Node) || !popover?.contains(event.target)) {
		const onCall = event.target instanceof Element && event.target.closest('a.api') !== null
		if (!onCall) {
			closeSignature()
		}
	}
}

function onEscape(event: KeyboardEvent): void {
	if (event.key === 'Escape') {
		closeSignature()
	}
}
