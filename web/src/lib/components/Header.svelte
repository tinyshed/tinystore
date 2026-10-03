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
</header>

<style>
	header {
		position: sticky;
		top: 0;
		z-index: 20;
		display: flex;
		align-items: center;
		gap: 28px;
		height: var(--ts-header);
		padding: 0 28px;
		background: var(--ts-bg);
		border-bottom: 1px solid var(--ts-line);
	}

	.brand {
		display: flex;
		align-items: center;
		gap: 10px;
		font-size: 18px;
		font-weight: 600;
		letter-spacing: -0.02em;
	}

	.tabs {
		display: flex;
		gap: 4px;
		font-size: 14.5px;
		font-weight: 500;
	}

	.tabs a {
		padding: 7px 12px;
		color: var(--ts-muted);
		border-radius: 8px;
	}

	.tabs a:hover {
		color: var(--ts-text);
	}

	.tabs a[aria-current='page'] {
		color: var(--ts-text);
		background: var(--ts-pill);
	}

	.tools {
		display: flex;
		align-items: center;
		gap: 10px;
		margin-left: auto;
	}

	.search {
		display: flex;
		align-items: center;
		justify-content: space-between;
		width: 176px;
		height: 36px;
		padding: 0 10px 0 12px;
		color: var(--ts-faint);
		background: none;
		border: 1px solid var(--ts-border);
		border-radius: 9px;
		font-size: 14px;
		cursor: pointer;
	}

	.search:hover {
		border-color: var(--ts-border-strong);
	}

	.search kbd {
		color: var(--ts-faint);
		border-color: var(--ts-border);
		background: none;
	}

	.square {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 36px;
		height: 36px;
		color: var(--ts-text-body);
		border: 1px solid var(--ts-border);
		border-radius: 9px;
	}

	.square:hover {
		background: var(--ts-hover);
	}

	.theme {
		display: contents;
	}

	.menu {
		display: none;
		padding: 8px;
		color: var(--ts-text);
		background: none;
		border: 0;
		cursor: pointer;
	}

	@media (max-width: 720px) {
		header {
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

		.tools {
			gap: 6px;
		}

		.search {
			width: auto;
			height: auto;
			padding: 6px 12px;
			color: var(--ts-muted-2);
			border-radius: 8px;
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
