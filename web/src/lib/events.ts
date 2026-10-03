/**
 * What a reader does that the server cannot see in its requests: copies,
 * searches, the language picked. The server counts these names and refuses
 * any other, so that what is counted stays a short list.
 */
export const readerEvents = [
	'copy-code',
	'copy-page',
	'search',
	'language',
	'theme',
	'outbound',
] as const

export type ReaderEvent = (typeof readerEvents)[number]
