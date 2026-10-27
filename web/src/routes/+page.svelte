<script lang="ts">
	import { codeBlocks } from '$lib/code-blocks'
	import AtRest from '$lib/components/AtRest.svelte'
	import Logo from '$lib/components/Logo.svelte'
	import Seo from '$lib/components/Seo.svelte'
	import { href, rebase } from '$lib/href'
	import { repository } from '$lib/site'
	import { track } from '$lib/track'

	// every word of this page but its buttons comes from web/landing.md
	let { data } = $props()

	// the memory at rest, service by service, under the hero; the grid keeps the other cards
	const rest = $derived(data.cards.find(card => card.title === 'Memory at rest'))
	const cards = $derived(data.cards.filter(card => card !== rest))
</script>

<Seo title={data.title} description={data.summary} origin={data.origin} path="/" />

<main>
	<section class="hero">
		<h1>{data.headline}</h1>
		<p class="pitch">{@html rebase(data.pitch)}</p>
		<div class="actions">
			<a class="key primary" href={href(data.start)}>Start here</a>
			<a class="key" href={repository} onclick={() => track('outbound', 'github')}>View on GitHub</a>
		</div>

		<div class="sample" {@attach codeBlocks}>
			{@html data.sample}
		</div>
	</section>

	{#if rest !== undefined && data.numbers.shares.length > 0}
		<AtRest card={rest} legend={data.numbers.legend} shares={data.numbers.shares} />
	{/if}

	<section class="engines" aria-label="Engines">
		{#each data.engines as engine, index (engine.name)}
			<a class="engine" href={engine.external ? engine.href : href(engine.href)}>
				<span class="number">{String(index + 1).padStart(2, '0')}</span>
				<span class="name">{engine.name}</span>
				<span class="text"><span class="short">{engine.short}.</span> {engine.text}</span>
				<span class="arrow" aria-hidden="true">{engine.external ? '↗' : '→'}</span>
			</a>
		{/each}
	</section>

	{#if data.cards.length > 0}
	<section class="numbers">
		<p class="eyebrow">Numbers</p>
		<h2>{data.numbers.title}</h2>
		<div class="cards">
			{#each cards as card (card.title)}
				<div class="card">
					<p class="card-title">{card.title}</p>
					<p class="unit">{card.unit}</p>
					<!-- one grid for the card, so that every row's bars share one scale -->
					<div class="rows">
						{#each card.rows as row (row.label)}
							<span class="label" class:ours={row.ours}>{row.label}</span>
							<span class="track" class:ours={row.ours}>
								{#each row.bars as bar, at (at)}
									<span
										class="bar"
										class:faded={bar.faded}
										class:thin={row.bars.length > 1}
										style:width="{(bar.fraction * 100).toFixed(1)}%"
									></span>
								{/each}
							</span>
							<span class="value" class:ours={row.ours}>{row.bars.map(bar => bar.value).join(' / ')}</span>
						{/each}
					</div>
				</div>
			{/each}
		</div>
		<dl class="legend">
			{#each data.numbers.legend as row (row.term)}
				<div>
					<dt>{row.term}</dt>
					<dd>{row.text}</dd>
				</div>
			{/each}
		</dl>
		<p class="caveat">{@html rebase(data.numbers.caveat)}</p>
	</section>
	{/if}

	<!-- the close: each language's install command, a press to copy it, and the way in -->
	<section class="install" aria-label="Install" {@attach codeBlocks}>
		<ul class="commands">
			{#each data.installs as install (install.key)}
				<li>
					<span class="language">{install.label}</span>
					<button type="button" class="command" data-copy="" data-copy-text={install.command} title="Copy">
						<code><span class="prompt">$</span> {install.command}</code>
						<span class="copied">Copied</span>
					</button>
				</li>
			{/each}
		</ul>
		<a class="key start" href={href(data.start)}>Start here</a>
	</section>
</main>

<footer>
	<a class="mast" href={href('/')} aria-label="TinyStore, home">
		<Logo size={40} />
		<span>TinyStore</span>
	</a>
	<nav class="links" aria-label="Documentation">
		{#each data.books as book (book.href)}
			<a href={href(book.href)}>{book.title}</a>
		{/each}
		<a href={repository} onclick={() => track('outbound', 'github')}>GitHub</a>
	</nav>
	<p class="colophon">
		<span>Apache-2.0</span>
		<span>Bun, Python, Go and Rust</span>
	</p>
</footer>

<style>
	main,
	footer {
		width: min(1080px, calc(100% - 44px));
		margin: 0 auto;
	}

	.hero {
		padding: 112px 0 72px;
	}

	h1 {
		max-width: 860px;
		margin: 0;
		font-size: 76px;
		font-weight: 600;
		line-height: 1;
		letter-spacing: -0.05em;
	}

	.pitch {
		max-width: 720px;
		margin: 32px 0 0;
		color: var(--ts-text-lead);
		font-size: 22px;
		line-height: 1.5;
		text-wrap: pretty;
	}

	.actions {
		display: flex;
		gap: 12px;
		margin-top: 36px;
	}

	.sample {
		margin-top: 64px;
	}

	.sample :global(.code-hero) {
		margin: 0;
		border-radius: 14px;
	}

	.sample :global(.code-hero .code-bar) {
		min-height: 52px;
		padding: 0 18px 0 10px;
	}

	.sample :global(.code-hero .code-lang) {
		padding: 6px 13px;
		font-size: 13.5px;
	}

	.sample :global(.code-install) {
		display: flex;
		align-items: center;
		color: var(--ts-muted-2);
		background: none;
		border: 0;
		font: 13px var(--ts-mono);
		cursor: pointer;
	}

	.sample :global(.code-install:hover) {
		color: var(--ts-text);
	}

	.sample :global(.code-install .prompt),
	.sample :global(.code-install .done) {
		color: var(--ts-accent-text);
	}

	.sample :global(.code-install .done),
	.sample :global(.code-install[data-copied] .command) {
		display: none;
	}

	.sample :global(.code-install[data-copied] .done) {
		display: inline;
	}

	.sample :global(.code-body) {
		position: relative;
	}

	.sample :global(.code-body .code-copy) {
		position: absolute;
		top: 14px;
		right: 16px;
		border: 1px solid var(--ts-border-2);
	}

	.sample :global(.code-body pre) {
		padding: 22px 26px 26px;
		font-size: 15px;
	}

	.engines {
		margin-top: 0;
		border-top: 1px solid var(--ts-line);
	}

	.engine {
		display: grid;
		grid-template-columns: 70px 180px 1fr 40px;
		align-items: baseline;
		padding: 24px 8px;
		border-bottom: 1px solid var(--ts-line);
	}

	.engine:hover {
		background: var(--ts-row-hover);
	}

	.number {
		color: var(--ts-dim);
		font: 13px var(--ts-mono);
	}

	.name {
		font-size: 24px;
		font-weight: 600;
		letter-spacing: -0.025em;
	}

	.text {
		color: var(--ts-muted);
		font-size: 17px;
		line-height: 1.5;
	}

	.short {
		color: var(--ts-text);
	}

	.arrow {
		color: var(--ts-accent-text);
		font-size: 18px;
		text-align: right;
	}

	.numbers {
		margin-top: 104px;
	}

	.eyebrow {
		margin: 0 0 14px;
		color: var(--ts-faint);
		font: 500 13px var(--ts-mono);
	}

	h2 {
		margin: 0 0 32px;
		font-size: 30px;
		font-weight: 600;
		line-height: 1.2;
		letter-spacing: -0.035em;
	}

	.cards {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: 18px;
	}

	.card {
		padding: 20px 22px 16px;
		border: 1px solid var(--ts-border-2);
		border-radius: 12px;
	}

	/* an odd card out closes the grid across both columns rather than half of one row */
	.card:last-child:nth-child(odd) {
		grid-column: 1 / -1;
	}

	.card-title {
		margin: 0;
		font-size: 17px;
		font-weight: 600;
		letter-spacing: -0.02em;
	}

	.unit {
		margin: 3px 0 12px;
		color: var(--ts-faint);
		font-size: 12.5px;
		line-height: 1.45;
	}

	.rows {
		display: grid;
		grid-template-columns: 112px minmax(0, 1fr) max-content;
		align-items: center;
		gap: 14px 12px;
		padding: 7px 0 4px;
	}

	.label {
		color: var(--ts-muted);
		font-size: 14px;
	}

	.track {
		display: flex;
		flex-direction: column;
		gap: 4px;
		min-width: 0;
	}

	.bar {
		height: 14px;
		background: var(--ts-border-strong);
		border-radius: 3px;
	}

	.bar.thin {
		height: 8px;
	}

	.bar.faded {
		opacity: 0.4;
	}

	.value {
		color: var(--ts-muted-2);
		font: 13px var(--ts-mono);
		white-space: nowrap;
	}

	.label.ours {
		color: var(--ts-text);
		font-weight: 600;
	}

	.track.ours .bar {
		background: var(--ts-accent);
	}

	.value.ours {
		color: var(--ts-text);
		font-weight: 500;
	}

	.legend {
		display: grid;
		gap: 6px;
		margin: 24px 0 0;
		color: var(--ts-muted);
		font-size: 14px;
		line-height: 1.5;
	}

	.legend div {
		display: grid;
		grid-template-columns: 80px minmax(0, 1fr);
		gap: 12px;
	}

	.legend dt {
		color: var(--ts-text);
		font: 500 13px/1.6 var(--ts-mono);
	}

	.legend dd {
		margin: 0;
	}

	.caveat {
		max-width: 760px;
		margin: 16px 0 0;
		color: var(--ts-faint);
		font-size: 13.5px;
		line-height: 1.6;
	}

	/* the caveat's link comes from landing.md, outside the component's scope */
	.caveat :global(a) {
		color: var(--ts-accent-text);
	}

	/* the close: the install commands two by two, the way in beside them */
	.install {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		align-items: end;
		gap: 40px;
		margin-top: 104px;
		padding-top: 40px;
		border-top: 1px solid var(--ts-line);
	}

	.commands {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: 16px 20px;
		margin: 0;
		padding: 0;
		list-style: none;
	}

	.commands li {
		display: grid;
		gap: 8px;
		min-width: 0;
	}

	.language {
		color: var(--ts-faint);
		font-size: 13.5px;
	}

	.command {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 16px;
		min-height: 48px;
		padding: 0 18px;
		color: var(--ts-text);
		background: var(--ts-surface);
		border: 1px solid var(--ts-border-2);
		border-radius: 10px;
		text-align: left;
		cursor: pointer;
	}

	.command code {
		overflow: hidden;
		font-size: 14px;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.prompt,
	.copied {
		color: var(--ts-accent-text);
	}

	.copied {
		display: none;
		flex: none;
		font-size: 13.5px;
	}

	.command:global([data-copied]) .copied {
		display: inline;
	}

	@media (hover: hover) {
		.command:hover {
			border-color: var(--ts-border-strong);
		}
	}

	.command:active {
		background: var(--ts-hover);
	}

	/* the footer: the wordmark large over the docs' parts, the licence beside them */
	footer {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		gap: 28px 40px;
		margin-top: 96px;
		padding: 48px 0 40px;
		border-top: 1px solid var(--ts-line);
	}

	.mast {
		display: flex;
		grid-column: 1 / -1;
		align-items: center;
		gap: 14px;
		font-size: 44px;
		font-weight: 600;
		letter-spacing: -0.045em;
	}

	.links {
		display: flex;
		flex-wrap: wrap;
		gap: 8px 24px;
		font-size: 15px;
	}

	.links a {
		color: var(--ts-text-body);
	}

	.links a:hover {
		color: var(--ts-accent-text);
	}

	.colophon {
		display: flex;
		gap: 24px;
		margin: 0;
		color: var(--ts-faint);
		font-size: 13.5px;
	}

	@media (max-width: 720px) {
		.hero {
			padding: 48px 0 40px;
		}

		h1 {
			font-size: 44px;
		}

		.pitch {
			margin-top: 20px;
			font-size: 18px;
		}

		.sample {
			margin-top: 40px;
		}

		.sample :global(.code-hero .code-bar) {
			flex-wrap: nowrap;
			padding: 0 12px 0 6px;
		}

		.sample :global(.code-install) {
			display: none;
		}

		.sample :global(.code-hero .code-tools),
		.sample :global(.code-hero .code-langs) {
			width: auto;
		}

		/* the long lines run under a copy button in the code, so it moves up into the bar,
		   where the install command it shares with leaves room on a phone */
		.sample :global(.code-hero .code-variant) {
			position: relative;
		}

		.sample :global(.code-body) {
			position: static;
		}

		.sample :global(.code-body .code-copy) {
			top: 11px;
			right: 12px;
		}

		.sample :global(.code-body pre) {
			padding: 16px 18px 18px;
			font-size: 12.5px;
		}

		.engine {
			grid-template-columns: 1fr auto;
			gap: 6px 12px;
			padding: 20px 4px;
		}

		.number {
			display: none;
		}

		.text {
			grid-column: 1 / -1;
			grid-row: 2;
			font-size: 15.5px;
		}

		.arrow {
			grid-column: 2;
			grid-row: 1;
		}

		.numbers {
			margin-top: 72px;
		}

		.cards {
			grid-template-columns: 1fr;
		}

		.card {
			padding: 18px 16px 14px;
		}

		.rows {
			grid-template-columns: 88px minmax(0, 1fr) max-content;
			gap: 12px 10px;
		}

		.label {
			font-size: 13px;
		}

		.value {
			font-size: 12px;
		}

		.install,
		.commands,
		footer {
			grid-template-columns: minmax(0, 1fr);
		}

		.install {
			align-items: start;
			margin-top: 72px;
		}

		footer {
			margin-top: 64px;
		}

		.mast {
			font-size: 32px;
		}

		.mast :global(svg) {
			width: 32px;
			height: 32px;
		}

		.colophon {
			flex-direction: column;
			gap: 4px;
		}
	}
</style>
