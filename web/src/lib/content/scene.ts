import type { Element } from 'hast'
import { h } from 'hastscript'

import { parseScene, SceneError } from '../scene/parse'
import type { Highlight } from './highlight'

/**
 * A ```scene fence as the page holds it before any script runs: its code,
 * highlighted and its calls linked like any block's, and its steps as a list.
 * The player takes the scene from `data-scene` and the code's lines from the
 * block, so a reader without scripts, a crawler and a print still get the
 * story. A scene that does not parse is a problem of the page.
 */
export function renderScene(source: string, highlight: Highlight, problems: string[]): Element {
	let scene: ReturnType<typeof parseScene>
	try {
		scene = parseScene(source)
	} catch (error) {
		if (!(error instanceof SceneError)) {
			throw error
		}
		problems.push(error.message)
		return h('figure.scene.scene-broken', h('pre', error.message))
	}

	const code =
		scene.code === undefined
			? []
			: [h('div.scene-code', [highlight(scene.code.text, scene.code.lang)])]
	return h('figure.scene', { dataScene: JSON.stringify(scene) }, [
		...code,
		h(
			'ol.scene-steps',
			scene.steps.map(step => h('li', step.caption)),
		),
	])
}
