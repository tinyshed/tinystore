<script lang="ts">
	import { currentTheme, setTheme } from '$lib/prefs'
	import { track } from '$lib/track'

	function toggle() {
		const next = currentTheme() === 'dark' ? 'light' : 'dark'
		setTheme(next)
		track('theme', next)
	}
</script>

<button type="button" class="toggle" onclick={toggle} title="Switch the theme" aria-label="Switch the theme">
	<span class="half"></span>
</button>

<style>
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
		.toggle:hover {
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
</style>
