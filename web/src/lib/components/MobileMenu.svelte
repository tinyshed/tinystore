<script lang="ts">
	import { afterNavigate } from '$app/navigation'
	import { href } from '$lib/href'
	import type { NavGroup } from '$lib/nav'

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

	$effect(() => {
		if (open && dialog !== undefined && !dialog.open) {
			dialog.showModal()
		}
		if (!open && dialog?.open) {
			dialog.close()
		}
	})

	afterNavigate(() => {
		open = false
	})
</script>

<dialog bind:this={dialog} aria-label="Menu" onclose={() => (open = false)}>
	<div class="top">
		<a class="brand" href={href('/')}><Logo size={24} /><span>TinyStore</span></a>
		<button type="button" class="close" onclick={() => (open = false)} aria-label="Close the menu">
			<Icon name="close" size={22} />
		</button>
	</div>
	<div class="content">
		<button
			type="button"
			class="search"
			onclick={() => {
				open = false
				onsearch()
			}}
		>
			Search the docs
		</button>
		<Sidebar {nav} {current} large />
		<div class="theme"><ThemeToggle /> <span>Theme</span></div>
	</div>
</dialog>

<style>
	dialog {
		width: 100vw;
		max-width: none;
		height: 100dvh;
		max-height: none;
		margin: 0;
		padding: 0;
		color: var(--ts-text);
		background: var(--ts-bg);
		border: 0;
	}

	.top {
		position: sticky;
		top: 0;
		display: flex;
		align-items: center;
		justify-content: space-between;
		height: 56px;
		padding: 0 14px 0 18px;
		background: var(--ts-bg);
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
		padding: 6px;
		color: var(--ts-text-body);
		background: none;
		border: 0;
		cursor: pointer;
	}

	.content {
		padding: 18px 18px 28px;
	}

	.search {
		display: flex;
		align-items: center;
		width: 100%;
		height: 44px;
		margin-bottom: 24px;
		padding: 0 14px;
		color: var(--ts-faint);
		background: none;
		border: 1px solid var(--ts-border);
		border-radius: 10px;
		font-size: 15px;
		text-align: left;
		cursor: pointer;
	}

	.theme {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 0 6px;
		color: var(--ts-muted);
		font-size: 15px;
	}
</style>
