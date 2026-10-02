package main

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"math"
	"path/filepath"
	"slices"
	"time"
	"unicode/utf8"

	"github.com/tinyshed/tinystore/records"
	"github.com/tinyshed/tinystore/server/reach"
	"github.com/tinyshed/tinystore/server/wire"
)

const mcpUsage = `usage:
  tinystore mcp [dir]   the store's tools for an AI agent: the Model Context Protocol on stdin and stdout

An agent reads the store through them and writes nothing: what the directory
holds, the application's logs, kv's keys, jobs, and SQL from one snapshot.
Claude Code adds it with:

  claude mcp add tinystore -- tinystore mcp ./data`

// the protocol versions mcp speaks, the newest first: a client asking for one
// it does not is answered with the newest
var mcpVersions = []string{"2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"}

const (
	toolTimeout = 30 * time.Second // what one tool call may take, a read of the store included
	rpcParse    = -32700           // JSON-RPC's codes
	rpcNoMethod = -32601
	rpcParams   = -32602
)

// rpcMessage is a JSON-RPC request, or a notification when it has no id
type rpcMessage struct {
	ID     json.RawMessage `json:"id,omitempty"`
	Method string          `json:"method"`
	Params json.RawMessage `json:"params,omitempty"`
}

type rpcReply struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      json.RawMessage `json:"id"`
	Result  any             `json:"result,omitempty"`
	Error   *rpcError       `json:"error,omitempty"`
}

type rpcError struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
}

// mcp answers an MCP client on in and out, a JSON-RPC message a line, until
// in ends: the client's leaving
func mcp(ctx context.Context, args []string, in io.Reader, out io.Writer, stderr io.Writer) error {
	flags := flag.NewFlagSet("mcp", flag.ContinueOnError)
	flags.SetOutput(stderr)
	flags.Usage = func() { fmt.Fprintln(stderr, mcpUsage) }
	given := flags.String("dir", "", "")
	dir, err := parseWithDir(flags, args, given)
	if err != nil {
		return err
	}

	a := &agent{dir: dir, handles: map[string]uint64{}}
	defer a.close()
	lines := bufio.NewScanner(in)
	lines.Buffer(make([]byte, 64<<10), 4<<20)
	encoder := json.NewEncoder(out)
	for lines.Scan() {
		if len(bytes.TrimSpace(lines.Bytes())) == 0 {
			continue
		}
		reply, answer := a.handle(ctx, lines.Bytes())
		if !answer {
			continue
		}
		if err = encoder.Encode(reply); err != nil {
			return err
		}
	}
	return lines.Err()
}

// agent is one MCP client's session: the directory it reads, and the
// connection to its server once a tool needed one
type agent struct {
	dir     string
	conn    *reach.Conn
	handles map[string]uint64 // a bucket's, a queue's or a database's handle on conn, by kind and name
}

func (a *agent) close() {
	if a.conn != nil {
		_ = a.conn.Close() // the client has left; nobody reads what a close says
	}
}

// handle answers one message; a notification is answered with nothing
func (a *agent) handle(ctx context.Context, line []byte) (rpcReply, bool) {
	var m rpcMessage
	if err := json.Unmarshal(line, &m); err != nil {
		return rpcReply{JSONRPC: "2.0", ID: json.RawMessage("null"), Error: &rpcError{
			Code: rpcParse, Message: "a line that is not a JSON-RPC message: " + err.Error(),
		}}, true
	}
	if len(m.ID) == 0 {
		return rpcReply{}, false // initialized, cancelled: nothing answers a notification
	}
	reply := rpcReply{JSONRPC: "2.0", ID: m.ID}
	switch m.Method {
	case "initialize":
		reply.Result = a.initialize(m.Params)
	case "ping":
		reply.Result = struct{}{}
	case "tools/list":
		reply.Result = map[string]any{"tools": mcpTools}
	case "tools/call":
		reply.Result, reply.Error = a.call(ctx, m.Params)
	default:
		reply.Error = &rpcError{Code: rpcNoMethod, Message: "no method " + m.Method}
	}
	return reply, true
}

