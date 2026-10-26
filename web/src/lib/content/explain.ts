import type { Element } from 'hast'
import { h } from 'hastscript'

import type { Highlight } from './highlight'

/**
 * A ```explain fence as the page holds it before any script runs: its
 * command, highlighted as a shell's, which the explain takes from
 * `data-command` and reads word by word once it mounts. GitHub shows the
 * fence as the command it is.
 */
export function renderExplain(source: string, highlight: Highlight): Element {
	const command = source.trim().split('\n')[0] ?? ''
	return h('figure.explain', { dataCommand: command }, [highlight(command, 'sh')])
}
