<script lang="ts">
	import Seo from '$lib/components/Seo.svelte'
	import { href } from '$lib/href'

	let { data } = $props()
</script>

<Seo title="Not found · TinyStore" description="No page here." origin={data.origin} path="/404" index={false} />

<main>
	<p class="status">404</p>
	<h1>No page here.</h1>
	<p>
		It may have moved when the docs were rewritten. <a href={href('/docs')}>The documentation</a>
		lists every page, and the search in the header finds any of them.
	</p>
</main>

<style>
	main {
		width: min(720px, calc(100% - 44px));
		margin: 0 auto;
		padding: 120px 0;
	}

	.status {
		margin: 0;
		color: var(--ts-accent-text);
		font: 500 14px var(--ts-mono);
	}

	h1 {
		margin: 12px 0 16px;
		font-size: 48px;
		letter-spacing: -0.04em;
	}

	p {
		color: var(--ts-text-lead);
		font-size: 18px;
		line-height: 1.6;
	}

	a {
		color: var(--ts-text);
		text-decoration: underline;
		text-underline-offset: 3px;
	}
</style>