func (a *agent) initialize(params json.RawMessage) any {
	var asked struct {
		ProtocolVersion string `json:"protocolVersion"`
	}
	_ = json.Unmarshal(params, &asked) //nolint:errcheck // a version not given is answered with the newest
	spoken := mcpVersions[0]
	if slices.Contains(mcpVersions, asked.ProtocolVersion) {
		spoken = asked.ProtocolVersion
	}
	return map[string]any{
		"protocolVersion": spoken,
		"capabilities":    map[string]any{"tools": map[string]any{}},
		"serverInfo":      map[string]any{"name": "tinystore", "version": version()},
		"instructions": "These tools read the TinyStore in " + a.dir + ", the application's store, and write " +
			"nothing: what it holds, its logs and events, kv's keys, jobs, and SQL from one snapshot. Names of " +
			"buckets, queues and databases are the application's own, as its code opens them.",
	}
}

// call runs a tool: its failure is the tool's answer, marked an error, which
// the agent reads, and only a call that names no tool is JSON-RPC's error
func (a *agent) call(ctx context.Context, params json.RawMessage) (any, *rpcError) {
	var asked struct {
		Name      string          `json:"name"`
		Arguments json.RawMessage `json:"arguments"`
	}
	if err := json.Unmarshal(params, &asked); err != nil {
		return nil, &rpcError{Code: rpcParams, Message: err.Error()}
	}
	i := slices.IndexFunc(mcpTools, func(t mcpTool) bool { return t.Name == asked.Name })
	if i < 0 {
		return nil, &rpcError{Code: rpcParams, Message: "no tool " + asked.Name}
	}
	if len(asked.Arguments) == 0 {
		asked.Arguments = json.RawMessage("{}")
	}
	ctx, cancel := context.WithTimeout(ctx, toolTimeout)
	defer cancel()
	answer, err := mcpTools[i].run(ctx, a, asked.Arguments)
	if err != nil {
		return toolText(err.Error(), true), nil
	}
	text, err := json.MarshalIndent(answer, "", "  ")
	if err != nil {
		return toolText(err.Error(), true), nil
	}
	return toolText(string(text), false), nil
}

func toolText(text string, failed bool) map[string]any {
	answer := map[string]any{"content": []map[string]any{{"type": "text", "text": text}}}
	if failed {
		answer["isError"] = true
	}
	return answer
}

// kept refuses a read of an engine whose file the store does not have, which
// the server would make as it opened the engine for the read
func (a *agent) kept(file, engine string) error {
	if _, err := stat(filepath.Join(a.dir, file)); err != nil {
		return fmt.Errorf("the store keeps no %s: %s is not in %s", engine, file, a.dir)
	}
	return nil
}

// store is the connection to the directory's server, reached at the first
// tool that needs it and again after it was lost
func (a *agent) store(ctx context.Context) (*reach.Conn, error) {
	if a.conn != nil {
		select {
		case <-a.conn.Done():
			a.close()
			a.conn, a.handles = nil, map[string]uint64{}
		default:
			return a.conn, nil
		}
	}
	conn, err := reachStore(ctx, a.dir)
	if err != nil {
		return nil, err
	}
	a.conn = conn
	return conn, nil
}

// open is a handle on conn, opened once a name: open the request's bytes
func (a *agent) open(ctx context.Context, conn *reach.Conn, method wire.Method, key string, open reach.Message) (
	uint64, error,
) {
	if handle, ok := a.handles[key]; ok {
		return handle, nil
	}
	body, err := conn.Call(ctx, method, open)
	if err != nil {
		return 0, err
	}
	var handle wire.Handle
	if err = handle.Decode(body); err != nil {
		return 0, err
	}
	a.handles[key] = handle.Handle
	return handle.Handle, nil
}

// mcpTool is a tool and what runs it; only the first three are the client's
type mcpTool struct {
	Name        string         `json:"name"`
	Description string         `json:"description"`
	InputSchema map[string]any `json:"inputSchema"`
	run         func(ctx context.Context, a *agent, arguments json.RawMessage) (any, error)
}

