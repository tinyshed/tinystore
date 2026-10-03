<script lang="ts">
	import { href } from '$lib/href'
	import type { NavGroup } from '$lib/nav'

	let {
		nav,
		current,
		large = false,
	}: { nav: NavGroup[]; current: string; large?: boolean } = $props()
</script>

<nav aria-label="Documentation" class:large>
	{#each nav as group (group.title)}
		<div class="group">
			<p class="title">{group.title}</p>
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
		</div>
	{/each}
</nav>

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

	a:hover {
		color: var(--ts-text);
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
		margin-bottom: 22px;
	}

	.large .title {
		padding: 0 6px 6px;
	}

	.large a,
	.large .planned {
		padding: 10px 12px;
		font-size: 16px;
		border-radius: 8px;
	}
</style>
