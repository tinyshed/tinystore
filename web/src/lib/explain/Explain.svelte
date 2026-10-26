<script lang="ts">
	import cli from '../content/cli.json'
	import prints from '../content/cli-prints.json'
	import { explain, type Help, type Role } from './explain'

	/**
	 * A command a reader types and sees read word by word, as the command line
	 * reads it, with what it does and what a call like it prints: the help's
	 * own words, and what its example printed on a demo store.
	 */
	let { command = '' }: { command?: string } = $props()

	let line = $state(command)
	const read = $derived(explain(line, cli as Help, prints))

	const labels: Record<Role, string> = {
		program: 'program',
		command: 'command',
		verb: 'verb',
		word: 'argument',
		flag: 'flag',
		value: 'value',
		unknown: 'not read',
	}
</script>

<div class="explain">
	<label class="field">
		<span class="prompt">$</span>
		<input bind:value={line} spellcheck="false" autocomplete="off" aria-label="a tinystore command" />
	</label>

	<ol class="words">
		{#each read.words as word, index (index)}
			<li class={word.role}>
				<code>{word.text}</code>
				<span class="role">{word.stands ?? labels[word.role]}</span>
				<span class="means">{word.means}</span>
			</li>
		{/each}
	</ol>

	{#if read.problem}
		<p class="problem">{read.problem}</p>
	{/if}

	{#if read.command}
		<p class="does">
			<strong>tinystore {read.command.name}</strong> — {read.command.does}. {read.command.more}
		</p>
	{/if}

	{#if read.prints}
		<div class="prints">
			<span class="caption">prints, roughly</span>
			<pre>{read.prints}</pre>
		</div>
	{/if}
</div>

<style>
	.explain {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: 12px;
		margin: 22px 0 28px;
		padding: 14px;
		border: 1px solid var(--ts-border-2);
		border-radius: 12px;
		background: var(--ts-surface);
	}

	.field {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 8px 12px;
		border: 1px solid var(--ts-border);
		border-radius: 8px;
		background: var(--ts-bg);
		font: 14px/1.5 var(--ts-mono);
	}

	.prompt {
		color: var(--ts-faint);
	}

	input {
		flex: 1;
		min-width: 0;
		border: 0;
		outline: 0;
		background: transparent;
		color: var(--ts-text);
		font: inherit;
	}

	/* one grid for every word, so that the columns line up */
	.words {
		display: grid;
		grid-template-columns: minmax(0, max-content) max-content minmax(0, 1fr);
		gap: 6px 14px;
		align-items: baseline;
		margin: 0;
		padding: 0;
		list-style: none;
		font-size: 14px;
	}

	.words li {
		display: contents;
	}

	.words code {
		justify-self: start;
		padding: 1px 6px;
		border-radius: 5px;
		background: var(--ts-chip);
		font: 13px/1.6 var(--ts-mono);
		overflow-wrap: anywhere;
	}

	.role {
		color: var(--ts-muted);
		font: 12px/1.6 var(--ts-mono);
	}

	.means {
		color: var(--ts-text-body);
	}

	.command code,
	.verb code {
		color: var(--ts-accent-text);
	}

	.flag code,
	.value code {
		color: var(--ts-text-lead);
	}

	.unknown code,
	.problem {
		color: var(--ts-danger);
	}

	.problem,
	.does {
		margin: 0;
		font-size: 14px;
	}

	.does {
		color: var(--ts-text-body);
	}

	.prints {
		display: grid;
		gap: 4px;
	}

	.caption {
		color: var(--ts-muted);
		font-size: 12px;
	}

	.prints pre {
		margin: 0;
		padding: 10px 12px;
		overflow-x: auto;
		border-radius: 8px;
		background: var(--ts-bg);
		border: 1px solid var(--ts-border);
		font: 13px/1.6 var(--ts-mono);
	}

	@media (max-width: 640px) {
		.words {
			grid-template-columns: minmax(0, max-content) minmax(0, 1fr);
		}

		.means {
			grid-column: 1 / -1;
			margin-bottom: 4px;
		}
	}
</style>