func schema(required []string, properties map[string]any) map[string]any {
	if required == nil {
		required = []string{}
	}
	return map[string]any{"type": "object", "properties": properties, "required": required}
}

func property(kind, description string) map[string]any {
	return map[string]any{"type": kind, "description": description}
}

var owners = map[string]any{
	"type": "array", "items": map[string]any{"type": "string"},
	"description": "the branch the key lies in, its owners in order, as the application's Of names them",
}

var mcpTools = []mcpTool{
	{
		Name:        "status",
		Description: "What the store's directory holds: each engine's file and its bytes, and the server serving it.",
		InputSchema: schema(nil, map[string]any{}),
		run:         runStatus,
	},
	{
		Name: "logs",
		Description: "The application's logs and events, the newest last, one JSON object a line: from a level " +
			"up, since a time, holding a text, of a stream.",
		InputSchema: schema(nil, map[string]any{
			"level":  property("string", "debug, info, warn or error: the least a record has"),
			"since":  property("string", "how far back, a Go duration: 15m, 1h, 24h"),
			"search": property("string", "text a record's body or name holds, its case ignored"),
			"stream": property("string", "the stream, as the application names its logger"),
			"limit":  property("integer", "the records, 100 unless it says, 1000 at most"),
		}),
		run: runLogs,
	},
	{
		Name:        "kv_get",
		Description: "A key's value in a kv bucket, or a counter's, with when it expires.",
		InputSchema: schema([]string{"bucket", "key"}, map[string]any{
			"bucket": property("string", "the bucket's name"),
			"key":    property("string", "the key"),
			"owners": owners,
		}),
		run: runKVGet,
	},
	{
		Name:        "kv_scan",
		Description: "A page of a kv bucket's keys and values in one branch, in the byte order of the keys.",
		InputSchema: schema([]string{"bucket"}, map[string]any{
			"bucket": property("string", "the bucket's name"),
			"owners": owners,
			"after":  property("string", "the key the page begins after: the last page's next"),
			"limit":  property("integer", "the keys, 50 unless it says, 1000 at most"),
		}),
		run: runKVScan,
	},
	{
		Name: "jobs_get",
		Description: "A job of a queue by its key: waiting with how many run ahead of it, running with its " +
			"progress, failed with its error, and its last run.",
		InputSchema: schema([]string{"queue", "key"}, map[string]any{
			"queue": property("string", "the queue's name"),
			"key":   property("string", "the job's key"),
		}),
		run: runJobsGet,
	},
	{
		Name:        "jobs_scan",
		Description: "A page of a queue's jobs under a key prefix, or of its failed jobs, the last failed first.",
		InputSchema: schema([]string{"queue"}, map[string]any{
			"queue":  property("string", "the queue's name"),
			"prefix": property("string", "the keys' prefix; none is every key"),
			"state":  property("string", "waiting, running or failed; failed without a prefix lists the failed"),
			"after":  property("string", "where the page before ended: its next"),
			"limit":  property("integer", "the jobs, 50 unless it says, 1000 at most"),
		}),
		run: runJobsScan,
	},
	{
		Name: "sql_query",
		Description: "The rows a statement returns from one of the application's SQL databases, read from one " +
			"snapshot, where no statement can write. The database is one the application has opened " +
			"through the store's server. Give the query a LIMIT: its rows travel in one message.",
		InputSchema: schema([]string{"database", "sql"}, map[string]any{
			"database": property("string", "the database's name, its file sql/<name>.db"),
			"sql":      property("string", "one statement that reads"),
			"args": map[string]any{
				"type": "array", "description": "the statement's positional arguments",
				"items": map[string]any{},
			},
		}),
		run: runSQLQuery,
	},
}

func arguments[T any](raw json.RawMessage) (T, error) {
	var asked T
	if err := json.Unmarshal(raw, &asked); err != nil {
		return asked, fmt.Errorf("the arguments: %w", err)
	}
	return asked, nil
}

