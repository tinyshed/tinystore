<script lang="ts">
	import { href } from '$lib/href'

	interface Neighbour {
		title: string
		url: string
	}

	let { previous, next }: { previous?: Neighbour | undefined; next?: Neighbour | undefined } =
		$props()
</script>

{#if previous !== undefined || next !== undefined}
	<nav class="pager" aria-label="Previous and next">
		{#if previous !== undefined}
			<a href={href(previous.url)}>
				<span class="label">Previous</span>
				<span class="name">{previous.title}</span>
			</a>
		{:else}
			<span></span>
		{/if}
		{#if next !== undefined}
			<a class="next" href={href(next.url)}>
				<span class="label">Next</span>
				<span class="name">{next.title}</span>
			</a>
		{/if}
	</nav>
{/if}

<style>
	.pager {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: 16px;
		margin-top: 72px;
	}

	a {
		display: block;
		padding: 18px 22px;
		border: 1px solid var(--ts-border);
		border-radius: 12px;
	}

	@media (hover: hover) {
		a:hover {
			border-color: var(--ts-border-strong);
			background: var(--ts-row-hover);
		}
	}

	a:active {
		background: var(--ts-hover);
	}

	.next {
		text-align: right;
	}

	.label {
		display: block;
		color: var(--ts-faint);
		font: 12px var(--ts-mono);
	}

	.name {
		display: block;
		margin-top: 6px;
		font-size: 18px;
		font-weight: 600;
	}

	.next .name {
		color: var(--ts-accent-text);
	}

	@media (max-width: 720px) {
		.pager {
			grid-template-columns: 1fr;
			gap: 12px;
			margin-top: 56px;
		}
	}
</style>
