<script lang="ts">
	import { page } from '$app/state'
	import { href } from '$lib/href'
</script>

<main>
	<p class="status">{page.status}</p>
	<h1>{page.status === 404 ? 'No page here.' : 'Something broke.'}</h1>
	<p><a href={href('/docs')}>The documentation</a> lists every page.</p>
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

	a {
		text-decoration: underline;
		text-underline-offset: 3px;
	}
</style>
