/**
 * Scrolls a list so that its current page's link sits in its middle: always,
 * or only when the link is out of sight. scrollIntoView would move the page
 * under the list too.
 */
export function revealCurrent(list: HTMLElement | undefined, always = false): void {
	const link = list?.querySelector<HTMLElement>('[aria-current="page"]')
	if (list === undefined || link === null || link === undefined) {
		return
	}
	const box = list.getBoundingClientRect()
	const at = link.getBoundingClientRect()
	if (always || at.top < box.top || at.bottom > box.bottom) {
		list.scrollTop += at.top - box.top - list.clientHeight / 2 + at.height / 2
	}
}
