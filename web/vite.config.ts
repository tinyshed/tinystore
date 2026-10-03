import { resolve } from 'node:path'
import adapter from '@sveltejs/adapter-static'
import { sveltekit } from '@sveltejs/kit/vite'
import { defineConfig, type Plugin } from 'vite'

// SITE_PORTABLE=1 builds a copy to open from any folder, into SITE_OUT
const portable = process.env.SITE_PORTABLE === '1'
const output = process.env.SITE_OUT ?? 'build'
// SITE_BASE=/tinystore serves the site under that path, behind a proxy that strips it
const base = process.env.SITE_BASE ?? ''

export default defineConfig({
	plugins: [
		sveltekit({
			compilerOptions: {
				// Force runes mode for the project, except for libraries. Can be removed in svelte 6.
				runes: ({ filename }) =>
					filename.split(/[/\\]/).includes('node_modules') ? undefined : true,
			},

			// Every page is prerendered: the server hands out files and renders nothing.
			adapter: adapter({ pages: output, assets: output, precompress: !portable, strict: true }),

			// Absolute, so that 404.html finds its assets whatever path it answers; a
			// portable build, a preview's, links relatively and opens from any folder.
			paths: { relative: portable, base: base as '' | `/${string}` },

			prerender: {
				// a link that leads nowhere fails the build, not the reader
				handleHttpError: 'fail',
				handleMissingId: 'fail',
				// while no page shows a file of the repository, /files/ has nothing to make
				handleUnseenRoutes: ({ routes, message }) => {
					if (routes.some(route => route !== '/files/[...path]')) {
						throw new Error(message)
					}
				},
			},
		}),
		reloadOnDocs(),
	],
})

// The pages read docs/ outside Vite's module graph, so a change there reloads by hand.
function reloadOnDocs(): Plugin {
	const watched = [resolve('../docs'), resolve('../.github/assets'), resolve('kitchen-sink.md')]

	return {
		name: 'tinystore:reload-on-docs',
		configureServer(server) {
			server.watcher.add(watched)
			server.watcher.on('all', (_, file) => {
				if (watched.some(dir => file.startsWith(dir))) {
					server.ws.send({ type: 'full-reload' })
				}
			})
		},
	}
}
