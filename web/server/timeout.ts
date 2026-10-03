export class TimeoutError extends Error {
	constructor(
		message: string,
		public readonly timeoutMs: number,
	) {
		super(message)
		this.name = new.target.name
	}
}

export function withTimeout<T>(task: Promise<T>, timeoutMs: number, label: string): Promise<T> {
	let timer: ReturnType<typeof setTimeout> | undefined

	const deadline = new Promise<never>((_, reject) => {
		timer = setTimeout(() => {
			reject(new TimeoutError(`${label} did not finish within ${timeoutMs}ms`, timeoutMs))
		}, timeoutMs)
	})

	// clearTimeout, not unref: an unreferenced timer never fires
	return Promise.race([task, deadline]).finally(() => {
		clearTimeout(timer)
	})
}
