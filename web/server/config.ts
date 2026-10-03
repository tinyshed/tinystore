import { z } from 'zod'

const schema = z.object({
	HOST: z.union([z.hostname(), z.ipv4(), z.ipv6()]).default('127.0.0.1'),
	// port 0 asks the operating system for a free port, which tests want
	PORT: z.coerce.number().int().min(0).max(65_535).default(3000),

	// the prerendered site, web/build after `bun run build`
	SITE_DIR: z.string().min(1).default('build'),
	// the TinyStore directory the server writes its views, events and logs into
	DATA_DIR: z.string().min(1).default('data'),

	// behind a proxy the reader's address is the first of X-Forwarded-For, believed only when told
	TRUST_PROXY: z.stringbool().default(false),

	SHUTDOWN_TIMEOUT_MS: z.coerce.number().int().min(0).default(10_000),
})

export function createConfig(env: Record<string, string | undefined>) {
	const result = schema.safeParse(env)

	if (!result.success) {
		const details = result.error.issues
			.map(issue => `  ${issue.path.join('.') || '(root)'}: ${issue.message}`)
			.join('\n')

		throw new Error(`invalid environment configuration:\n${details}`)
	}

	return result.data
}

export type Config = ReturnType<typeof createConfig>
