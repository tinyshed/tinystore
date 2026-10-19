import { icons } from './icons'
import type { Action, Scene } from './parse'

/**
 * What a scene draws and how it moves, on one SVG. The stage paints the
 * picture as it stands before a step, then plays the step's actions over it.
 *
 * A narrow stage turns the scene on its side: actors placed left to right
 * stand top to bottom, so that on a phone the labels keep their size instead
 * of shrinking with a wide picture.
 */

const svgNS = 'http://www.w3.org/2000/svg'
const ease = (t: number) => (t < 0.5 ? 4 * t * t * t : 1 - (-2 * t + 2) ** 3 / 2)
const out = (t: number) => 1 - (1 - t) ** 3
// the space a row of a tall stage takes: a tile, its label and three lines of state
const rowGap = 130

interface Line {
	text: string
	struck?: boolean
	fresh?: boolean
}

interface Picture {
	actors: Map<string, { lines: Line[]; mark?: string }>
	links: Map<string, { a: string; b: string; cut: boolean }>
}

const key = (a: string, b: string) => [a, b].sort().join('|')

/** What persists of an action: a line, a mark, a cable. */
function apply(picture: Picture, action: Action): void {
	const actor = picture.actors.get(action.a)
	switch (action.verb) {
		case 'set':
			if (actor) actor.lines = [{ text: action.b, fresh: true }]
			break
		case 'add':
			actor?.lines.push({ text: action.b, fresh: true })
			break
		case 'strike':
			for (const line of actor?.lines ?? []) if (line.text === action.b) line.struck = true
			break
		case 'mark':
			if (actor) actor.mark = action.b
			break
		case 'link':
			picture.links.set(key(action.a, action.b), { a: action.a, b: action.b, cut: false })
			break
		case 'unlink':
			picture.links.delete(key(action.a, action.b))
			break
		case 'cut': {
			const link = picture.links.get(key(action.a, action.b))
			if (link) link.cut = true
			break
		}
	}
}

export class Stage {
	tall = false
	#run = 0
	#width = 1000
	#height = 360
	#picture!: Picture
	#fx!: SVGGElement

	constructor(
		readonly svg: SVGSVGElement,
		readonly scene: Scene,
		readonly reduced: () => boolean,
	) {}

	/** Fits the stage to its width, wide or tall, and paints the picture before a step. */
	fit(width: number, step: number): void {
		this.tall = width < 560
		this.#width = this.tall ? 440 : 1000
		this.#height = this.tall ? 70 + (this.#rows().length - 1) * rowGap + 110 : 360
		this.svg.setAttribute('viewBox', `0 0 ${this.#width} ${this.#height}`)
		this.show(step)
	}

	/** The picture as it stands before `step`, everything before it applied. */
	show(step: number): void {
		this.#run++
		const picture: Picture = { actors: new Map(), links: new Map() }
		for (const actor of this.scene.actors) picture.actors.set(actor.id, { lines: [] })
		const before = [...this.scene.setup, ...this.scene.steps.slice(0, step).flatMap(s => s.actions)]
		for (const action of before) apply(picture, action)
		for (const actor of picture.actors.values()) for (const line of actor.lines) line.fresh = false
		this.#paint(picture)
	}

	stop(): void {
		this.#run++
	}

