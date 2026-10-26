import { track } from './track'

/**
 * A call's signature, opened from its link in a code block: a card under the
 * call on a wide screen, with an arrow at it, and a sheet from the bottom on a
 * phone, which a swipe down closes. A second click on the call, a click
 * elsewhere or Escape closes either, and the focus goes back to the call.
 */

interface Open {
	call: HTMLAnchorElement
	box: HTMLElement
	scrim?: HTMLElement
}

let open: Open | undefined

const phone = () => matchMedia('(max-width: 640px), (pointer: coarse)').matches

export function showSignature(call: HTMLAnchorElement): void {
	if (open?.call === call) {
		closeSignature()
		return
	}
	closeSignature(true)

	const sheet = phone()
	const box = card(call, sheet)
	const scrim = sheet ? document.createElement('div') : undefined
	if (scrim) {
		scrim.className = 'api-scrim'
		scrim.addEventListener('click', () => closeSignature())
		document.body.append(scrim)
	}
	document.body.append(box)
	if (sheet) {
		box.classList.add('sheet')
		dragToClose(box)
	} else {
		place(box, call)
	}
	call.setAttribute('aria-expanded', 'true')
	open = { call, box, ...(scrim ? { scrim } : {}) }
	box.querySelector<HTMLElement>('.api-close')?.focus({ preventScroll: true })
	track('signature', call.dataset.name)

	setTimeout(() => {
		document.addEventListener('click', onOutside)
		document.addEventListener('keydown', onEscape)
		addEventListener('resize', onResize)
	})
}

function card(call: HTMLAnchorElement, sheet: boolean): HTMLElement {
	if (call.classList.contains('contract')) {
		return contractCard(call, sheet)
	}
	const name = call.dataset.name ?? call.textContent ?? ''
	const dot = name.lastIndexOf('.')
	const owner = dot === -1 ? '' : name.slice(0, dot)
	const box = document.createElement('div')
	box.className = 'api-pop'
	box.setAttribute('role', 'dialog')
	box.setAttribute('aria-label', name)

	const head = element('div', 'api-head')
	if (sheet) box.append(element('span', 'api-grip'))
	head.append(
		element('span', 'api-kind', owner === '' ? 'function' : owner),
		element('span', 'api-name', dot === -1 ? name : name.slice(dot + 1)),
	)
	const close = element('button', 'api-close', '×')
	close.setAttribute('aria-label', 'Close')
	close.addEventListener('click', () => closeSignature())
	head.append(close)

	const forms = Number(call.dataset.forms ?? 0)
	box.append(
		head,
		element('pre', 'api-sig', call.dataset.sig ?? ''),
		...(forms > 0
			? [
					element(
						'p',
						'api-forms',
						`+ ${forms} more ${forms === 1 ? 'form' : 'forms'} in the reference`,
					),
				]
			: []),
		docOf(call.dataset.doc ?? ''),
		foot(call),
	)
	return box
}

/** A contract's id in the prose, opened as a call is: its promise, and CONTRACTS.md a link away. */
function contractCard(chip: HTMLAnchorElement, sheet: boolean): HTMLElement {
	const id = chip.textContent ?? ''
	const box = document.createElement('div')
	box.className = 'api-pop'
	box.setAttribute('role', 'dialog')
	box.setAttribute('aria-label', id)

	const head = element('div', 'api-head')
	if (sheet) box.append(element('span', 'api-grip'))
	const close = element('button', 'api-close', '×')
	close.setAttribute('aria-label', 'Close')
	close.addEventListener('click', () => closeSignature())
	head.append(element('span', 'api-kind', 'contract'), element('span', 'api-name', id), close)

	const foot = element('div', 'api-foot')
	const link = element('a', '', 'Open CONTRACTS.md ↗') as HTMLAnchorElement
	link.href = chip.href
	foot.append(link)

	box.append(head, docOf(chip.dataset.contract ?? ''), foot)
	return box
}

