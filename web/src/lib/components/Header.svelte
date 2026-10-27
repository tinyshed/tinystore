<script lang="ts">
	import { onMount } from 'svelte'

	import { href } from '$lib/href'
	import { repository } from '$lib/site'

	import Icon from './Icon.svelte'
	import Logo from './Logo.svelte'
	import ThemeToggle from './ThemeToggle.svelte'

	interface Tab {
		title: string
		href: string
		active: boolean
	}

	let { tabs, onsearch, onmenu }: { tabs: Tab[]; onsearch: () => void; onmenu: () => void } =
		$props()

	let shortcut = $state('⌘K')

	onMount(() => {
		if (!/Mac|iPhone|iPad/.test(navigator.platform)) {
			shortcut = 'Ctrl K'
		}
	})
</script>

<header>
	<div class="bar">
		<a class="brand" href={href('/')} aria-label="TinyStore, home">
			<Logo />
			<span>TinyStore</span>
		</a>

		<nav class="tabs" aria-label="Sections">
			{#each tabs as tab (tab.href)}
				<a href={href(tab.href)} aria-current={tab.active ? 'page' : undefined}>{tab.title}</a>
			{/each}
		</nav>

		<div class="tools">
			<button type="button" class="search" onclick={onsearch}>
				<Icon name="search" size={15} />
				<span>Search</span>
				<kbd>{shortcut}</kbd>
			</button>
			<a class="square github" href={repository} title="GitHub" aria-label="TinyStore on GitHub">
				<Icon name="github" />
			</a>
			<span class="theme"><ThemeToggle /></span>
			<button type="button" class="menu" onclick={onmenu} aria-label="Open the menu">
				<Icon name="menu" size={20} />
			</button>
		</div>
	</div>
</header>

<style>
	header {
		position: sticky;
		top: 0;
		z-index: 20;
		background: var(--ts-bg);
		border-bottom: 1px solid var(--ts-line);
	}

	/* the bar keeps to the docs' own width, so that it lines up with the page under it */
	.bar {
		display: flex;
		align-items: center;
		gap: 32px;
		max-width: 1440px;
		height: var(--ts-header);
		margin: 0 auto;
		padding: 0 28px;
	}

	.brand {
		display: flex;
		flex: none;
		align-items: center;
		gap: 10px;
		font-size: 18px;
		font-weight: 600;
		letter-spacing: -0.02em;
	}

	/* the parts of the docs, the one read marked on the bar's own hairline */
	.tabs {
		display: flex;
		align-self: stretch;
		gap: 24px;
		font-size: 14.5px;
		font-weight: 500;
	}

	.tabs a {
		display: flex;
		align-items: center;
		margin-bottom: -1px;
		color: var(--ts-muted);
		border-bottom: 2px solid transparent;
	}

	.tabs a[aria-current='page'] {
		color: var(--ts-text);
		border-bottom-color: var(--ts-accent);
	}

	.tools {
		display: flex;
		align-items: center;
		gap: 6px;
		margin-left: auto;
	}

	/* the search as a field to type into, the one wide control of the bar */
	.search {
		display: flex;
		align-items: center;
		gap: 8px;
		width: 240px;
		height: 36px;
		margin-right: 4px;
		padding: 0 6px 0 11px;
		color: var(--ts-faint);
		background: var(--ts-surface);
		border: 1px solid var(--ts-border-2);
		border-radius: 8px;
		font-size: 14px;
		cursor: pointer;
	}

	.search span {
		flex: 1;
		text-align: left;
	}

	.search kbd {
		background: var(--ts-bg);
	}

	.square,
	.menu {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 36px;
		height: 36px;
		padding: 0;
		color: var(--ts-text-body);
		background: none;
		border: 1px solid transparent;
		border-radius: 8px;
		cursor: pointer;
	}

	@media (hover: hover) {
		.tabs a:hover {
			color: var(--ts-text);
		}

		.search:hover {
			color: var(--ts-text-body);
			border-color: var(--ts-border-strong);
		}

		.square:hover,
		.menu:hover {
			background: var(--ts-hover);
			border-color: var(--ts-border);
		}
	}

	.search:active,
	.square:active,
	.menu:active {
		background: var(--ts-pill);
	}

	.theme {
		display: contents;
	}

	.menu {
		display: none;
		width: 40px;
		height: 40px;
		color: var(--ts-text);
	}

	@media (max-width: 720px) {
		.bar {
			gap: 12px;
			height: 56px;
			padding: 0 12px 0 18px;
		}

		.brand {
			gap: 9px;
			font-size: 17px;
		}

		.tabs,
		.github,
		.theme {
			display: none;
		}

		.search {
			width: auto;
			height: 36px;
			margin-right: 0;
			padding: 0 12px;
			color: var(--ts-muted-2);
			font-size: 13.5px;
		}

		.search kbd {
			display: none;
		}

		.menu {
			display: flex;
		}
	}
</style>
