<script lang="ts">
	import type { Card } from '$lib/content/landing'
	import { href, rebase } from '$lib/href'

	/**
	 * The memory each stack holds at rest, on one scale: TinyStore's one block
	 * beside the five services stacked, each in its own brand's colour, so that
	 * every one of them shows its share. The bars fill once, the first time the
	 * chart is on screen; without a script, and for a reader who asks for less
	 * motion, they are simply full.
	 */
	let {
		card,
		legend,
		shares,
		note,
	}: {
		card: Card
		legend: { term: string; text: string }[]
		shares: { name: string; value: string }[]
		/** which round measured it, as HTML from landing.md */
		note: string
	} = $props()

	const rows = $derived(
		card.rows.map(row => ({
			label: row.label,
			ours: row.ours,
			value: row.bars[0]?.value ?? '',
			about: legend.find(entry => entry.term === row.label)?.text,
		})),
	)
	const ours = $derived(rows.find(row => row.ours))
	const theirs = $derived(rows.find(row => !row.ours))
	const total = $derived(shares.reduce((sum, share) => sum + mebibytes(share.value), 0))
	// the scale a little past the largest stack, a hairline every hundred MiB inside it
	const span = $derived(total * 1.04)
	const ticks = $derived(Array.from({ length: Math.floor(span / 100) + 1 }, (_, at) => at * 100))
	const segments = $derived(
		shares.reduce<{ name: string; value: string; brand: string; at: number; size: number }[]>(
			(placed, share) => {
				const last = placed.at(-1)
				const at = last === undefined ? 0 : last.at + last.size
				placed.push({ ...share, brand: share.name.toLowerCase(), at, size: mebibytes(share.value) })
				return placed
			},
			[],
		),
	)

	let waiting = $state(false)
	let chart: HTMLElement | undefined = $state()

	function mebibytes(value: string): number {
		return Number(/[\d.]+/.exec(value)?.[0] ?? 0)
	}

	// the bars wait empty until the chart is half on screen, then fill one after another
	$effect(() => {
		if (chart === undefined || matchMedia('(prefers-reduced-motion: reduce)').matches) {
			return
		}
		waiting = true
		const seen = new IntersectionObserver(
			([entry]) => {
				if (entry?.isIntersecting) {
					seen.disconnect()
					waiting = false
				}
			},
			{ threshold: 0.5 },
		)
		seen.observe(chart)
		return () => seen.disconnect()
	})
</script>

