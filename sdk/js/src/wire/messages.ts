// Every message of docs/wire.md, by the names its tables give each field.
// messages.json holds a vector of each, which test/wire.test.ts reads through
// these declarations.

import {
	bin,
	bool,
	floats,
	int,
	int64,
	ints,
	key,
	kvValue,
	list,
	message,
	names,
	nullable,
	sqlValue,
	str,
	text,
	uint,
} from './codec.ts'

export const methods = {
	'kv.open': 0x0101,
	'kv.get': 0x0102,
	'kv.has': 0x0103,
	'kv.set': 0x0104,
	'kv.delete': 0x0105,
	'kv.take': 0x0106,
	'kv.touch': 0x0107,
	'kv.add': 0x0108,
	'kv.max': 0x0109,
	'kv.clear': 0x010a,
	'kv.batch': 0x010b,
	'kv.view': 0x010c,
	'kv.scan': 0x010d,
	'kv.allow': 0x010e,
	'kv.configure': 0x010f,
	'kv.watch': 0x0110,
	'jobs.open': 0x0201,
	'jobs.enqueue': 0x0202,
	'jobs.update': 0x0203,
	'jobs.cancel': 0x0204,
	'jobs.get': 0x0205,
	'jobs.claim': 0x0206,
	'jobs.settle': 0x0207,
	'jobs.scan': 0x0208,
	'jobs.work': 0x0209,
	'blobs.open': 0x0301,
	'blobs.stat': 0x0302,
	'blobs.delete': 0x0303,
	'blobs.copy': 0x0304,
	'blobs.move': 0x0305,
	'blobs.usage': 0x0306,
	'blobs.clear': 0x0307,
	'blobs.scan': 0x0308,
	'blobs.put': 0x0309,
	'blobs.get': 0x030a,
	'sql.open': 0x0401,
	'sql.exec': 0x0402,
	'sql.query': 0x0403,
	'sql.batch': 0x0404,
	'records.append': 0x0501,
	'records.read': 0x0502,
	'records.follow': 0x0503,
	'records.lines': 0x0504,
	'records.damaged': 0x0505,
	'records.drop': 0x0506,
	'metrics.ingest': 0x0601,
	'metrics.read': 0x0602,
	'metrics.aggregate': 0x0603,
	'metrics.drop': 0x0604,
	'metrics.explain': 0x0605,
} as const

export type Method = keyof typeof methods

export const Hello = message('hello', {
	protocol: [1, uint],
	client: [2, str],
	token: [3, str],
	maxBody: [4, uint],
	streamCredit: [5, uint],
	challenge: [6, bin],
})

export const Welcome = message('welcome', {
	protocol: [1, uint],
	server: [2, str],
	instance: [3, bin],
	capability: [4, str],
	maxBody: [5, uint],
	inFlight: [6, uint],
	connectionCredit: [7, uint],
	streamCredit: [8, uint],
	engines: [9, list(str)],
	now: [10, int],
	proof: [11, bin],
})

export const GoAway = message('goaway', { code: [1, str], message: [2, str] })

export const Failure = message('error', {
	code: [1, str],
	message: [2, str],
	what: [3, names(text)],
})

export const Handle = message('handle', { handle: [1, uint] })

export const Empty = message('empty', {})

export const KvBucket = message('kv.bucket', {
	name: [1, str],
	counters: [2, bool],
	defaultTtl: [3, uint],
	sliding: [4, uint],
	loseAtMost: [5, uint],
	config: [6, bool],
	rate: [7, uint],
	per: [8, uint],
	burst: [9, uint],
})

const kvCall = {
	handle: [1, uint],
	owners: [2, list(key)],
	key: [3, key],
	value: [4, kvValue],
	ttl: [5, uint],
	expireAt: [6, int],
	ifVersion: [7, bin],
	ifAbsent: [8, bool],
	n: [9, int64],
	after: [10, key],
	limit: [11, uint],
} as const

export const KvCall = message('kv.call', kvCall)

export const KvOperation = message('kv.operation', { method: [0, uint], ...kvCall })

export const KvCalls = message('kv.calls', { calls: [1, list(KvOperation)] })

export const KvEntry = message('kv.entry', {
	found: [1, bool],
	value: [2, kvValue],
	version: [3, bin],
	expires: [4, int],
	key: [5, key],
})

export const KvResults = message('kv.results', { entries: [1, list(KvEntry)] })

export const KvPage = message('kv.page', { more: [1, bool], after: [2, key] })

export const KvAllowance = message('kv.allowance', {
	ok: [1, bool],
	left: [2, uint],
	retryAfter: [3, uint],
})

export const KvConfigure = message('kv.configure', {
	handle: [1, uint],
	set: [2, list(str)],
	reset: [3, list(str)],
})

export const KvKept = message('kv.kept', { changes: [1, uint], fields: [2, list(str)] })

export const JobsRepeat = message('jobs.repeat', {
	cron: [1, str],
	zone: [2, str],
	every: [3, uint],
})

