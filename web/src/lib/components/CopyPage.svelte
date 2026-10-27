<script lang="ts">
	import { href } from '$lib/href'
	import { track } from '$lib/track'

	import Icon from './Icon.svelte'

	/**
	 * What a reader can do with the page beyond reading it: copy it as
	 * markdown, the one press of the key, and from its menu read that markdown
	 * or hand the page to an assistant, which reads it from the site.
	 */
	let { markdown, origin }: { markdown: string; origin: string } = $props()

	let copied = $state(false)
	let open = $state(false)
	let root: HTMLElement | undefined = $state()

	// an assistant reads the page's markdown from the site itself, so it gets this version of it
	const prompt = $derived(`Read from ${origin}${markdown} so I can ask questions about it.`)
	const assistants = $derived([
		{
			key: 'claude',
			name: 'Claude',
			mark: 'claude',
			href: `https://claude.ai/new?q=${encodeURIComponent(prompt)}`,
		},
		{
			key: 'chatgpt',
			name: 'ChatGPT',
			mark: 'openai',
			href: `https://chatgpt.com/?hints=search&prompt=${encodeURIComponent(prompt)}`,
		},
	] as const)

	// the page as markdown that stands on its own: an agent's prompt, an issue, a chat
	async function copy() {
		open = false
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

	// the menu closes on a press anywhere else, on Escape, and once an item is picked
	$effect(() => {
		if (!open) {
			return
		}
		const away = (event: Event) => {
			if (!(event.target instanceof Node && root?.contains(event.target))) {
				open = false
			}
		}
		const onkey = (event: KeyboardEvent) => {
			if (event.key === 'Escape') {
				open = false
				root?.querySelector<HTMLElement>('.more')?.focus()
			}
		}
		document.addEventListener('pointerdown', away)
		document.addEventListener('keydown', onkey)
		return () => {
			document.removeEventListener('pointerdown', away)
			document.removeEventListener('keydown', onkey)
		}
	})
</script>

<div class="page-menu" bind:this={root}>
	<div class="split" class:copied>
		<button type="button" class="copy" onclick={copy} title="Copy this page as Markdown">
			<Icon name="copy" size={15} />
			<span>{copied ? 'Copied' : 'Copy page'}</span>
		</button>
		<button
			type="button"
			class="more"
			aria-label="More ways to use this page"
			aria-haspopup="menu"
			aria-expanded={open}
			onclick={() => (open = !open)}
		>
			<Icon name="chevron" size={16} />
		</button>
	</div>

	{#if open}
		<div class="items" role="menu">
			<button type="button" role="menuitem" onclick={copy}>
				<span class="mark"><Icon name="copy" size={16} /></span>
				<span class="words">
					<span class="what">Copy page</span>
					<span class="why">As Markdown, for a chat with an assistant</span>
				</span>
			</button>
			<a role="menuitem" href={href(markdown)} onclick={() => (open = false)}>
				<span class="mark"><Icon name="markdown" size={16} /></span>
				<span class="words">
					<span class="what">View as Markdown</span>
					<span class="why">The page as plain text</span>
				</span>
			</a>
			{#each assistants as assistant (assistant.key)}
				<a
					role="menuitem"
					href={assistant.href}
					target="_blank"
					rel="noopener"
					onclick={() => {
						open = false
						track('ask-ai', assistant.key)
					}}
				>
					<span class="mark {assistant.key}"><Icon name={assistant.mark} size={16} /></span>
					<span class="words">
						<span class="what">Open in {assistant.name}</span>
						<span class="why">Ask questions about this page</span>
					</span>
					<span class="out"><Icon name="external" size={13} /></span>
				</a>
			{/each}
		</div>
	{/if}
</div>

<style>
	.page-menu {
		position: relative;
		flex: none;
	}

	/* the copy and the menu as one key, split by a hairline */
	.split {
		display: flex;
		background: var(--ts-key-face);
		border: 1px solid var(--ts-border-3);
		border-radius: 9px;
		box-shadow: var(--ts-shadow-key);
	}

	.split button {
		display: flex;
		align-items: center;
		height: 38px;
		color: var(--ts-text-body);
		background: none;
		border: 0;
		font-size: 14px;
		font-weight: 500;
		white-space: nowrap;
		cursor: pointer;
	}

	.split .copy {
		gap: 8px;
		padding: 0 12px 0 13px;
		border-radius: 8px 0 0 8px;
	}

	.split .more {
		justify-content: center;
		width: 34px;
		border-left: 1px solid var(--ts-border-2);
		border-radius: 0 8px 8px 0;
	}

	.more :global(svg) {
		color: var(--ts-faint);
		transition: transform 0.2s;
	}

	.more[aria-expanded='true'] :global(svg) {
		transform: rotate(180deg);
	}

	@media (hover: hover) {
		.split button:hover {
			color: var(--ts-text);
			background: var(--ts-hover);
		}
	}

	.split button:active {
		background: var(--ts-pill);
	}

	.copied .copy {
		color: var(--ts-accent-text);
	}

	.items {
		position: absolute;
		top: calc(100% + 10px);
		right: 0;
		z-index: 30;
		display: grid;
		gap: 2px;
		width: min(320px, 86vw);
		padding: 6px;
		background: var(--ts-dialog);
		border: 1px solid var(--ts-border-3);
		border-radius: 12px;
		box-shadow: 0 16px 40px -16px var(--ts-shadow);
		transform-origin: top right;
		animation: pop 0.14s ease-out;
	}

	@keyframes pop {
		from {
			opacity: 0;
			transform: translateY(-4px);
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.items {
			animation: none;
		}

		.more :global(svg) {
			transition: none;
		}
	}

	.items button,
	.items a {
		display: grid;
		grid-template-columns: 32px minmax(0, 1fr) auto;
		align-items: center;
		gap: 12px;
		padding: 8px 10px 8px 8px;
		color: var(--ts-text-body);
		background: none;
		border: 0;
		border-radius: 8px;
		font: inherit;
		text-align: left;
		cursor: pointer;
	}

	@media (hover: hover) {
		.items button:hover,
		.items a:hover {
			background: var(--ts-hover);
		}
	}

	.items button:active,
	.items a:active {
		background: var(--ts-pill);
	}

	.mark {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 32px;
		height: 32px;
		color: var(--ts-text-body);
		border: 1px solid var(--ts-border-2);
		border-radius: 8px;
	}

	.mark.claude {
		color: var(--ts-brand-claude);
	}

	.words {
		display: grid;
		gap: 1px;
		min-width: 0;
	}

	.what {
		color: var(--ts-text);
		font-size: 14px;
		font-weight: 500;
	}

	.why {
		color: var(--ts-muted);
		font-size: 12.5px;
		line-height: 1.4;
	}

	.out {
		display: flex;
		color: var(--ts-faint);
	}
</style>