func runStatus(ctx context.Context, a *agent, _ json.RawMessage) (any, error) {
	found, err := readStatus(a.dir)
	if err == nil && found.Server != nil {
		found.Server.Answers = answers(ctx, a.dir)
	}
	return found, err
}

func runLogs(ctx context.Context, a *agent, raw json.RawMessage) (any, error) {
	asked, err := arguments[struct {
		Level, Since, Search, Stream string
		Limit                        int
	}](raw)
	if err != nil {
		return nil, err
	}
	flags := logsFlags{level: asked.Level, grep: asked.Search, stream: asked.Stream, limit: min(asked.Limit, 1000)}
	if flags.limit <= 0 {
		flags.limit = 100
	}
	if asked.Since != "" {
		if flags.since, err = time.ParseDuration(asked.Since); err != nil {
			return nil, err
		}
	}
	query, err := recordsQuery(flags, time.Now())
	if err != nil {
		return nil, err
	}
	if err = a.kept("records.db", "logs"); err != nil {
		return nil, err
	}
	conn, err := a.store(ctx)
	if err != nil {
		return nil, err
	}
	found, _, err := readRecords(ctx, conn, query)
	if err != nil {
		return nil, err
	}
	slices.Reverse(found)
	var lines bytes.Buffer
	printer := records.NewPrinter(&lines, records.ConsoleJSON)
	for _, r := range found {
		if err = printRecord(printer, r); err != nil {
			return nil, err
		}
	}
	return json.RawMessage(jsonLines(lines.Bytes())), nil
}

// jsonLines is JSON lines as one JSON array, so that the tool's answer is JSON
func jsonLines(lines []byte) []byte {
	array := []byte{'['}
	for i, line := range bytes.Split(bytes.TrimSpace(lines), []byte{'\n'}) {
		if len(line) == 0 {
			continue
		}
		if i > 0 {
			array = append(array, ',')
		}
		array = append(array, line...)
	}
	return append(array, ']')
}

type kvAsked struct {
	Bucket, Key, After string
	Owners             []string
	Limit              int
}

// kvHandle opens a bucket of values, or counters when that is what the name
// holds
func (a *agent) kvHandle(ctx context.Context, bucket string) (*reach.Conn, uint64, bool, error) {
	if err := a.kept("kv.db", "kv"); err != nil {
		return nil, 0, false, err
	}
	conn, err := a.store(ctx)
	if err != nil {
		return nil, 0, false, err
	}
	handle, err := a.open(ctx, conn, wire.KVOpen, "kv values "+bucket, wire.KVBucket{Name: bucket})
	if err == nil {
		return conn, handle, false, nil
	}
	handle, countersErr := a.open(ctx, conn, wire.KVOpen, "kv counters "+bucket, wire.KVBucket{
		Name: bucket, Counters: true,
	})
	if countersErr != nil {
		return nil, 0, false, err
	}
	return conn, handle, true, nil
}

func runKVGet(ctx context.Context, a *agent, raw json.RawMessage) (any, error) {
	asked, err := arguments[kvAsked](raw)
	if err != nil {
		return nil, err
	}
	conn, handle, _, err := a.kvHandle(ctx, asked.Bucket)
	if err != nil {
		return nil, err
	}
	body, err := conn.Call(ctx, wire.KVGet, wire.KVCall{Handle: handle, Owners: asked.Owners, Key: asked.Key})
	if err != nil {
		return nil, err
	}
	var entry wire.KVEntry
	if err = entry.Decode(body); err != nil {
		return nil, err
	}
	return shownEntry(asked.Key, entry), nil
}