	/** Plays a step's actions, `&` ones together; false when it was stopped. */
	async play(step: number, progress: (done: number) => void): Promise<boolean> {
		const run = ++this.#run
		this.show(step)
		this.#run = run
		const groups: Action[][] = []
		for (const action of this.scene.steps[step]?.actions ?? []) {
			const last = groups.at(-1)
			if (action.together && last) last.push(action)
			else groups.push([action])
		}
		for (const [index, group] of groups.entries()) {
			progress(index / groups.length)
			await Promise.all(group.map(action => this.#act(action, run)))
			if (run !== this.#run) return false
		}
		progress(1)
		return true
	}

	/** Waits, unless the stage is stopped meanwhile. */
	wait(ms: number): Promise<boolean> {
		const run = this.#run
		return new Promise(resolve =>
			setTimeout(() => resolve(run === this.#run), this.reduced() ? Math.min(ms, 400) : ms),
		)
	}

	#pos(id: string): { x: number; y: number } {
		const actor = this.scene.actors.find(a => a.id === id)
		const [x, y] = actor ? [actor.x, actor.y] : [50, 50]
		if (!this.tall) {
			return { x: (x / 100) * this.#width, y: (y / 100) * this.#height }
		}
		// tall: each column of the wide picture is a row, rows packed tight
		return { x: 60 + (y / 100) * (this.#width - 120), y: 70 + this.#rows().indexOf(x) * rowGap }
	}

	#rows(): number[] {
		return [...new Set(this.scene.actors.map(actor => actor.x))].sort((a, b) => a - b)
	}

	#paint(picture: Picture): void {
		this.#picture = picture
		const links = [...picture.links.values()].map(link => this.#cable(link)).join('')
		const actors = this.scene.actors.map(actor => this.#actor(actor.id)).join('')
		this.svg.innerHTML = `<g class="links">${links}</g><g class="actors">${actors}</g><g class="fx"></g>`
		this.#fx = this.svg.querySelector('.fx') as SVGGElement
	}

	#cable(link: { a: string; b: string; cut: boolean }): string {
		const p = this.#pos(link.a)
		const q = this.#pos(link.b)
		const scissors = link.cut
			? `<g class="scissors" transform="translate(${(p.x + q.x) / 2} ${(p.y + q.y) / 2})"><circle r="15"/><text y="5" text-anchor="middle">✂</text></g>`
			: ''
		return `<g class="cable${link.cut ? ' cut' : ''}" data-k="${key(link.a, link.b)}"><line x1="${p.x}" y1="${p.y}" x2="${q.x}" y2="${q.y}"/>${scissors}</g>`
	}

	#actor(id: string): string {
		const actor = this.scene.actors.find(a => a.id === id)
		const state = this.#picture.actors.get(id)
		if (!actor || !state) return ''
		const p = this.#pos(id)
		const badge = { home: '★ HOME', copy: 'COPY', down: 'CUT OFF' }[state.mark ?? ''] ?? ''
		const lines = state.lines
			.slice(-3)
			.map(
				(line, i) =>
					`<text class="line${line.struck ? ' struck' : ''}${line.fresh ? ' fresh' : ''}" y="${68 + i * 17}" text-anchor="middle">${esc(line.text)}</text>`,
			)
			.join('')
		return `<g class="actor ${state.mark ?? ''}" data-id="${id}" transform="translate(${p.x} ${p.y})">
			${state.mark === 'home' ? '<circle class="halo" r="46"/>' : ''}
			<g class="tile"><circle r="32"/><g class="icon" transform="translate(-15 -15) scale(1.25)">${icons[actor.icon] ?? ''}</g></g>
			${badge ? `<text class="badge ${state.mark}" y="-46" text-anchor="middle">${badge}</text>` : ''}
			<text class="label" y="50" text-anchor="middle">${esc(actor.label)}</text>${lines}</g>`
	}

	async #act(action: Action, run: number): Promise<void> {
		switch (action.verb) {
			case 'send':
				return this.#send(action, run)
			case 'pulse':
				return this.#pulse(action.a, run)
			case 'say':
				return this.#say(action, run)
			case 'wait':
				await this.#sleep(Number(action.a), run)
				return
			default:
				apply(this.#picture, action)
				this.#paint(this.#picture)
				await this.#sleep(action.verb === 'cut' ? 650 : 420, run)
		}
	}

	// a packet along an arc, from the sender's edge to the receiver's, a trail behind it
	async #send(action: Action, run: number): Promise<void> {
		const p = this.#pos(action.a)
		const q = this.#pos(action.b)
		const dx = q.x - p.x
		const dy = q.y - p.y
		const dist = Math.hypot(dx, dy) || 1
		const bend = Math.min(130, dist * 0.22) * (dx >= 0 ? -1 : 1) * (this.tall ? -1 : 1)
		const cx = (p.x + q.x) / 2 - (dy / dist) * bend
		const cy = (p.y + q.y) / 2 + (dx / dist) * bend
		const tone = action.color ?? (action.ghost ? 'ghost' : action.lost ? 'accent' : 'accent')
		const width = Math.max(30, action.label.length * 7.4 + 22)
		const group = document.createElementNS(svgNS, 'g')
		group.setAttribute('class', `send ${tone}`)
		group.innerHTML = `<path class="trail" d="M${p.x} ${p.y} Q${cx} ${cy} ${q.x} ${q.y}"/><g class="packet"><rect x="${-width / 2}" y="-13" width="${width}" height="26" rx="13"/><text y="4.5" text-anchor="middle">${esc(action.label)}</text></g>`
		this.#fx.append(group)
		const trail = group.querySelector('.trail') as SVGPathElement
		const packet = group.querySelector('.packet') as SVGGElement
		const length = trail.getTotalLength()
		const from = Math.min(36, length / 4)
		const to = Math.max(from, length - 40)
		const stop = action.lost ? 0.55 : 1
		trail.style.strokeDasharray = `${length}`
		const flown = await this.#tween(
			(action.lost ? 0.55 : 1) * (620 + dist * 0.85),
			t => {
				const k = ease(t) * stop
				const at = trail.getPointAtLength(from + (to - from) * k)
				packet.setAttribute(
					'transform',
					`translate(${at.x} ${at.y}) scale(${0.8 + 0.2 * Math.min(1, t * 4)})`,
				)
				trail.style.strokeDashoffset = `${length * (1 - k)}`
			},
			run,
		)
		if (!flown) return
		if (action.lost) {
			group.classList.add('lost')
			const label = packet.querySelector('text')
			if (label) label.textContent = `✕ ${action.label}`
		} else {
			void this.#pulse(action.b, run)
		}
		await this.#tween(
			action.lost ? 700 : 380,
			t => {
				packet.style.opacity = `${1 - out(t)}`
				trail.style.opacity = `${0.6 * (1 - t)}`
			},
			run,
		)
		group.remove()
	}

	async #pulse(id: string, run: number): Promise<void> {
		const p = this.#pos(id)
		const ring = document.createElementNS(svgNS, 'circle')
		ring.setAttribute('class', 'ring')
		ring.setAttribute('cx', `${p.x}`)
		ring.setAttribute('cy', `${p.y}`)
		this.#fx.prepend(ring)
		const tile = this.svg.querySelector(`.actor[data-id="${id}"] .tile`)
		await this.#tween(
			520,
			t => {
				ring.setAttribute('r', `${32 + 28 * out(t)}`)
				ring.style.opacity = `${0.7 * (1 - t)}`
				tile?.setAttribute('transform', `scale(${1 + 0.08 * Math.sin(Math.PI * t)})`)
			},
			run,
		)
		ring.remove()
	}

