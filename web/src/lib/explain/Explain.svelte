<script lang="ts">
	import cli from '../content/cli.json'
	import prints from '../content/cli-prints.json'
	import { explain, type Help, type Role, type Word } from './explain'

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

	/** What each word means, a flag and its value as one, the program's own left out. */
	const meanings = $derived.by(() => {
		const said: { text: string; role: Role; means: string }[] = []
		read.words.forEach((word: Word, index: number) => {
			if (word.role === 'program' || word.role === 'value') return
			const next = read.words[index + 1]
			const valued = word.role === 'flag' && next?.role === 'value' && next.stands === word.stands
			said.push({
				text: valued ? `${word.text} ${next.text}` : word.text,
				role: word.role,
				means: word.means,
			})
		})
		return said
	})
</script>

<div class="explain">
	<label class="field">
		<span class="prompt">$</span>
		<input bind:value={line} spellcheck="false" autocomplete="off" aria-label="a tinystore command" />
	</label>

	<div class="line" aria-hidden="true">
		{#each read.words as word, index (index)}
			<span class="token {word.role}">
				<code>{word.text}</code>
				<small>{word.stands ?? labels[word.role]}</small>
			</span>
		{/each}
	</div>

	<dl class="meanings">
		{#each meanings as meaning, index (index)}
			<div class={meaning.role}>
				<dt><code>{meaning.text}</code></dt>
				<dd>{meaning.means}</dd>
			</div>
		{/each}
	</dl>

	{#if read.problem}
		<p class="problem">{read.problem}</p>
	{/if}

	{#if read.command}
		<p class="does"><strong>tinystore {read.command.name}</strong> — {read.command.does}.</p>
		{#if read.command.more}
			<p class="more">{read.command.more}</p>
		{/if}
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
		gap: 14px;
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

	/* the command once, each word over what it is */
	.line {
		display: flex;
		flex-wrap: wrap;
		gap: 8px 6px;
	}

	.token {
		display: inline-grid;
		justify-items: start;
		gap: 2px;
		min-width: 0;
	}

	.token code {
		max-width: 100%;
		padding: 1px 6px;
		border-radius: 5px;
		background: var(--ts-chip);
		font: 13px/1.6 var(--ts-mono);
		overflow-wrap: anywhere;
	}

	.token small {
		padding-left: 6px;
		color: var(--ts-faint);
		font: 11px/1.3 var(--ts-mono);
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

	/* what each word means: the word, then its meaning beside it, or under it on a phone */
	.meanings {
		display: grid;
		grid-template-columns: minmax(0, max-content) minmax(0, 1fr);
		gap: 6px 14px;
		margin: 0;
		font-size: 14px;
	}

	.meanings div {
		display: contents;
	}

	.meanings dt code {
		padding: 0;
		border: 0;
		background: none;
		font: 600 13px/1.6 var(--ts-mono);
		overflow-wrap: anywhere;
	}

	.meanings dd {
		margin: 0;
		color: var(--ts-text-body);
	}

	.problem,
	.does {
		margin: 0;
		font-size: 14px;
	}

	.does {
		color: var(--ts-text-body);
	}

	.more {
		margin: -8px 0 0;
		color: var(--ts-muted);
		font-size: 13px;
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
		font: 12.5px/1.6 var(--ts-mono);
	}

	@media (max-width: 640px) {
		.explain {
			gap: 12px;
			padding: 12px;
		}

		.field {
			padding: 7px 10px;
			font-size: 13px;
		}

		/* a word and its meaning on one line, as a sentence wraps */
		.meanings {
			display: grid;
			grid-template-columns: minmax(0, 1fr);
			gap: 6px;
			font-size: 13.5px;
			line-height: 1.5;
		}

		.meanings div {
			display: block;
		}

		.meanings dt,
		.meanings dd {
			display: inline;
		}

		.meanings dt {
			margin-right: 8px;
		}

		.does {
			font-size: 13.5px;
		}
	}
</style>
