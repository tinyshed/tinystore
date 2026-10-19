<script lang="ts">
	import type { Scene } from './parse'
	import { Stage } from './stage'

	/**
	 * A scene played on the page: its code above, the stage, and a caption a
	 * step at a time. It starts when it comes into view and pauses when it
	 * leaves; a tap on the stage pauses or goes on, and a swipe turns the step.
	 */
	let { scene, codeHtml = '' }: { scene: Scene; codeHtml?: string } = $props()

	let root: HTMLElement
	let svg: SVGSVGElement
	let code: HTMLElement | undefined = $state()
	let at = $state(0)
	let playing = $state(false)
	let finished = $state(false)
	let progress = $state(0)
	let stage: Stage | undefined
	let token = 0

	const steps = $derived(scene.steps.length)
	const caption = $derived(scene.steps[at]?.caption ?? '')
	const reduced = () => matchMedia('(prefers-reduced-motion: reduce)').matches

	// a caption stays long enough to be read before the next step
	const hold = (step: number) => Math.max(1600, (scene.steps[step]?.caption.length ?? 0) * 40)

	async function run(from: number) {
		const mine = ++token
		playing = true
		finished = false
		for (let step = from; step < steps; step++) {
			at = step
			const played = await stage?.play(step, done => (progress = done))
			if (!played || mine !== token) return
			if (!(await stage?.wait(hold(step))) || mine !== token) return
		}
		playing = false
		finished = true
	}

	function pause() {
		token++
		playing = false
		stage?.show(at + 1)
		progress = 1
	}

	function go(step: number) {
		token++
		stage?.stop()
		run(Math.max(0, Math.min(steps - 1, step)))
	}

	function toggle() {
		if (playing) pause()
		else if (finished) go(0)
		else go(at)
	}

	// the code's lines this step runs, marked in the highlighted block
	$effect(() => {
		const [from, to] = scene.steps[at]?.lines ?? [0, -1]
		code?.querySelectorAll('.line').forEach((line, index) => {
			line.classList.toggle('run', index + 1 >= from && index + 1 <= to)
		})
	})

	$effect(() => {
		stage = new Stage(svg, scene, reduced)
		let tall: boolean | undefined
		const resize = new ResizeObserver(() => {
			const narrow = root.clientWidth < 560
			if (narrow === tall) return
			tall = narrow
			stage?.fit(root.clientWidth, at)
			if (playing) go(at)
		})
		resize.observe(root)

		let seen = false
		const view = new IntersectionObserver(
			([entry]) => {
				if (entry?.isIntersecting && !seen) {
					seen = true
					go(0)
				} else if (!entry?.isIntersecting && playing) {
					pause()
				}
			},
			{ threshold: 0.45 },
		)
		view.observe(root)
		return () => {
			token++
			stage?.stop()
			resize.disconnect()
			view.disconnect()
		}
	})

	// a swipe turns the step, a tap pauses or goes on
	let down: { x: number; y: number } | undefined
	function pointerdown(event: PointerEvent) {
		down = { x: event.clientX, y: event.clientY }
	}
	function pointerup(event: PointerEvent) {
		if (!down) return
		const dx = event.clientX - down.x
		const dy = event.clientY - down.y
		down = undefined
		if (Math.abs(dx) > 40 && Math.abs(dx) > Math.abs(dy)) go(at + (dx < 0 ? 1 : -1))
		else if (Math.abs(dx) < 8 && Math.abs(dy) < 8) toggle()
	}

	function keydown(event: KeyboardEvent) {
		if (event.key === 'ArrowRight') go(at + 1)
		else if (event.key === 'ArrowLeft') go(at - 1)
		else if (event.key === ' ') {
			event.preventDefault()
			toggle()
		}
	}
</script>

