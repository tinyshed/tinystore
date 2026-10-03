<script lang="ts">
	interface Crumb {
		name: string
		path: string
	}

	let {
		title,
		description,
		origin,
		path,
		markdown,
		crumbs = [],
		index = true,
	}: {
		title: string
		description: string
		origin: string
		path: string
		markdown?: string | undefined
		crumbs?: Crumb[]
		index?: boolean
	} = $props()

	const url = $derived(origin + path)

	// breadcrumbs for search engines, written so that no text can end the script early
	const breadcrumbs = $derived(
		crumbs.length === 0
			? ''
			: JSON.stringify({
					'@context': 'https://schema.org',
					'@type': 'BreadcrumbList',
					itemListElement: crumbs.map((crumb, at) => ({
						'@type': 'ListItem',
						position: at + 1,
						name: crumb.name,
						item: origin + crumb.path,
					})),
				}).replace(/</g, '\\u003c'),
	)
</script>

<svelte:head>
	<title>{title}</title>
	<meta name="description" content={description} />
	{#if index}
		<link rel="canonical" href={url} />
	{:else}
		<meta name="robots" content="noindex" />
	{/if}
	{#if markdown !== undefined}
		<link rel="alternate" type="text/markdown" href={origin + markdown} />
	{/if}
	<meta property="og:site_name" content="TinyStore" />
	<meta property="og:type" content={path === '/' ? 'website' : 'article'} />
	<meta property="og:title" content={title} />
	<meta property="og:description" content={description} />
	<meta property="og:url" content={url} />
	<meta property="og:image" content="{origin}/og.png" />
	<meta name="twitter:card" content="summary_large_image" />
	{#if breadcrumbs !== ''}
		{@html `<script type="application/ld+json">${breadcrumbs}</script>`}
	{/if}
</svelte:head>
