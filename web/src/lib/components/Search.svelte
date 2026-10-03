<script lang="ts">
	import { goto } from '$app/navigation'
	import { href } from '$lib/href'
	import { type Entry, type Filter, find, type Hit } from '$lib/search'
	import { track } from '$lib/track'

	import Icon from './Icon.svelte'

	let { open = $bindable(false) }: { open?: boolean } = $props()

	const groupNames: Record<Entry['kind'], string> = {
		guide: 'Guide',
		reference: 'Reference',
		design: 'Design',
	}

	let dialog: HTMLDialogElement | undefined = $state()
	let input: HTMLInputElement | undefined = $state()
	let entries: Entry[] = $state([])
	let query = $state('')
	let filter: Filter = $state('all')
	let selected = $state(0)

	const filters = $derived<Filter[]>([
		'all',
		...(['guide', 'reference', 'design'] as const).filter(kind =>
			entries.some(entry => entry.kind === kind),
		),
		'code',
	])

	const hits = $derived(find(entries, query, filter))

	// in the order the groups show them, so that the arrows walk what the eye sees
	const groups = $derived(
		(['guide', 'reference', 'design'] as const)
			.map(kind => ({ kind, hits: hits.filter(hit => hit.entry.kind === kind) }))
			.filter(group => group.hits.length > 0),
	)
	const ordered = $derived(groups.flatMap(group => group.hits))

	$effect(() => {
		if (open && dialog !== undefined && !dialog.open) {
			dialog.showModal()
			input?.select()
			void loadEntries()
		}
		if (!open && dialog?.open) {
			dialog.close()
		}
	})

	$effect(() => {
		void query
		void filter
		selected = 0
	})

	async function loadEntries() {
		if (entries.length > 0) {
			return
		}
		try {
			entries = await (await fetch(href('/search.json'))).json()
		} catch {
			entries = []
		}
	}

	function onkeydown(event: KeyboardEvent) {
		if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
			event.preventDefault()
			const step = event.key === 'ArrowDown' ? 1 : -1
			selected = (selected + step + ordered.length) % Math.max(ordered.length, 1)
			document.getElementById(`hit-${selected}`)?.scrollIntoView({ block: 'nearest' })
		} else if (event.key === 'Enter') {
			const hit = ordered[selected]
			if (hit !== undefined) {
				event.preventDefault()
				void pick(hit)
			}
		} else if (event.key === 'Tab') {
			event.preventDefault()
			const at = filters.indexOf(filter)
			filter = filters[(at + (event.shiftKey ? -1 : 1) + filters.length) % filters.length] ?? 'all'
		}
	}

	async function pick(hit: Hit) {
		track('search', query.trim().slice(0, 100))
		open = false
		await goto(href(hit.entry.url))
	}

	function label(value: Filter): string {
		return value === 'all' ? 'All' : value === 'code' ? 'Code' : groupNames[value]
	}
</script>

<dialog
	bind:this={dialog}
	aria-label="Search the docs"
	onclose={() => (open = false)}
	onclick={event => {
		if (event.target === dialog) open = false
	}}