export const JobsQueue = message('jobs.queue', {
	name: [1, str],
	lease: [2, uint],
	maxAttempts: [3, uint],
	backoffFirst: [4, uint],
	backoffMost: [5, uint],
	maxWaiting: [6, uint],
	keepFailed: [7, uint],
	keepDone: [8, uint],
	schedule: [9, JobsRepeat],
})

const jobsJob = {
	value: [1, str],
	key: [2, str],
	at: [3, int],
	after: [4, uint],
	repeat: [5, JobsRepeat],
} as const

export const JobsJob = message('jobs.job', jobsJob)

export const JobsBatch = message('jobs.batch', { handle: [1, uint], jobs: [2, list(JobsJob)] })

export const JobsChange = message('jobs.change', { ...jobsJob, handle: [6, uint] })

export const JobsKey = message('jobs.key', { handle: [1, uint], key: [2, str] })

export const JobsEntry = message('jobs.entry', {
	found: [1, bool],
	key: [2, str],
	value: [3, str],
	at: [4, int],
	attempt: [5, uint],
	state: [6, uint],
	err: [7, str],
	repeat: [8, str],
})

export const JobsLease = message('jobs.lease', { handle: [1, uint], lease: [2, uint] })

export const JobsHeld = message('jobs.held', {
	found: [1, bool],
	job: [2, uint],
	key: [3, str],
	value: [4, str],
	at: [5, int],
	attempt: [6, uint],
})

export const JobsOutcome = message('jobs.outcome', {
	job: [1, uint],
	how: [2, uint],
	err: [3, str],
	at: [4, int],
	after: [5, uint],
})

export const JobsOutcomes = message('jobs.outcomes', { outcomes: [1, list(JobsOutcome)] })

export const JobsSettled = message('jobs.settled', { settled: [1, list(nullable(Failure))] })

export const JobsQuery = message('jobs.query', {
	handle: [1, uint],
	prefix: [2, str],
	state: [3, uint],
	after: [4, str],
	limit: [5, uint],
})

export const JobsPage = message('jobs.page', { more: [1, bool], after: [2, str] })

export const JobsWorkers = message('jobs.workers', {
	handle: [1, uint],
	workers: [2, uint],
	timeout: [3, uint],
	untilIdle: [4, bool],
})

export const BlobsBucket = message('blobs.bucket', {
	name: [1, str],
	defaultTtl: [2, uint],
	maxSize: [3, uint],
})

export const BlobsCall = message('blobs.call', {
	handle: [1, uint],
	owners: [2, list(key)],
	key: [3, str],
	to: [4, str],
	contentType: [5, str],
	meta: [6, names(text)],
	ttl: [7, uint],
	expireAt: [8, int],
	size: [9, uint],
	ifMatch: [10, str],
	ifNoneMatch: [11, bool],
	prefix: [12, str],
	after: [13, str],
	limit: [14, uint],
	offset: [15, uint],
	length: [16, uint],
})

export const BlobsObject = message('blobs.object', {
	found: [1, bool],
	key: [2, str],
	size: [3, uint],
	etag: [4, str],
	contentType: [5, str],
	modified: [6, int],
	expires: [7, int],
	meta: [8, names(text)],
})

export const BlobsTotal = message('blobs.total', { objects: [1, int], bytes: [2, int] })

export const BlobsPage = message('blobs.page', { more: [1, bool], after: [2, str] })

export const SqlMigration = message('sql.migration', { name: [1, str], text: [2, str] })

export const SqlDatabase = message('sql.database', {
	name: [1, str],
	migrations: [2, list(SqlMigration)],
})

export const SqlStatement = message('sql.statement', {
	handle: [1, uint],
	sql: [2, str],
	args: [3, list(sqlValue)],
	named: [4, names(sqlValue)],
	write: [5, bool],
	rows: [6, bool],
})

export const SqlStatements = message('sql.statements', {
	handle: [1, uint],
	statements: [2, list(SqlStatement)],
	read: [3, bool],
})

export const SqlDone = message('sql.done', { changes: [1, int], lastId: [2, int] })

export const SqlColumns = message('sql.columns', { columns: [1, list(str)] })

export const SqlRow = message('sql.row', { values: [1, list(sqlValue)] })

export const SqlResult = message('sql.result', {
	changes: [1, int],
	lastId: [2, int],
	columns: [3, list(str)],
	rows: [4, list(list(sqlValue))],
})

export const SqlResults = message('sql.results', { results: [1, list(SqlResult)] })

export const RecordsRecord = message('records.record', {
	at: [1, int64],
	stream: [2, text],
	name: [3, text],
	level: [4, int],
	body: [5, text],
	traceId: [6, bin],
	spanId: [7, bin],
	context: [8, list(text)],
	attrs: [9, list(text)],
})

export const RecordsBatch = message('records.batch', { records: [1, list(RecordsRecord)] })

export const RecordsQuery = message('records.query', {
	from: [1, int64],
	to: [2, int64],
	streams: [3, list(text)],
	names: [4, list(text)],
	minLevel: [5, int],
	traceId: [6, bin],
	attrs: [7, list(text)],
	context: [8, list(text)],
	newest: [9, bool],
	limit: [10, uint],
	budgetBlocks: [11, uint],
	budgetBytes: [12, uint],
	budgetRecords: [13, uint],
	search: [14, str],
})

