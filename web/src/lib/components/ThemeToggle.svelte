<script lang="ts">
	import { onMount } from 'svelte'
	import { slide } from 'svelte/transition'

	import { unfolding } from '$lib/motion'
	import { currentLook, type Look, looks, setLook } from '$lib/prefs'
	import { track } from '$lib/track'

	import Icon from './Icon.svelte'

	const groups = (['light', 'dark'] as const).map(theme => ({
		theme,
		label: theme === 'light' ? 'Light' : 'Dark',
		looks: (Object.keys(looks) as Look[]).filter(look => looks[look].theme === theme),
	}))

	// inline, as the phone's menu shows it: every look at once, no list to open
	let { inline = false }: { inline?: boolean } = $props()

	let open = $state(false)
	let current: Look = $state('light')
	let menu: HTMLElement | undefined = $state()

	onMount(() => {
		current = currentLook()
	})

	// the menu closes once a look is picked, on Escape, or on a click anywhere else
	$effect(() => {
		if (!open || inline) {
			return
		}
		const outside = (event: Event) => {
			if (!(event.target instanceof Node && menu?.contains(event.target))) {
				open = false
			}
		}
		const onEscape = (event: KeyboardEvent) => {
			if (event.key === 'Escape') {
				open = false
			}
		}
		document.addEventListener('click', outside)
		document.addEventListener('keydown', onEscape)
		return () => {
			document.removeEventListener('click', outside)
			document.removeEventListener('keydown', onEscape)
		}
	})

	function pick(look: Look) {
		const apply = () => {
			setLook(look)
			current = look
		}
		// the page crossfades from one look to the next, where the browser can and the reader allows
		const still = matchMedia('(prefers-reduced-motion: reduce)').matches
		if (!still && typeof document.startViewTransition === 'function') {
			document.startViewTransition(apply)
		} else {
			apply()
		}
		// the phone's list stays open, so that a reader tries one look after another
		if (!inline) {
			open = false
		}
		track('theme', look)
	}
</script>

{#if inline}
	<div class="inline" class:unfolded={open}>
		<button type="button" class="fold" aria-expanded={open} onclick={() => (open = !open)}>
			<span class="swatch" data-look={current}><span class="ink">Aa</span><span class="dot"></span></span>
			<span class="what">Theme</span>
			<span class="which">{looks[current].label}</span>
			<Icon name="chevron" size={16} />
		</button>
		{#if open}
			<div class="looks-inline" transition:slide={unfolding()}>
				{#each groups as group (group.theme)}
					<p class="group">{group.label}</p>
					<div class="chips">
						{#each group.looks as look (look)}
							<button type="button" class="chip" aria-pressed={current === look} onclick={() => pick(look)}>
								<span class="swatch" data-look={look}><span class="ink">Aa</span><span class="dot"></span></span>
								<span>{looks[look].label}</span>
							</button>
						{/each}
					</div>
				{/each}
			</div>
		{/if}
	</div>
{:else}
<div class="theme" bind:this={menu}>
	<button
		type="button"
		class="toggle"
		onclick={() => (open = !open)}
		title="Theme"
		aria-label="Theme"
		aria-haspopup="menu"
		aria-expanded={open}
	>
		<span class="half"></span>
	</button>
	{#if open}
		<div class="looks" role="menu">
			{#each groups as group (group.theme)}
				<p class="group">{group.label}</p>
				{#each group.looks as look (look)}
					<button
						type="button"
						role="menuitemradio"
						aria-checked={current === look}
						onclick={() => pick(look)}
					>
						<!-- the swatch is the look itself: its tokens, whatever the page's -->
						<span class="swatch" data-look={look}><span class="ink">Aa</span><span class="dot"></span></span>
						<span>{looks[look].label}</span>
					</button>
				{/each}
			{/each}
		</div>
	{/if}
</div>
{/if}

<style>
	.fold {
		display: flex;
		align-items: center;
		gap: 10px;
		width: 100%;
		padding: 12px 2px;
		color: var(--ts-muted);
		background: none;
		border: 0;
		font: 500 14.5px var(--ts-sans);
		cursor: pointer;
	}

	.fold .which {
		margin-left: auto;
		color: var(--ts-text);
	}

	.fold :global(svg) {
		color: var(--ts-faint);
		transition: transform 0.2s;
	}

	.inline.unfolded .fold :global(svg) {
		transform: rotate(180deg);
	}

	.looks-inline {
		padding: 0 2px 12px;
	}

	.inline .group {
		margin: 6px 0;
	}

	.chips {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: 6px;
	}

	.chip {
		display: flex;
		align-items: center;
		gap: 8px;
		min-width: 0;
		padding: 7px 8px;
		color: var(--ts-text-body);
		background: none;
		border: 1px solid var(--ts-border);
		border-radius: 9px;
		font: 500 13.5px var(--ts-sans);
		text-align: left;
		cursor: pointer;
	}

	.chip > span:last-child {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.chip[aria-pressed='true'] {
		color: var(--ts-text);
		background: var(--ts-pill);
		border-color: var(--ts-border-strong);
	}

	.theme {
		position: relative;
	}

	.toggle {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 36px;
		height: 36px;
		padding: 0;
		background: none;
		border: 1px solid transparent;
		border-radius: 8px;
		cursor: pointer;
	}

	@media (hover: hover) {
		.toggle:hover {
			border-color: var(--ts-border);
		}

		.toggle:hover,
		.looks button:hover {
			background: var(--ts-hover);
		}
	}

	.toggle:active {
		background: var(--ts-pill);
	}

	.half {
		width: 16px;
		height: 16px;
		border: 1.5px solid var(--ts-text-body);
		border-radius: 99px;
		background: linear-gradient(90deg, var(--ts-text-body) 50%, transparent 50%);
	}

	.looks {
		position: absolute;
		top: calc(100% + 8px);
		right: 0;
		z-index: 40;
		display: grid;
		gap: 2px;
		min-width: 200px;
		max-height: min(560px, 80vh);
		overflow-y: auto;
		padding: 6px;
		background: var(--ts-dialog);
		border: 1px solid var(--ts-border);
		border-radius: 12px;
		box-shadow: 0 12px 32px var(--ts-shadow);
		transform-origin: top right;
		animation: pop 0.14s ease-out;
	}

	@keyframes pop {
		from {
			opacity: 0;
			transform: translateY(-4px) scale(0.97);
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.looks {
			animation: none;
		}
	}

	.looks button {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 7px 10px;
		color: var(--ts-text-body);
		background: none;
		border: 0;
		border-radius: 8px;
		font: 500 14px var(--ts-sans);
		text-align: left;
		cursor: pointer;
	}

	.looks button[aria-checked='true'] {
		color: var(--ts-text);
		background: var(--ts-pill);
	}

	.group {
		margin: 6px 10px 2px;
		color: var(--ts-faint);
		font: 500 11px / 1.4 var(--ts-mono);
		letter-spacing: 0.08em;
		text-transform: uppercase;
	}

	.swatch {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 4px;
		width: 34px;
		height: 22px;
		padding: 0 5px;
		background: var(--ts-bg);
		border: 1px solid var(--ts-border-strong);
		border-radius: 6px;
	}

	.ink {
		color: var(--ts-text);
		font: 600 10px var(--ts-sans);
	}

	.dot {
		width: 7px;
		height: 7px;
		background: var(--ts-accent);
		border-radius: 50%;
	}
</style>
