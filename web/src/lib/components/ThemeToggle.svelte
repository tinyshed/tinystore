<script lang="ts">
	import { onMount } from 'svelte'

	import { currentLook, type Look, setLook } from '$lib/prefs'
	import { track } from '$lib/track'

	const looks: { key: Look; label: string }[] = [
		{ key: 'light', label: 'Light' },
		{ key: 'paper', label: 'Paper' },
		{ key: 'dark', label: 'Dark' },
		{ key: 'black', label: 'Black' },
	]

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
			{#each looks as look (look.key)}
				<button
					type="button"
					role="menuitemradio"
					aria-checked={current === look.key}
					onclick={() => pick(look.key)}
				>
					<span class="swatch {look.key}">Aa</span>
					<span>{look.label}</span>
				</button>
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
		min-width: 150px;
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

	.swatch {
		display: grid;
		place-items: center;
		width: 26px;
		height: 20px;
		border: 1px solid var(--ts-border-strong);
		border-radius: 5px;
		font: 600 10px var(--ts-sans);
	}

	.swatch.light {
		color: var(--ts-swatch-ink-light);
		background: var(--ts-swatch-light);
	}

	.swatch.paper {
		color: var(--ts-swatch-ink-light);
		background: var(--ts-swatch-paper);
	}

	.swatch.dark {
		color: var(--ts-swatch-ink-dark);
		background: var(--ts-swatch-dark);
	}

	.swatch.black {
		color: var(--ts-swatch-ink-dark);
		background: var(--ts-swatch-black);
	}
</style>
