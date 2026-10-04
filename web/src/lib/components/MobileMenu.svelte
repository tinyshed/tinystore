<script lang="ts">
	import { afterNavigate } from '$app/navigation'
	import { href } from '$lib/href'
	import type { NavGroup } from '$lib/nav'
	import { revealCurrent } from '$lib/reveal'

	import Icon from './Icon.svelte'
	import Logo from './Logo.svelte'
	import Sidebar from './Sidebar.svelte'
	import ThemeToggle from './ThemeToggle.svelte'

	let {
		open = $bindable(false),
		nav,
		current,
		onsearch,
	}: { open?: boolean; nav: NavGroup[]; current: string; onsearch: () => void } = $props()

	let dialog: HTMLDialogElement | undefined = $state()
	let content: HTMLElement | undefined = $state()

	$effect(() => {
		if (open && dialog !== undefined && !dialog.open) {
			dialog.showModal()
			revealCurrent(content, true)
		}
		if (!open && dialog?.open) {
			slideAway(dialog)
		}
	})

	afterNavigate(() => {
		open = false
	})

	function slideAway(sheet: HTMLDialogElement) {
		if (matchMedia('(prefers-reduced-motion: reduce)').matches) {
			sheet.close()
			return
		}
		sheet.classList.add('closing')
		const done = () => {
			sheet.classList.remove('closing')
			sheet.close()
		}
		sheet.addEventListener('animationend', done, { once: true })
	}
</script>

<dialog
	bind:this={dialog}
	aria-label="Menu"
	onclose={() => (open = false)}
	oncancel={event => {
		event.preventDefault()
		open = false
	}}
	onclick={event => {
		if (event.target === dialog) open = false
	}}
>
	<div class="top">
		<a class="brand" href={href('/')}><Logo size={24} /><span>TinyStore</span></a>
		<button type="button" class="close" onclick={() => (open = false)} aria-label="Close the menu">
			<Icon name="close" size={22} />
		</button>
	</div>
	<button
		type="button"
		class="search"
		onclick={() => {
			open = false
			onsearch()
		}}
	>
		<Icon name="search" size={16} />
		Search the docs
	</button>
	<div class="content" bind:this={content}>
		<Sidebar {nav} {current} large />
		<div class="theme"><ThemeToggle /> <span>Theme</span></div>
	</div>
</dialog>

<style>
	dialog {
		width: min(380px, 88vw);
		max-width: none;
		height: 100dvh;
		max-height: none;
		margin: 0 0 0 auto;
		padding: 0;
		color: var(--ts-text);
		background: var(--ts-bg);
		border: 0;
		border-left: 1px solid var(--ts-line);
		box-shadow: -24px 0 60px -30px var(--ts-shadow);
	}

	dialog[open] {
		display: flex;
		flex-direction: column;
		animation: slide-in 0.24s cubic-bezier(0.2, 0.8, 0.2, 1);
	}

	dialog::backdrop {
		background: var(--ts-overlay);
		backdrop-filter: blur(2px);
		animation: fade 0.24s ease-out;
	}

	dialog:global(.closing) {
		animation: slide-out 0.18s ease-in forwards;
	}

	dialog:global(.closing)::backdrop {
		animation: fade 0.18s ease-in reverse forwards;
	}

	@media (prefers-reduced-motion: reduce) {
		dialog[open],
		dialog::backdrop {
			animation: none;
		}
	}

	@keyframes slide-in {
		from {
			transform: translateX(100%);
		}
	}

	@keyframes slide-out {
		to {
			transform: translateX(100%);
		}
	}

	@keyframes fade {
		from {
			opacity: 0;
		}
	}

	.top {
		display: flex;
		flex: none;
		align-items: center;
		justify-content: space-between;
		height: 56px;
		padding: 0 10px 0 18px;
		border-bottom: 1px solid var(--ts-line);
	}

	.brand {
		display: flex;
		align-items: center;
		gap: 9px;
		font-size: 17px;
		font-weight: 600;
		letter-spacing: -0.02em;
	}

	.close {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 40px;
		height: 40px;
		padding: 0;
		color: var(--ts-text-body);
		background: none;
		border: 0;
		border-radius: 10px;
		cursor: pointer;
	}

	.close:active {
		background: var(--ts-pill);
	}

	.content {
		flex: 1;
		min-height: 0;
		padding: 4px 16px 28px;
		overflow-y: auto;
		overscroll-behavior: contain;
	}

	.search {
		display: flex;
		flex: none;
		align-items: center;
		gap: 10px;
		height: 44px;
		margin: 14px 16px 6px;
		padding: 0 14px;
		color: var(--ts-faint);
		background: var(--ts-surface);
		border: 1px solid var(--ts-border);
		border-radius: 10px;
		font-size: 15px;
		text-align: left;
		cursor: pointer;
	}

	.search:active {
		background: var(--ts-pill);
	}

	.theme {
		display: flex;
		align-items: center;
		gap: 12px;
		margin-top: 20px;
		padding: 0 6px;
		color: var(--ts-muted);
		font-size: 15px;
	}
</style>
