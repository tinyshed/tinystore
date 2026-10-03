<script lang="ts">
	let { size = 28 }: { size?: number } = $props()
</script>

<svg width={size} height={size} viewBox="0 0 32 32" aria-hidden="true">
	<path class="top" d="M16 3l14 7-14 7-14-7z" />
	<path class="bands" d="M2 13l14 7 14-7v3l-14 7-14-7zm0 6l14 7 14-7v3l-14 7-14-7z" />
</svg>

<style>
	svg {
		display: block;
		flex: none;
	}

	.top {
		fill: var(--ts-logo-top);
	}

	.bands {
		fill: var(--ts-text);
	}
</style>
