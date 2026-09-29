// Standard Schema, https://standardschema.dev: the interface zod, valibot and
// arktype each implement, copied here as the spec asks rather than depended on.

/** A schema that checks a value and may give back one of another type. */
export interface StandardSchemaV1<Input = unknown, Output = Input> {
	readonly '~standard': StandardSchemaV1.Props<Input, Output>
}

export declare namespace StandardSchemaV1 {
	export interface Props<Input = unknown, Output = Input> {
		readonly version: 1
		readonly vendor: string
		readonly validate: (value: unknown) => Result<Output> | Promise<Result<Output>>
		readonly types?: Types<Input, Output> | undefined
	}

	export type Result<Output> = SuccessResult<Output> | FailureResult

	export interface SuccessResult<Output> {
		readonly value: Output
		readonly issues?: undefined
	}

	export interface FailureResult {
		readonly issues: ReadonlyArray<Issue>
	}

	export interface Issue {
		readonly message: string
		readonly path?: ReadonlyArray<PropertyKey | PathSegment> | undefined
	}

	export interface PathSegment {
		readonly key: PropertyKey
	}

	export interface Types<Input = unknown, Output = Input> {
		readonly input: Input
		readonly output: Output
	}

	export type InferOutput<Schema extends StandardSchemaV1> = NonNullable<
		Schema['~standard']['types']
	>['output']
}

export function isSchema(value: unknown): value is StandardSchemaV1 {
	return typeof value === 'object' && value !== null && '~standard' in value
}

/** The value a schema gives back, or the issues it found, spelled as one line. */
export async function check<T>(
	schema: StandardSchemaV1<unknown, T>,
	value: unknown,
): Promise<{ value: T } | { issues: string }> {
	const result = await schema['~standard'].validate(value)
	if (result.issues === undefined) {
		return { value: result.value }
	}
	const issues = result.issues
		.map(issue => {
			const path = issue.path
				?.map(segment => String(typeof segment === 'object' ? segment.key : segment))
				.join('.')
			return path === undefined || path === '' ? issue.message : `${path}: ${issue.message}`
		})
		.join('; ')
	return { issues }
}
