import { describe, expect, test } from 'bun:test'
import { createHash } from 'node:crypto'

import { apiEngines } from './api'
import { loadSite } from './site'
import { skillIndex, skillMarkdown, skillName } from './skill'

// the docs are read once, and a cold read outlasts a test's five seconds
const reading = 60_000

describe('the skill', () => {
	test(
		"is TinyStore's install and every engine's first examples, as agentskills.io reads a skill",
		async () => {
			const site = await loadSite()
			const skill = skillMarkdown(site)
			const front = skill.split('---\n')[1] ?? ''
			const description = JSON.parse(/^description: (.*)$/m.exec(front)?.[1] ?? '""') as string

			expect(skillName).toMatch(/^[a-z0-9]+(-[a-z0-9]+)*$/)
			expect(front).toStartWith(`name: ${skillName}\n`)
			expect(description).toStartWith(site.summary)
			expect(description.length).toBeLessThanOrEqual(1024)
			for (const engine of apiEngines) {
				expect(skill).toContain(`\n## ${engine.title}\n`)
			}
			for (const language of ['ts', 'python', 'go', 'rust']) {
				expect(skill).toContain(`\`\`\`${language}\n`)
			}
			expect(skill).toContain(`(${site.origin}/llms.txt)`)
			expect(skill).not.toContain('](../')
		},
		reading,
	)

	test(
		'is listed by the discovery index with the SHA-256 of its bytes',
		async () => {
			const site = await loadSite()
			const digest = createHash('sha256').update(skillMarkdown(site)).digest('hex')

			expect(JSON.parse(skillIndex(site))).toEqual({
				$schema: 'https://schemas.agentskills.io/discovery/0.2.0/schema.json',
				skills: [
					{
						name: skillName,
						type: 'skill-md',
						description: expect.stringContaining(site.summary),
						url: `${site.origin}/.well-known/agent-skills/${skillName}/SKILL.md`,
						digest: `sha256:${digest}`,
					},
				],
			})
		},
		reading,
	)
})