func runKVScan(ctx context.Context, a *agent, raw json.RawMessage) (any, error) {
	asked, err := arguments[kvAsked](raw)
	if err != nil {
		return nil, err
	}
	conn, handle, _, err := a.kvHandle(ctx, asked.Bucket)
	if err != nil {
		return nil, err
	}
	limit := uint64(max(min(asked.Limit, 1000), 0))
	if limit == 0 {
		limit = 50
	}
	st, err := conn.Open(ctx, wire.KVScan, wire.KVCall{
		Handle: handle, Owners: asked.Owners, After: asked.After, Limit: limit,
	}, true)
	if err != nil {
		return nil, err
	}
	if _, err = st.Response(ctx); err != nil {
		return nil, err
	}
	page := struct {
		Entries []map[string]any `json:"entries"`
		Next    string           `json:"next,omitempty"`
	}{Entries: []map[string]any{}}
	for {
		body, last, err := st.Next(ctx)
		if err != nil {
			return nil, err
		}
		if last {
			var ended wire.KVPage
			if err = ended.Decode(body); err == nil && ended.More {
				page.Next = ended.After
			}
			return page, err
		}
		var entry wire.KVEntry
		if err = entry.Decode(body); err != nil {
			return nil, err
		}
		entry.Found = true
		page.Entries = append(page.Entries, shownEntry(entry.Key, entry))
	}
}

// shownEntry is a kv entry as an agent reads it: a value that is JSON as that
// JSON, other text as text, and bytes that are not text in base64
func shownEntry(key string, entry wire.KVEntry) map[string]any {
	shown := map[string]any{"key": key, "found": entry.Found}
	if !entry.Found {
		return shown
	}
	switch value := entry.Value; {
	case value.Kind == wire.KVInt:
		shown["value"] = value.Int
	case value.Kind == wire.KVBytes && json.Valid(value.Bytes):
		shown["value"] = json.RawMessage(value.Bytes)
	case value.Kind == wire.KVBytes && utf8.Valid(value.Bytes):
		shown["value"] = string(value.Bytes)
	case value.Kind == wire.KVBytes:
		shown["value"], shown["encoding"] = value.Bytes, "base64"
	default:
		shown["value"] = nil
	}
	if entry.Expires != 0 {
		shown["expires"] = time.UnixMilli(entry.Expires).UTC()
	}
	return shown
}

type jobsAsked struct {
	Queue, Key, Prefix, State, After string
	Limit                            int
}

func (a *agent) queue(ctx context.Context, name string) (*reach.Conn, uint64, error) {
	if err := a.kept("jobs.db", "jobs"); err != nil {
		return nil, 0, err
	}
	conn, err := a.store(ctx)
	if err != nil {
		return nil, 0, err
	}
	handle, err := a.open(ctx, conn, wire.JobsOpen, "jobs "+name, wire.JobsQueue{Name: name})
	return conn, handle, err
}

func runJobsGet(ctx context.Context, a *agent, raw json.RawMessage) (any, error) {
	asked, err := arguments[jobsAsked](raw)
	if err != nil {
		return nil, err
	}
	conn, handle, err := a.queue(ctx, asked.Queue)
	if err != nil {
		return nil, err
	}
	body, err := conn.Call(ctx, wire.JobsGet, wire.JobsKey{Handle: handle, Key: asked.Key})
	if err != nil {
		return nil, err
	}
	var entry wire.JobsEntry
	if err = entry.Decode(body); err != nil {
		return nil, err
	}
	return shownJob(entry), nil
}

// scanStates are the states a jobs scan selects by
var scanStates = map[string]uint64{"waiting": 1, "running": 2, "failed": 3}

