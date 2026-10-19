import { describe, expect, test } from 'bun:test'

import { parseScene } from './parse'

const pay = `
code ts
  const charges = store.once('charges', { keep: '1d' })
  return charges.run(key, () => stripe.charge(order, key))
end
actor buyer "Buyer" at 8,50 icon=user
actor kv "kv.db" at 64,50 icon=database
link buyer kv
set kv "charges: —"

step "The buyer clicks Pay twice." line=2
  send buyer -> kv "run key-7f"
  & send buyer -> kv "run key-7f" ghost
  say kv "waiting…"

step "Stripe answers." line=1-2
  send kv -> buyer "receipt" color=ok
`

describe('a scene', () => {
	test('reads its code, actors, start and steps', () => {
		const scene = parseScene(pay)

		expect(scene.code).toEqual({
			lang: 'ts',
			text: "const charges = store.once('charges', { keep: '1d' })\nreturn charges.run(key, () => stripe.charge(order, key))",
		})
		expect(scene.actors.map(actor => `${actor.id}@${actor.x},${actor.y}:${actor.icon}`)).toEqual([
			'buyer@8,50:user',
			'kv@64,50:database',
		])
		expect(scene.setup.map(action => action.verb)).toEqual(['link', 'set'])
		expect(scene.steps.map(step => step.lines)).toEqual([
			[2, 2],
			[1, 2],
		])
		expect(scene.steps[0]?.actions[1]).toMatchObject({
			verb: 'send',
			together: true,
			ghost: true,
			label: 'run key-7f',
		})
		expect(scene.steps[1]?.actions[0]).toMatchObject({ a: 'kv', b: 'buyer', color: 'ok' })
	})

	test('fails on what it cannot play, naming the line', () => {
		const fail = (text: string) => () => parseScene(`actor a "A" at 10,10 icon=user\n${text}`)

		expect(fail('step "x"\n  send a -> b "hi"')).toThrow('send names b, and the scene has a')
		expect(fail('step "x"\n  fly a')).toThrow('scene line 3: fly is not a verb')
		expect(fail('actor b "B" at 10,10 icon=rocket\nstep "x"')).toThrow('b wears icon rocket')
		expect(fail('actor b "B" at 120,10\nstep "x"')).toThrow('x and y are percents')
		expect(fail('step "x" line=3\n  pulse a')).toThrow('runs line 3, and the code has 0')
		expect(fail('mark a boss\nstep "x"')).toThrow('a mark is home, copy or down')
		expect(() => parseScene('actor a "A" at 1,1 icon=user')).toThrow('at least one `step`')
	})
})