<div class="scene-player" bind:this={root}>
	{#if codeHtml}
		<div class="scene-code" bind:this={code}>{@html codeHtml}</div>
	{/if}
	<div
		class="stage"
		role="button"
		aria-roledescription="animation"
		aria-label="Step {at + 1} of {steps}: {caption}. Press to {playing ? 'pause' : 'play'}, arrows to turn the step."
		tabindex="0"
		onpointerdown={pointerdown}
		onpointerup={pointerup}
		onkeydown={keydown}
	>
		<span class="bar" style:width="{((at + progress) / steps) * 100}%"></span>
		<svg bind:this={svg} aria-hidden="true"></svg>
	</div>
	<div class="foot">
		<button class="play" onclick={toggle} aria-label={playing ? 'Pause' : finished ? 'Play again' : 'Play'}>
			{#if playing}
				<svg viewBox="0 0 16 16" aria-hidden="true"><path d="M5 3v10M11 3v10" /></svg>
			{:else if finished}
				<svg viewBox="0 0 16 16" aria-hidden="true"><path d="M13 8a5 5 0 1 1-1.5-3.6M13 2.5v3h-3" /></svg>
			{:else}
				<svg viewBox="0 0 16 16" aria-hidden="true"><path class="fill" d="M5 3l8 5-8 5z" /></svg>
			{/if}
		</button>
		<div class="caption" aria-live="polite">
			<span class="count">{at + 1} / {steps}</span>
			{#key at}<p>{caption}</p>{/key}
		</div>
		<div class="dots">
			{#each scene.steps as step, index (index)}
				<button
					class:done={index < at}
					aria-current={index === at ? 'step' : undefined}
					aria-label="Step {index + 1}: {step.caption}"
					onclick={() => go(index)}
				></button>
			{/each}
		</div>
	</div>
</div>

<style>
	.scene-player {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: 10px;
		margin: 26px 0 30px;
	}

	.scene-code {
		min-width: 0;
		overflow-x: auto;
		border-radius: 12px;
		background: var(--ts-surface);
		border: 1px solid var(--ts-border-2);
	}

	.scene-code :global(pre) {
		margin: 0;
		padding: 12px 0;
		font: 13px/1.75 var(--ts-mono);
	}

	.scene-code :global(.line) {
		display: inline-block;
		min-width: 100%;
		padding: 0 18px;
		border-left: 2px solid transparent;
		transition:
			background 0.3s,
			border-color 0.3s;
	}

	.scene-code :global(.line.run) {
		background: var(--ts-accent-soft);
		border-left-color: var(--ts-accent);
	}

	.stage {
		position: relative;
		border-top: 1px solid var(--ts-line);
		border-bottom: 1px solid var(--ts-line);
		touch-action: pan-y;
		user-select: none;
		cursor: pointer;
	}

	.stage:focus-visible {
		outline: 2px solid var(--ts-accent);
		outline-offset: 2px;
	}

	.bar {
		position: absolute;
		top: -1px;
		left: 0;
		height: 1px;
		background: var(--ts-accent);
		transition: width 0.4s ease;
	}

	.stage svg {
		display: block;
		width: 100%;
		height: auto;
		overflow: hidden;
	}

	.stage :global(text) {
		font-family: var(--ts-sans);
		paint-order: stroke;
		stroke: var(--ts-bg);
		stroke-width: 4px;
		stroke-linejoin: round;
	}

	.stage :global(.cable line) {
		stroke: var(--ts-border-strong);
		stroke-width: 1;
	}

	.stage :global(.cable.cut line) {
		stroke: var(--ts-danger);
		stroke-dasharray: 3 5;
	}

	.stage :global(.scissors circle) {
		fill: var(--ts-bg);
		stroke: var(--ts-danger);
	}

	.stage :global(.scissors text) {
		fill: var(--ts-danger);
		font-size: 13px;
		stroke: none;
	}

	.stage :global(.tile circle) {
		fill: var(--ts-surface-2);
	}

	.stage :global(.actor.down .tile circle) {
		fill: var(--ts-danger-soft);
	}

	.stage :global(.icon) {
		fill: none;
		stroke: var(--ts-text);
		stroke-width: 1.5;
		stroke-linecap: round;
		stroke-linejoin: round;
	}

	.stage :global(.actor.down .icon) {
		stroke: var(--ts-danger);
	}

	.stage :global(.halo) {
		fill: none;
		stroke: var(--ts-accent);
		stroke-dasharray: 2 4;
	}

	.stage :global(.label) {
		font-size: 14px;
		font-weight: 550;
		fill: var(--ts-text);
	}

	.stage :global(.line) {
		font: 11.5px var(--ts-mono);
		fill: var(--ts-muted);
	}

	.stage :global(.line.fresh) {
		fill: var(--ts-text);
	}

	.stage :global(.line.struck) {
		fill: var(--ts-danger);
		text-decoration: line-through;
	}

	.stage :global(.badge) {
		font: 500 10.5px var(--ts-mono);
		letter-spacing: 0.12em;
		fill: var(--ts-muted);
	}

	.stage :global(.badge.home) {
		fill: var(--ts-accent-text);
	}

	.stage :global(.badge.down) {
		fill: var(--ts-danger);
	}

	.stage :global(.trail) {
		fill: none;
		stroke: var(--ts-accent);
		opacity: 0.6;
	}

	.stage :global(.packet rect) {
		fill: var(--ts-accent);
	}

	.stage :global(.packet text) {
		font: 500 11.5px var(--ts-mono);
		fill: var(--ts-accent-ink);
		stroke: none;
	}

	.stage :global(.send.ok .trail) {
		stroke: var(--ts-ok);
	}

	.stage :global(.send.ok .packet rect),
	.stage :global(.send.lost .packet rect) {
		fill: var(--ts-ok);
	}

	.stage :global(.send.lost .packet rect) {
		fill: var(--ts-danger);
	}

	.stage :global(.send.ok .packet text),
	.stage :global(.send.lost .packet text) {
		fill: var(--ts-bg);
	}

	.stage :global(.send.ghost .trail) {
		stroke: var(--ts-border-strong);
	}

	.stage :global(.send.ghost .packet rect) {
		fill: var(--ts-surface-2);
		stroke: var(--ts-border-strong);
	}

	.stage :global(.send.ghost .packet text) {
		fill: var(--ts-muted);
	}

	.stage :global(.ring) {
		fill: none;
		stroke: var(--ts-accent);
	}

	.stage :global(.say text) {
		font: 500 12.5px var(--ts-mono);
		fill: var(--ts-text);
	}

	.stage :global(.say.ok text) {
		fill: var(--ts-ok);
	}

	.stage :global(.say.bad text) {
		fill: var(--ts-danger);
	}

	.foot {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr) auto;
		gap: 14px;
		align-items: start;
	}

	.play {
		display: grid;
		place-items: center;
		width: 40px;
		height: 40px;
		border: 1px solid var(--ts-border-strong);
		border-radius: 50%;
		background: none;
		color: var(--ts-text);
		cursor: pointer;
	}

	.play:hover {
		border-color: var(--ts-text);
	}

	.play svg {
		width: 14px;
		height: 14px;
		fill: none;
		stroke: currentColor;
		stroke-width: 1.8;
		stroke-linecap: round;
		stroke-linejoin: round;
	}

	.play .fill {
		fill: currentColor;
	}

	.caption {
		display: grid;
		gap: 2px;
		min-height: 52px;
	}

	.count {
		font: 500 11px var(--ts-mono);
		color: var(--ts-muted);
	}

	.caption p {
		margin: 0;
		font-size: 15.5px;
		line-height: 1.5;
		color: var(--ts-text);
		animation: caption 0.45s ease both;
	}

	@keyframes caption {
		from {
			opacity: 0;
			transform: translateY(3px);
		}
	}

	.dots {
		display: flex;
		padding-top: 4px;
	}

	/* each dot is a 32px target around a 6px mark */
	.dots button {
		position: relative;
		width: 16px;
		height: 32px;
		padding: 0;
		border: 0;
		background: none;
		cursor: pointer;
	}

	.dots button::after {
		content: '';
		position: absolute;
		inset: 13px 5px;
		border-radius: 50%;
		background: var(--ts-border-strong);
	}

	.dots button.done::after {
		background: var(--ts-muted);
	}

	.dots button[aria-current='step']::after {
		background: var(--ts-accent);
	}

	@media (max-width: 560px) {
		.foot {
			grid-template-columns: auto minmax(0, 1fr);
		}

		.dots {
			grid-column: 1 / -1;
			padding-top: 0;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.caption p,
		.bar,
		.scene-code :global(.line) {
			animation: none;
			transition: none;
		}
	}
</style>