>
	<div class="panel">
		<div class="field">
			<span class="glass"><Icon name="search" size={18} /></span>
			<input
				bind:this={input}
				bind:value={query}
				{onkeydown}
				type="search"
				placeholder="Search the docs"
				aria-label="Search the docs"
				autocomplete="off"
				spellcheck="false"
			/>
			<kbd class="esc">esc</kbd>
			<button type="button" class="cancel" onclick={() => (open = false)}>Cancel</button>
		</div>

		<div class="filters" role="tablist" aria-label="What to search">
			{#each filters as value (value)}
				<button
					type="button"
					role="tab"
					aria-selected={filter === value}
					onclick={() => {
						filter = value
						input?.focus()
					}}
				>
					{label(value)}
				</button>
			{/each}
		</div>

		<div class="results" role="listbox" aria-label="Results">
			{#each groups as group (group.kind)}
				<p class="group">{groupNames[group.kind]}</p>
				{#each group.hits as hit (hit.entry.url)}
					{@const at = ordered.indexOf(hit)}
					<a
						id="hit-{at}"
						class="hit"
						href={href(hit.entry.url)}
						role="option"
						aria-selected={at === selected}
						onclick={event => {
							event.preventDefault()
							void pick(hit)
						}}
						onmousemove={() => (selected = at)}
					>
						<span class="mark">#</span>
						<span class="body">
							<span class="where">
								{#if hit.entry.heading !== ''}<span class="page">{`${hit.entry.page} › `}</span>{hit.entry.heading}{:else}{hit.entry.page}{/if}
							</span>
							<!-- snippet() escapes the text and marks the matches -->
							<span class="snippet">{@html hit.snippet}</span>
						</span>
						<kbd class="enter">↵</kbd>
					</a>
				{/each}
			{/each}
			{#if query.trim() !== '' && hits.length === 0}
				<p class="empty">Nothing matches “{query.trim()}”.</p>
			{/if}
		</div>

		<div class="foot">
			<span class="keys">
				<span><kbd>↑</kbd><kbd>↓</kbd> navigate</span>
				<span><kbd>↵</kbd> open</span>
				<span><kbd>⇥</kbd> filter</span>
			</span>
			<span>{hits.length} {hits.length === 1 ? 'result' : 'results'}</span>
		</div>
	</div>
</dialog>

<style>
	dialog {
		width: min(680px, calc(100vw - 32px));
		max-width: none;
		max-height: min(640px, calc(100vh - 160px));
		margin: 120px auto 0;
		padding: 0;
		color: var(--ts-text);
		background: var(--ts-dialog);
		border: 1px solid var(--ts-border-3);
		border-radius: 18px;
		box-shadow: 0 40px 100px -20px var(--ts-shadow);
		overflow: hidden;
	}

	dialog::backdrop {
		background: var(--ts-overlay);
		backdrop-filter: blur(3px);
	}

	.panel {
		display: flex;
		flex-direction: column;
		max-height: inherit;
	}

	.field {
		display: flex;
		flex: none;
		align-items: center;
		gap: 14px;
		height: 64px;
		padding: 0 18px;
		border-bottom: 1px solid var(--ts-border-2);
	}

	.glass {
		display: flex;
		color: var(--ts-muted-2);
	}

	input {
		flex: 1;
		min-width: 0;
		color: var(--ts-text);
		background: none;
		border: 0;
		outline: 0;
		font: 400 19px var(--ts-sans);
		caret-color: var(--ts-accent);
	}

	input::placeholder {
		color: var(--ts-faint);
	}

	input::-webkit-search-cancel-button {
		display: none;
	}

	.cancel {
		display: none;
	}

	.filters {
		display: flex;
		flex: none;
		gap: 8px;
		padding: 12px 18px;
		overflow-x: auto;
		border-bottom: 1px solid var(--ts-line);
	}

	.filters button {
		flex: none;
		padding: 5px 12px;
		color: var(--ts-muted);
		background: none;
		border: 1px solid var(--ts-border-2);
		border-radius: 99px;
		font-size: 13px;
		font-weight: 500;
		cursor: pointer;
	}

	.filters button[aria-selected='true'] {
		color: var(--ts-bg);
		background: var(--ts-text);
		border-color: var(--ts-text);
	}

	.results {
		flex: 1;
		min-height: 0;
		padding: 0 8px 8px;
		overflow-y: auto;
	}

	.group {
		margin: 0;
		padding: 14px 14px 6px;
		color: var(--ts-dim);
		font: 500 11.5px var(--ts-mono);
		letter-spacing: 0.08em;
		text-transform: uppercase;
	}

	.hit {
		display: flex;
		align-items: flex-start;
		gap: 14px;
		padding: 12px 14px;
		border-radius: 10px;
	}

	.hit[aria-selected='true'] {
		background: var(--ts-chip);
	}

	.mark {
		display: flex;
		flex: none;
		align-items: center;
		justify-content: center;
		width: 30px;
		height: 30px;
		margin-top: 1px;
		color: var(--ts-muted-2);
		border: 1px solid var(--ts-border-3);
		border-radius: 8px;
		font: 500 13px var(--ts-mono);
	}

	.hit[aria-selected='true'] .mark {
		color: var(--ts-accent-text);
	}

	.body {
		flex: 1;
		min-width: 0;
	}

	.where {
		display: block;
		font-size: 15.5px;
		font-weight: 500;
	}

	.page {
		color: var(--ts-muted-2);
		font-weight: 400;
	}

	.snippet {
		display: block;
		margin-top: 3px;
		color: var(--ts-muted);
		font-size: 14px;
		line-height: 1.5;
	}

	.snippet :global(mark) {
		color: var(--ts-accent-text);
		background: none;
		font-weight: 600;
	}

	.enter {
		align-self: center;
		visibility: hidden;
	}

	.hit[aria-selected='true'] .enter {
		visibility: visible;
	}

	.empty {
		margin: 0;
		padding: 24px 14px;
		color: var(--ts-muted);
		font-size: 14px;
	}

	.foot {
		display: flex;
		flex: none;
		align-items: center;
		justify-content: space-between;
		padding: 12px 18px;
		color: var(--ts-faint);
		background: var(--ts-dialog-foot);
		border-top: 1px solid var(--ts-border-2);
		font-size: 13px;
	}

	.keys {
		display: flex;
		gap: 18px;
	}

	.keys span {
		display: flex;
		align-items: center;
		gap: 6px;
	}

	@media (max-width: 720px) {
		dialog {
			width: 100vw;
			height: 100dvh;
			max-height: none;
			margin: 0;
			border: 0;
			border-radius: 0;
		}

		.field {
			height: auto;
			padding: 14px;
			gap: 12px;
			border-bottom: 0;
		}

		input {
			font-size: 17px;
		}

		.esc,
		.foot .keys,
		.enter,
		.mark {
			display: none;
		}

		.cancel {
			display: block;
			padding: 0;
			color: var(--ts-accent-text);
			background: none;
			border: 0;
			font-size: 16px;
			font-weight: 500;
			cursor: pointer;
		}

		.filters {
			padding: 0 14px 14px;
		}

		.results {
			padding: 0;
		}

		.hit {
			padding: 14px 18px;
			border-bottom: 1px solid var(--ts-line);
			border-radius: 0;
		}
	}
</style>
