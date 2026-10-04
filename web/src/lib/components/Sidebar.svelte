<script lang="ts">
	import { href } from '$lib/href'
	import type { NavGroup } from '$lib/nav'

	import Icon from './Icon.svelte'

	let {
		nav,
		current,
		large = false,
	}: { nav: NavGroup[]; current: string; large?: boolean } = $props()

	const holds = (group: NavGroup) => group.items.some(item => item.href === current)
</script>

<!-- the menu's groups fold, the current page's open; the sidebar's are all open -->
<nav aria-label="Documentation" class:large>
	{#each nav as group (group.title)}
		{#if large}
			<details class="group" open={holds(group)}>
				<summary class="title">{group.title}<Icon name="chevron" size={16} /></summary>
				{@render items(group)}
			</details>
		{:else}
			<div class="group">
				<p class="title">{group.title}</p>
				{@render items(group)}
			</div>
		{/if}
	{/each}
</nav>

{#snippet items(group: NavGroup)}
	<ul>
		{#each group.items as item (item.title)}
			<li>
				{#if item.href === undefined}
					<span class="planned" title="Not written yet">{item.title}</span>
				{:else}
					<a
						href={item.external ? item.href : href(item.href)}
						aria-current={item.href === current ? 'page' : undefined}
					>
						{item.title}
						{#if item.external}<span class="out" aria-hidden="true">↗</span>{/if}
					</a>
				{/if}
			</li>
		{/each}
	</ul>
{/snippet}

<style>
	.group {
		margin-bottom: 26px;
	}

	.title {
		margin: 0;
		padding: 0 10px 8px;
		color: var(--ts-text);
		font: 500 12px var(--ts-mono);
		letter-spacing: 0.08em;
		text-transform: uppercase;
	}

	ul {
		margin: 0;
		padding: 0;
		list-style: none;
	}

	a,
	.planned {
		display: block;
		padding: 6px 10px;
		font-size: 14.5px;
		line-height: 1.45;
		border-radius: 7px;
	}

	a {
		color: var(--ts-muted);
	}

	@media (hover: hover) {
		a:hover {
			color: var(--ts-text);
			background: var(--ts-row-hover);
		}
	}

	a:active {
		background: var(--ts-pill);
	}

	a[aria-current='page'] {
		color: var(--ts-accent-text);
		background: var(--ts-hover);
		font-weight: 500;
	}

	.planned {
		color: var(--ts-muted);
		opacity: 0.45;
		cursor: default;
	}

	.out {
		margin-left: 2px;
		color: var(--ts-faint);
		font-size: 12px;
	}

	.large .group {
		margin: 0;
		border-bottom: 1px solid var(--ts-line);
	}

	.large .title {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 15px 6px;
		list-style: none;
		cursor: pointer;
	}

	.large .title::-webkit-details-marker {
		display: none;
	}

	.large .title :global(svg) {
		color: var(--ts-faint);
		transition: transform 0.2s;
	}

	.large .group[open] > .title :global(svg) {
		transform: rotate(180deg);
	}

	.large ul {
		padding-bottom: 12px;
	}

	.large a,
	.large .planned {
		padding: 10px 12px;
		font-size: 16px;
		border-radius: 8px;
	}
</style>
