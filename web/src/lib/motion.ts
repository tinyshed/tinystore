import { cubicOut } from 'svelte/easing'

/**
 * How a part of a menu unfolds and folds: a short slide, or none for a reader
 * whose system asks for less motion.
 */
export function unfolding(): { duration: number; easing: (t: number) => number } {
	const still =
		typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches
	return { duration: still ? 0 : 220, easing: cubicOut }
}
