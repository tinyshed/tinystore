import type { ParamMatcher } from '@sveltejs/kit'

// a page's path, beside its markdown at the same path with .md after it
export const match: ParamMatcher = param => !param.endsWith('.md')
