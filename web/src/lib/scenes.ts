import { mount, unmount } from 'svelte'
import type { Attachment } from 'svelte/attachments'

import type { Scene } from './scene/parse'
import ScenePlayer from './scene/Scene.svelte'

/**
 * Plays the scenes of HTML that arrives through {@html}: each `figure.scene`
 * the build wrote, its code and its steps as a list for a reader without
 * scripts, becomes a player. The attachment runs again when the HTML does.
 */
export function scenes(html: string): Attachment<HTMLElement> {
	return node => {
		void html
		const players = [...node.querySelectorAll<HTMLElement>('figure.scene[data-scene]')].map(
			figure => {
				const scene = JSON.parse(figure.dataset.scene ?? '{}') as Scene
				const codeHtml = figure.querySelector('.scene-code')?.innerHTML ?? ''
				figure.replaceChildren()
				figure.classList.add('playing')
				return mount(ScenePlayer, { target: figure, props: { scene, codeHtml } })
			},
		)
		return () => {
			for (const player of players) void unmount(player)
		}
	}
}
