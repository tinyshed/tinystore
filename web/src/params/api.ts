import type { ParamMatcher } from '@sveltejs/kit'

import { apiLanguages } from '$lib/content/api'

// llms-go.txt and its siblings, and no other llms-*.txt
export const match: ParamMatcher = param => (apiLanguages as readonly string[]).includes(param)