export const RecordsPage = message('records.page', {
	more: [1, bool],
	from: [2, int64],
	to: [3, int64],
})

export const RecordsCursor = message('records.cursor', {
	segment: [1, int],
	row: [2, int],
	limit: [3, uint],
	expired: [4, uint],
})

export const RecordsStream = message('records.stream', { stream: [1, text] })

export const RecordsDamage = message('records.damage', {
	stream: [1, text],
	segment: [2, int],
	headRow: [3, int],
	from: [4, int64],
	to: [5, int64],
	reason: [6, str],
})

export const RecordsDamages = message('records.damages', { damages: [1, list(RecordsDamage)] })

export const MetricsSeries = message('metrics.series', {
	labels: [1, names(str)],
	kind: [2, str],
	times: [3, ints],
	values: [4, floats],
})

export const MetricsBatch = message('metrics.batch', { series: [1, list(MetricsSeries)] })

export const MetricsPlan = message('metrics.plan', {
	series: [1, uint],
	blocks: [2, uint],
	summarized: [3, uint],
	bytes: [4, uint],
	decoded: [5, uint],
	limitSeries: [6, uint],
	limitBlocks: [7, uint],
	limitBytes: [8, uint],
	limitDecoded: [9, uint],
	limitAnswered: [10, uint],
	stops: [11, nullable(Failure)],
})

export const MetricsCondition = message('metrics.condition', {
	label: [1, str],
	kind: [2, str],
	values: [3, list(str)],
})

export const MetricsRange = message('metrics.range', {
	matchers: [1, names(str)],
	from: [2, int64],
	to: [3, int64],
	limitSeries: [4, uint],
	limitBlocks: [5, uint],
	limitBytes: [6, uint],
	limitDecoded: [7, uint],
	limitAnswered: [8, uint],
	width: [9, uint],
	op: [10, str],
	where: [11, list(MetricsCondition)],
	by: [12, list(str)],
	without: [13, list(str)],
})

export const MetricsBuckets = message('metrics.buckets', {
	labels: [1, names(str)],
	kind: [2, str],
	from: [3, ints],
	to: [4, ints],
	count: [5, ints],
	resets: [6, ints],
	values: [7, floats],
	flags: [8, bin],
})

export const MetricsLabels = message('metrics.labels', { labels: [1, names(str)] })

export const MetricsDropped = message('metrics.dropped', {
	found: [1, bool],
	unreadableGroups: [2, uint],
})

/** Every message by the name messages.json gives it. */
export const messages = {
	hello: Hello,
	welcome: Welcome,
	goaway: GoAway,
	error: Failure,
	handle: Handle,
	empty: Empty,
	'kv.bucket': KvBucket,
	'kv.call': KvCall,
	'kv.operation': KvOperation,
	'kv.calls': KvCalls,
	'kv.entry': KvEntry,
	'kv.results': KvResults,
	'kv.page': KvPage,
	'kv.allowance': KvAllowance,
	'kv.configure': KvConfigure,
	'kv.kept': KvKept,
	'jobs.queue': JobsQueue,
	'jobs.repeat': JobsRepeat,
	'jobs.job': JobsJob,
	'jobs.batch': JobsBatch,
	'jobs.change': JobsChange,
	'jobs.key': JobsKey,
	'jobs.entry': JobsEntry,
	'jobs.lease': JobsLease,
	'jobs.held': JobsHeld,
	'jobs.outcome': JobsOutcome,
	'jobs.outcomes': JobsOutcomes,
	'jobs.settled': JobsSettled,
	'jobs.query': JobsQuery,
	'jobs.page': JobsPage,
	'jobs.workers': JobsWorkers,
	'blobs.bucket': BlobsBucket,
	'blobs.call': BlobsCall,
	'blobs.object': BlobsObject,
	'blobs.total': BlobsTotal,
	'blobs.page': BlobsPage,
	'sql.database': SqlDatabase,
	'sql.migration': SqlMigration,
	'sql.statement': SqlStatement,
	'sql.statements': SqlStatements,
	'sql.done': SqlDone,
	'sql.columns': SqlColumns,
	'sql.row': SqlRow,
	'sql.result': SqlResult,
	'sql.results': SqlResults,
	'records.record': RecordsRecord,
	'records.batch': RecordsBatch,
	'records.query': RecordsQuery,
	'records.page': RecordsPage,
	'records.cursor': RecordsCursor,
	'records.stream': RecordsStream,
	'records.damage': RecordsDamage,
	'records.damages': RecordsDamages,
	'metrics.series': MetricsSeries,
	'metrics.batch': MetricsBatch,
	'metrics.range': MetricsRange,
	'metrics.condition': MetricsCondition,
	'metrics.plan': MetricsPlan,
	'metrics.buckets': MetricsBuckets,
	'metrics.labels': MetricsLabels,
	'metrics.dropped': MetricsDropped,
} as const
