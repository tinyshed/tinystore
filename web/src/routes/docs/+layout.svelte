<script lang="ts">
	import { afterNavigate } from '$app/navigation'
	import { page } from '$app/state'
	import Sidebar from '$lib/components/Sidebar.svelte'
	import { unbase } from '$lib/href'
	import { sidebarPath } from '$lib/nav'
	import { revealCurrent } from '$lib/reveal'

	let { data, children } = $props()

	let sidebar: HTMLElement | undefined = $state()

	afterNavigate(() => revealCurrent(sidebar))
</script>

<div class="docs">
	<aside class="sidebar" bind:this={sidebar}>
		<Sidebar nav={data.nav} current={sidebarPath(unbase(page.url.pathname))} />
	</aside>
	{@render children()}
</div>

<style>
	.docs {
		display: grid;
		grid-template-columns: 280px minmax(0, 1fr);
		max-width: 1440px;
		margin: 0 auto;
	}

	.sidebar {
		position: sticky;
		top: var(--ts-header);
		height: calc(100vh - var(--ts-header));
		padding: 32px 20px 32px 28px;
		overflow-y: auto;
		border-right: 1px solid var(--ts-line);
		scrollbar-width: thin;
	}

	@media (max-width: 1024px) {
		.docs {
			grid-template-columns: minmax(0, 1fr);
		}

		.sidebar {
			display: none;
		}
	}
</style>