func runJobsScan(ctx context.Context, a *agent, raw json.RawMessage) (any, error) {
	asked, err := arguments[jobsAsked](raw)
	if err != nil {
		return nil, err
	}
	state, known := scanStates[asked.State]
	if asked.State != "" && !known {
		return nil, fmt.Errorf("a state of %q: waiting, running or failed", asked.State)
	}
	conn, handle, err := a.queue(ctx, asked.Queue)
	if err != nil {
		return nil, err
	}
	limit := uint64(max(min(asked.Limit, 1000), 0))
	if limit == 0 {
		limit = 50
	}
	st, err := conn.Open(ctx, wire.JobsScan, wire.JobsQuery{
		Handle: handle, Prefix: asked.Prefix, State: state, After: asked.After, Limit: limit,
	}, true)
	if err != nil {
		return nil, err
	}
	if _, err = st.Response(ctx); err != nil {
		return nil, err
	}
	page := struct {
		Jobs []map[string]any `json:"jobs"`
		Next string           `json:"next,omitempty"`
	}{Jobs: []map[string]any{}}
	for {
		body, last, err := st.Next(ctx)
		if err != nil {
			return nil, err
		}
		if last {
			var ended wire.JobsPage
			if err = ended.Decode(body); err == nil && ended.More {
				page.Next = ended.After
			}
			return page, err
		}
		var entry wire.JobsEntry
		if err = entry.Decode(body); err != nil {
			return nil, err
		}
		entry.Found = true
		page.Jobs = append(page.Jobs, shownJob(entry))
	}
}

// stateNames names the states wire.JobsEntry numbers
var stateNames = map[uint64]string{1: "waiting", 2: "running", 3: "failed", 4: "done", 5: "cancelled"}

// shownJob is a job's entry as an agent reads it, its state by its name
func shownJob(entry wire.JobsEntry) map[string]any {
	if !entry.Found {
		return map[string]any{"found": false}
	}
	shown := map[string]any{
		"found": true, "key": entry.Key, "state": stateNames[entry.State],
		"at": time.UnixMilli(entry.At).UTC(), "attempt": entry.Attempt,
	}
	if json.Valid([]byte(entry.Value)) {
		shown["value"] = json.RawMessage(entry.Value)
	}
	if entry.Ahead != 0 {
		shown["ahead"] = entry.Ahead
	}
	if entry.Progress != "" && json.Valid([]byte(entry.Progress)) {
		shown["progress"] = json.RawMessage(entry.Progress)
	}
	if entry.Err != "" {
		shown["error"] = entry.Err
	}
	if entry.Repeat != "" {
		shown["repeat"] = entry.Repeat
	}
	if entry.Ran != 0 {
		took := time.Duration(min(entry.Took, math.MaxInt64/uint64(time.Millisecond))) * time.Millisecond
		shown["ran"], shown["took"] = time.UnixMilli(entry.Ran).UTC(), took.String()
	}
	return shown
}

func runSQLQuery(ctx context.Context, a *agent, raw json.RawMessage) (any, error) {
	asked, err := arguments[struct {
		Database, SQL string
		Args          []any
	}](raw)
	if err != nil {
		return nil, err
	}
	conn, err := a.store(ctx)
	if err != nil {
		return nil, err
	}
	handle, err := a.open(ctx, conn, wire.SQLOpen, "sql "+asked.Database, wire.SQLDatabase{Name: asked.Database})
	if err != nil {
		return nil, fmt.Errorf("%w; a database opens here once the application has opened it through the "+
			"store's server", err)
	}
	body, err := conn.Call(ctx, wire.SQLBatch, wire.SQLStatements{
		Handle: handle, Read: true,
		Statements: []wire.SQLStatement{{SQL: asked.SQL, Args: sqlArgs(asked.Args), Rows: true}},
	})
	if err != nil {
		return nil, err
	}
	var results wire.SQLResults
	if err = results.Decode(body); err != nil {
		return nil, err
	}
	if len(results.Results) != 1 {
		return nil, errors.New("a batch of one statement answered another number of results")
	}
	result := results.Results[0]
	rows := make([]map[string]any, len(result.Rows))
	for i, row := range result.Rows {
		rows[i] = map[string]any{}
		for j, column := range result.Columns {
			if j < len(row) {
				rows[i][column] = row[j]
			}
		}
	}
	return map[string]any{"columns": result.Columns, "rows": rows}, nil
}

// sqlArgs are a statement's arguments as JSON gave them, its numbers that are
// whole as integers, which SQLite compares with an integer column's
func sqlArgs(given []any) []any {
	args := make([]any, len(given))
	for i, arg := range given {
		if n, ok := arg.(float64); ok && n == float64(int64(n)) {
			arg = int64(n)
		}
		args[i] = arg
	}
	return args
}
