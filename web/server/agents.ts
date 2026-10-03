/** Who asked for a page, as the views are counted. */
export type Agent = 'person' | 'crawler' | 'agent'

// the fetchers that read for a model or an assistant, by the names they give themselves
const agents =
	/\b(GPTBot|ChatGPT-User|OAI-SearchBot|ClaudeBot|Claude-User|Claude-SearchBot|anthropic-ai|PerplexityBot|Perplexity-User|Google-Extended|GoogleOther|CCBot|Bytespider|Amazonbot|Applebot-Extended|meta-externalagent|cohere-ai|DuckAssistBot|MistralAI-User|YouBot)\b/i

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
	if (agents.test(userAgent)) {
		return 'agent'
	}
	return crawlers.test(userAgent) ? 'crawler' : 'person'
}