// the doc's first paragraph, its code spans as code
function docOf(doc: string): HTMLElement {
	const p = element('p', doc === '' ? 'api-doc none' : 'api-doc')
	if (doc === '') {
		p.textContent = 'No doc comment yet.'
		return p
	}
	for (const [index, part] of doc.split('`').entries()) {
		p.append(index % 2 === 1 ? element('code', '', part) : document.createTextNode(part))
	}
	return p
}

function foot(call: HTMLAnchorElement): HTMLElement {
	const foot = element('div', 'api-foot')
	const link = element('a', '', 'Open in the API reference →') as HTMLAnchorElement
	link.href = call.href
	const copy = element('button', 'api-copy', 'Copy')
	copy.addEventListener('click', async () => {
		try {
			await navigator.clipboard.writeText(call.dataset.sig ?? '')
			copy.textContent = 'Copied'
		} catch {
			copy.textContent = 'Select it above'
		}
		setTimeout(() => {
			copy.textContent = 'Copy'
		}, 1400)
	})
	foot.append(link, element('span', 'api-where', call.dataset.page ?? ''), copy)
	return foot
}

function element(tag: string, className: string, text?: string): HTMLElement {
	const node = document.createElement(tag)
	if (className) node.className = className
	if (text !== undefined) node.textContent = text
	return node
}

// under the call, or above it when there is no room below; the arrow points at it
function place(box: HTMLElement, call: HTMLElement): void {
	const at = call.getBoundingClientRect()
	const viewport = document.documentElement.clientWidth
	const width = box.offsetWidth
	const height = box.offsetHeight
	const left = Math.min(Math.max(16, at.left - 18), viewport - width - 16)
	const above = at.bottom + height + 12 > innerHeight && at.top - height - 12 > 0
	box.classList.toggle('above', above)
	box.style.left = `${left + scrollX}px`
	box.style.top = `${(above ? at.top - height - 10 : at.bottom + 10) + scrollY}px`
	box.style.setProperty(
		'--arrow',
		`${Math.max(16, Math.min(width - 16, at.left + at.width / 2 - left))}px`,
	)
}

export function closeSignature(now = false): void {
	if (!open) return
	const { call, box, scrim } = open
	open = undefined
	call.setAttribute('aria-expanded', 'false')
	document.removeEventListener('click', onOutside)
	document.removeEventListener('keydown', onEscape)
	removeEventListener('resize', onResize)
	if (now || matchMedia('(prefers-reduced-motion: reduce)').matches) {
		box.remove()
		scrim?.remove()
		return
	}
	box.classList.add('closing')
	scrim?.classList.add('closing')
	// gone once its animation ends, or soon anyway: a scrim left behind would take every tap
	const remove = () => {
		box.remove()
		scrim?.remove()
	}
	box.addEventListener('animationend', remove, { once: true })
	setTimeout(remove, 400)
	call.focus({ preventScroll: true })
}

function onOutside(event: MouseEvent): void {
	const target = event.target
	if (!open || !(target instanceof Element)) return
	if (!open.box.contains(target) && target.closest('a.api, a.contract') === null) closeSignature()
}

function onEscape(event: KeyboardEvent): void {
	if (event.key === 'Escape') closeSignature()
}

function onResize(): void {
	if (open && !open.box.classList.contains('sheet')) place(open.box, open.call)
}

function dragToClose(box: HTMLElement): void {
	let start: number | undefined
	box.addEventListener(
		'touchstart',
		event => {
			if (box.scrollTop === 0) start = event.touches[0]?.clientY
		},
		{ passive: true },
	)
	box.addEventListener(
		'touchmove',
		event => {
			if (start === undefined) return
			const dy = Math.max(0, (event.touches[0]?.clientY ?? start) - start)
			box.style.transform = `translateY(${dy}px)`
		},
		{ passive: true },
	)
	box.addEventListener('touchend', event => {
		if (start === undefined) return
		const dy = (event.changedTouches[0]?.clientY ?? start) - start
		start = undefined
		box.style.transform = ''
		if (dy > 80) closeSignature()
	})
}
