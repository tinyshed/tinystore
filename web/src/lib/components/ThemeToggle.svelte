<script lang="ts">
	import { onMount } from 'svelte'

	import { currentLook, type Look, looks, setLook } from '$lib/prefs'
	import { track } from '$lib/track'

	const groups = (['light', 'dark'] as const).map(theme => ({
		theme,
		label: theme === 'light' ? 'Light' : 'Dark',
		looks: (Object.keys(looks) as Look[]).filter(look => looks[look].theme === theme),
	}))

	let open = $state(false)
	let current: Look = $state('light')
	let menu: HTMLElement | undefined = $state()

	onMount(() => {
		current = currentLook()
	})

	// the menu closes once a look is picked, on Escape, or on a click anywhere else
	$effect(() => {
		if (!open) {
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
		setLook(look)
		current = look
		open = false
		track('theme', look)
	}
</script>

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

<style>
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
		border: 1px solid var(--ts-border);
		border-radius: 9px;
		cursor: pointer;
	}

	@media (hover: hover) {
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
