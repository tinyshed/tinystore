/** Who asked for a page, as the views are counted. */
export type Agent = 'person' | 'crawler' | 'agent'

// the fetchers that read for a model or an assistant, by the names they give
// themselves, under the provider whose they are
const agents: Record<string, RegExp> = {
	anthropic: /\b(?:ClaudeBot|Claude-User|Claude-SearchBot|anthropic-ai)\b/i,
	openai: /\b(?:GPTBot|ChatGPT-User|OAI-SearchBot)\b/i,
	perplexity: /\b(?:PerplexityBot|Perplexity-User)\b/i,
	google: /\b(?:Google-Extended|GoogleOther)\b/i,
	'common-crawl': /\bCCBot\b/i,
	bytedance: /\bBytespider\b/i,
	amazon: /\bAmazonbot\b/i,
	apple: /\bApplebot-Extended\b/i,
	meta: /\bmeta-externalagent\b/i,
	cohere: /\bcohere-ai\b/i,
	duckduckgo: /\bDuckAssistBot\b/i,
	mistral: /\bMistralAI-User\b/i,
	you: /\bYouBot\b/i,
}

const crawlers =
	/bot|crawl|spider|slurp|^curl\/|^wget|python-requests|python-httpx|aiohttp|go-http-client|okhttp|axios|^node|headlesschrome|lighthouse/i

/**
 * A reader by the User-Agent they send: an agent reading for a model, any
 * other program, or a person. A request with no User-Agent is a program.
 */
export function agentOf(userAgent: string | null): Agent {
	if (userAgent === null || userAgent.trim() === '') {
		return 'crawler'
	}
	if (providerOf(userAgent) !== undefined) {
		return 'agent'
	}
	return crawlers.test(userAgent) ? 'crawler' : 'person'
}

/** Whose agent a User-Agent names, 'anthropic' of `Claude-User`; undefined for anyone else. */
export function providerOf(userAgent: string | null): string | undefined {
	if (userAgent === null) {
		return undefined
	}
	return Object.entries(agents).find(([, names]) => names.test(userAgent))?.[0]
}
