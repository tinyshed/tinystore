<script lang="ts">
	let { headings }: { headings: { id: string; text: string }[] } = $props()

	let active = $state('')

	// the heading last scrolled past the top, and the last one at the page's end
	$effect(() => {
		const ids = headings.map(heading => heading.id)

		const update = () => {
			let current = ids[0] ?? ''
			for (const id of ids) {
				const top = document.getElementById(id)?.getBoundingClientRect().top
				if (top !== undefined && top < 140) {
					current = id
				}
			}
			if (innerHeight + scrollY >= document.documentElement.scrollHeight - 4) {
				current = ids.at(-1) ?? current
			}
			active = current
		}

		update()
		addEventListener('scroll', update, { passive: true })
		return () => removeEventListener('scroll', update)
	})
</script>

{#if headings.length > 0}
	<nav aria-label="On this page">
		<p class="title">On this page</p>
		<ul>
			{#each headings as heading (heading.id)}
				<li>
					<a href="#{heading.id}" aria-current={heading.id === active ? 'location' : undefined}>
						{heading.text}
					</a>
				</li>
			{/each}
		</ul>
	</nav>
{/if}

<style>
	.title {
		margin: 0 0 14px;
		color: var(--ts-text);
		font: 500 12px var(--ts-mono);
		letter-spacing: 0.08em;
		text-transform: uppercase;
	}

	ul {
		margin: 0;
		padding: 0;
		list-style: none;
		border-left: 1px solid var(--ts-line);
	}

	a {
		display: block;
		margin-left: -1px;
		padding: 6px 14px;
		color: var(--ts-muted-2);
		border-left: 1px solid transparent;
		font-size: 14px;
		line-height: 1.4;
	}

	a:hover {
		color: var(--ts-text);
	}

	a[aria-current='location'] {
		color: var(--ts-accent-text);
		border-left-color: var(--ts-accent);
		font-weight: 500;
	}
</style>
