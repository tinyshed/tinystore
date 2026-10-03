<script lang="ts">
	import { codeBlocks } from '$lib/code-blocks'
	import CopyPage from '$lib/components/CopyPage.svelte'
	import Outline from '$lib/components/Outline.svelte'
	import Pager from '$lib/components/Pager.svelte'
	import Seo from '$lib/components/Seo.svelte'
	import { rebase } from '$lib/href'

	let { data } = $props()

	const page = $derived(data.page)
	const updated = $derived(
		page.updated === undefined
			? undefined
			: new Date(page.updated).toLocaleDateString('en-GB', {
					day: 'numeric',
					month: 'long',
					year: 'numeric',
					timeZone: 'UTC',
				}),
	)
</script>

<Seo
	title={page.url === '/docs' ? 'TinyStore documentation' : `${page.title} · TinyStore`}
	description={page.lead ?? page.title}
	origin={data.origin}
	path={page.url}
	markdown={page.markdown}
	index={!page.hidden}
	crumbs={[
		{ name: 'Docs', path: '/docs' },
		...(page.url === '/docs' ? [] : [{ name: page.title, path: page.url }]),
	]}
/>

<div class="page">
	<div class="mobile-bar">
		<span class="crumbs">
			{#if page.section !== ''}{page.section} <span class="slash">/</span>{/if}
			<span class="here">{page.title}</span>
		</span>
		{#if page.headings.length > 0}
			<details class="toc">
				<summary>On this page ▾</summary>
				<ul>
					{#each page.headings as heading (heading.id)}
						<li><a href="#{heading.id}">{heading.text}</a></li>
					{/each}
				</ul>
			</details>
		{/if}
	</div>

	<article>
		<p class="crumbs desktop">
			{#if page.section !== ''}{page.section} <span class="slash">/</span>{/if}
			<span class="here">{page.title}</span>
		</p>
		<div class="title">
			<h1>{page.title}</h1>
			<CopyPage markdown={page.markdown} />
		</div>

		<div class="prose" {@attach codeBlocks}>
			{@html rebase(page.html)}
		</div>

		<p class="meta">
			<a href={page.editUrl}>Edit this page on GitHub</a>
			{#if updated !== undefined}<span>Updated {updated}</span>{/if}
		</p>

		<Pager previous={page.previous} next={page.next} />
	</article>

	<aside class="outline">
		<Outline headings={page.headings} />
	</aside>
</div>

<style>
	.page {
		display: grid;
		grid-template-columns: minmax(0, 880px) minmax(0, 280px);
	}

	article {
		min-width: 0;
		padding: 56px 48px 96px;
	}

	.crumbs {
		color: var(--ts-faint);
		font: 13px var(--ts-mono);
	}

	.desktop {
		margin: 0 0 18px;
	}

	.slash {
		color: var(--ts-border-strong);
	}

	.here {
		color: var(--ts-accent-text);
	}

	.title {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 16px;
		margin-bottom: 18px;
	}

	.title :global(button) {
		margin-top: 16px;
	}

	h1 {
		margin: 0;
		font-size: 60px;
		font-weight: 600;
		line-height: 1.05;
		letter-spacing: -0.045em;
	}

	.meta {
		display: flex;
		flex-wrap: wrap;
		justify-content: space-between;
		gap: 12px;
		margin: 56px 0 0;
		padding-top: 18px;
		color: var(--ts-faint);
		border-top: 1px solid var(--ts-line);
		font: 13px var(--ts-mono);
	}

	.meta a:hover {
		color: var(--ts-accent-text);
	}

	.outline {
		position: sticky;
		top: var(--ts-header);
		align-self: start;
		max-height: calc(100vh - var(--ts-header));
		padding: 60px 24px 24px 8px;
		overflow-y: auto;
		scrollbar-width: thin;
	}

	.mobile-bar {
		display: none;
	}

	@media (max-width: 1280px) {
		.page {
			grid-template-columns: minmax(0, 1fr);
		}

		.outline {
			display: none;
		}
	}

	@media (max-width: 720px) {
		article {
			padding: 30px 22px 40px;
		}

		.desktop {
			display: none;
		}

		.mobile-bar {
			position: sticky;
			top: 56px;
			z-index: 10;
			display: flex;
			align-items: center;
			justify-content: space-between;
			gap: 12px;
			padding: 12px 18px;
			background: var(--ts-bg);
			border-bottom: 1px solid var(--ts-line);
			font-size: 12.5px;
		}

		.mobile-bar .crumbs {
			overflow: hidden;
			text-overflow: ellipsis;
			white-space: nowrap;
		}

		.toc {
			position: relative;
			flex: none;
		}

		.toc summary {
			padding: 4px 12px;
			color: var(--ts-text-body);
			border: 1px solid var(--ts-border);
			border-radius: 99px;
			font-size: 13px;
			list-style: none;
			cursor: pointer;
		}

		.toc summary::-webkit-details-marker {
			display: none;
		}

		.toc ul {
			position: absolute;
			right: 0;
			width: min(280px, 80vw);
			margin: 8px 0 0;
			padding: 8px;
			list-style: none;
			background: var(--ts-dialog);
			border: 1px solid var(--ts-border-3);
			border-radius: 12px;
			box-shadow: 0 20px 50px -20px var(--ts-shadow);
		}

		.toc a {
			display: block;
			padding: 8px 10px;
			color: var(--ts-muted);
			border-radius: 8px;
			font-size: 14px;
		}

		.toc a:hover {
			color: var(--ts-text);
			background: var(--ts-hover);
		}

		.title {
			flex-wrap: wrap;
			align-items: center;
			gap: 12px;
		}

		.title :global(button) {
			margin-top: 0;
		}

		h1 {
			font-size: 44px;
		}
	}
</style>