<section class="at-rest" aria-label={card.title}>
	<div>
		<div class="head">
			<div>
				<h2>{card.title}</h2>
				<p class="unit">{card.unit}</p>
			</div>
			<dl class="results">
				{#each rows as row (row.label)}
					<div class:ours={row.ours}>
						<dt>{row.label}</dt>
						<dd>{row.value}<span class="mib">MiB</span></dd>
					</div>
				{/each}
			</dl>
		</div>

		<div class="chart" class:waiting bind:this={chart}>
			<div class="ticks" aria-hidden="true">
				{#each ticks as tick (tick)}
					<span class="tick" style:--at={tick / span}><span class="mark">{tick}</span></span>
				{/each}
			</div>

			{#if ours !== undefined}
				<div class="row">
					<div class="who">
						<span class="label">{ours.label}</span>
						{#if ours.about}<span class="about">{ours.about}</span>{/if}
					</div>
					<div class="track" aria-hidden="true">
						<span
							class="block ours"
							style:--at="0"
							style:--size={mebibytes(ours.value) / span}
							style:--i="0"
						></span>
						<span class="beside" style:--at={mebibytes(ours.value) / span}>{ours.value} MiB</span>
					</div>
				</div>
			{/if}

			{#if theirs !== undefined}
				<div class="row">
					<div class="who">
						<span class="label">{theirs.label}</span>
						{#if theirs.about}<span class="about">{theirs.about}</span>{/if}
					</div>
					<div class="stack">
						<div class="track" aria-hidden="true">
							{#each segments as segment, at (segment.name)}
								<span
									class="block"
									style:--at={segment.at / span}
									style:--size={segment.size / span}
									style:--i={at + 1}
									style:--brand="var(--ts-brand-{segment.brand})"
								></span>
							{/each}
						</div>
						<ul class="shares">
							{#each segments as segment (segment.name)}
								<li style:--brand="var(--ts-brand-{segment.brand})">
									<span class="swatch"></span>
									<span>{segment.name}</span>
									<span class="value">{segment.value}</span>
								</li>
							{/each}
						</ul>
					</div>
				</div>
			{/if}
		</div>

		<div class="foot">
			<p class="note">
				{@html rebase(note)}
				<a href={href('/docs/handbook/production/benchmarks#at-rest')}>Every figure at rest</a>
			</p>
		</div>
	</div>
</section>

<style>
	.at-rest {
		padding: 56px 0 96px;
	}

	/* the measurement named on the left, the two totals on the right */
	.head {
		display: grid;
		grid-template-columns: minmax(0, 5fr) minmax(0, 7fr);
		align-items: end;
		gap: 48px;
		margin-bottom: 48px;
	}

	h2 {
		min-width: 0;
		margin: 0;
		font-size: 40px;
		font-weight: 600;
		line-height: 1.05;
		letter-spacing: -0.04em;
		overflow-wrap: break-word;
		text-wrap: balance;
	}

	.unit {
		margin: 12px 0 0;
		color: var(--ts-faint);
		font-size: 14px;
	}

	.results {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		margin: 0;
	}

	.results div {
		padding: 0 24px;
		border-left: 1px solid var(--ts-border-2);
	}

	.results dt {
		color: var(--ts-muted);
		font-size: 14px;
	}

	.results dd {
		margin: 4px 0 0;
		color: var(--ts-text);
		font-size: 64px;
		font-weight: 600;
		font-variant-numeric: tabular-nums;
		line-height: 1;
		letter-spacing: -0.045em;
		white-space: nowrap;
	}

	.results .ours dd {
		color: var(--ts-accent-text);
	}

	.mib {
		margin-left: 0.15em;
		font-size: 0.35em;
		letter-spacing: 0;
	}

	/* the chart: a hairline every hundred MiB under two rows on one scale */
	.chart {
		position: relative;
		display: grid;
		gap: 36px;
		padding-top: 36px;
	}

	.ticks {
		position: absolute;
		inset: 0 0 0 calc(25% + 24px);
		pointer-events: none;
	}

	.tick {
		position: absolute;
		top: 0;
		bottom: 0;
		left: calc(var(--at) * 100%);
		border-left: 1px solid var(--ts-line);
	}

	.mark {
		position: absolute;
		top: 0;
		left: 4px;
		color: var(--ts-faint);
		font: 12px var(--ts-mono);
		white-space: nowrap;
	}

	.row {
		position: relative;
		display: grid;
		grid-template-columns: 25% minmax(0, 1fr);
		align-items: start;
		gap: 24px;
	}

	.who {
		display: grid;
		gap: 2px;
	}

	.label {
		color: var(--ts-text);
		font-size: 20px;
		font-weight: 600;
		letter-spacing: -0.02em;
	}

	.about {
		color: var(--ts-muted);
		font-size: 14px;
		line-height: 1.45;
	}

	.track {
		position: relative;
		height: 40px;
	}

	/* a service's colour, quietened into the page, so that TinyStore's amber is the one that speaks */
	.block,
	.swatch {
		--tint: color-mix(in oklab, var(--brand) 36%, var(--ts-bg));
	}

	/* a block is its MiB's share of the scale, filled by transform so that it never reflows */
	.block {
		position: absolute;
		top: 0;
		bottom: 0;
		left: calc(var(--at) * 100%);
		width: calc(var(--size) * 100%);
		background: var(--tint);
		box-shadow: inset -2px 0 0 var(--ts-bg);
		transform-origin: left;
		transition: transform 0.42s cubic-bezier(0.16, 1, 0.3, 1);
		transition-delay: calc(var(--i) * 120ms);
	}

	.block.ours {
		background: var(--ts-accent);
		border-radius: 3px;
		box-shadow: none;
	}

	.waiting .block {
		transform: scaleX(0);
		transition: none;
	}

	@media (prefers-reduced-motion: reduce) {
		.block {
			transition: none;
		}
	}

	.beside {
		position: absolute;
		top: 50%;
		left: calc(var(--at) * 100%);
		margin-left: 12px;
		color: var(--ts-accent-text);
		font-size: 18px;
		font-weight: 600;
		font-variant-numeric: tabular-nums;
		white-space: nowrap;
		transform: translateY(-50%);
	}

	.stack {
		display: grid;
		gap: 14px;
		min-width: 0;
	}

	/* the five, in the stack's order: a swatch, the name and its share */
	.shares {
		display: flex;
		flex-wrap: wrap;
		gap: 8px 22px;
		margin: 0;
		padding: 0;
		list-style: none;
		font-size: 14px;
	}

	.shares li {
		display: flex;
		align-items: center;
		gap: 8px;
		color: var(--ts-text);
	}

	.swatch {
		flex: none;
		width: 11px;
		height: 11px;
		background: var(--tint);
		border-radius: 3px;
	}

	.shares .value {
		color: var(--ts-muted);
		font-variant-numeric: tabular-nums;
	}

	.foot {
		margin-top: 36px;
		padding-top: 16px;
		border-top: 1px solid var(--ts-line);
	}

	.note {
		max-width: 720px;
		margin: 0;
		color: var(--ts-faint);
		font-size: 13.5px;
		line-height: 1.6;
	}

	/* the round's link comes from landing.md, outside the component's scope */
	.note :global(a) {
		color: var(--ts-accent-text);
	}

	.note :global(a:hover) {
		text-decoration: underline;
	}

	@media (max-width: 720px) {
		.at-rest {
			padding: 24px 0 56px;
		}

		.head {
			grid-template-columns: minmax(0, 1fr);
			gap: 20px;
			margin-bottom: 28px;
		}

		h2 {
			font-size: 30px;
		}

		.results div:first-child {
			padding-left: 0;
			border-left: 0;
		}

		.results dd {
			font-size: 40px;
		}

		/* a phone has no room for the hairlines: the blocks and the list under them carry the scale */
		.ticks,
		.about {
			display: none;
		}

		.chart {
			gap: 24px;
			padding-top: 0;
		}

		.row {
			grid-template-columns: minmax(0, 1fr);
			gap: 8px;
		}

		.label {
			font-size: 18px;
		}

		.track {
			height: 32px;
		}

		.beside {
			font-size: 15px;
		}

		/* the five listed under the stack, a ruled row each */
		.shares {
			display: grid;
			gap: 0;
			border-top: 1px solid var(--ts-line);
		}

		.shares li {
			display: grid;
			grid-template-columns: 11px minmax(0, 1fr) auto;
			gap: 12px;
			padding: 8px 0;
			border-bottom: 1px solid var(--ts-line);
		}

		.shares .value {
			text-align: right;
		}
	}
</style>