	// a word said beside the speaker, on the side with room
	async #say(action: Action, run: number): Promise<void> {
		const p = this.#pos(action.a)
		const right = p.x < this.#width * 0.68
		const x = p.x + (right ? 44 : -44)
		const group = document.createElementNS(svgNS, 'g')
		group.setAttribute('class', `say ${action.color ?? ''}`)
		group.innerHTML = `<text text-anchor="${right ? 'start' : 'end'}">${esc(action.b)}</text>`
		this.#fx.append(group)
		await this.#tween(
			320,
			t => {
				group.style.opacity = `${t}`
				group.setAttribute(
					'transform',
					`translate(${x + (1 - out(t)) * (right ? -8 : 8)} ${p.y - 6})`,
				)
			},
			run,
		)
		await this.#sleep(450, run)
	}

	#sleep(ms: number, run: number): Promise<boolean> {
		return new Promise(resolve =>
			setTimeout(() => resolve(run === this.#run), this.reduced() ? 1 : ms),
		)
	}

	#tween(ms: number, draw: (t: number) => void, run: number): Promise<boolean> {
		const span = this.reduced() ? 1 : ms
		return new Promise(resolve => {
			const start = performance.now()
			const frame = (now: number) => {
				if (run !== this.#run) return resolve(false)
				const t = Math.min(1, Math.max(0, now - start) / span)
				draw(t)
				if (t < 1) requestAnimationFrame(frame)
				else resolve(true)
			}
			requestAnimationFrame(frame)
		})
	}
}

function esc(text: string): string {
	return text.replace(/&/g, '&amp;').replace(/</g, '&lt;')
}
