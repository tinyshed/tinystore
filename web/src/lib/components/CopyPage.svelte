<script lang="ts">
	import { href } from '$lib/href'
	import { track } from '$lib/track'

	let { markdown }: { markdown: string } = $props()

	let copied = $state(false)

	// the page as markdown that stands on its own: an agent's prompt, an issue, a chat
	async function copy() {
		try {
			const response = await fetch(href(markdown))
			await navigator.clipboard.writeText(await response.text())
		} catch {
			return
		}
		copied = true
		track('copy-page')
		setTimeout(() => (copied = false), 1800)
	}
</script>

<button type="button" class:copied onclick={copy} title="Copy this page as Markdown">
	{copied ? 'Copied to clipboard' : 'Copy as Markdown'}
</button>

<style>
	button {
		display: flex;
		align-items: center;
		padding: 8px 14px;
		color: var(--ts-text-body);
		background: transparent;
		border: 1px solid var(--ts-border-2);
		border-radius: 9px;
		font-size: 14px;
		font-weight: 500;
		white-space: nowrap;
		cursor: pointer;
	}

	@media (hover: hover) {
		button:hover {
			color: var(--ts-text);
			background: var(--ts-hover);
		}
	}

	button:active {
		background: var(--ts-pill);
	}

	.copied {
		color: var(--ts-accent-text);
		background: var(--ts-hover);
		border-color: var(--ts-accent-line);
	}
</style>
