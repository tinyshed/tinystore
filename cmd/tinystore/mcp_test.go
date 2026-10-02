package main

import (
	"bytes"
	"encoding/json"
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"

	"github.com/tinyshed/tinystore/server/reach"
	"github.com/tinyshed/tinystore/server/wire"
)

// mcpSession sends an MCP client's lines to mcp and reads what it answered,
// an answer a request
func mcpSession(t *testing.T, dir string, requests ...string) map[string]json.RawMessage {
	t.Helper()
	var out, stderr bytes.Buffer
	if err := mcp(t.Context(), []string{dir}, strings.NewReader(strings.Join(requests, "\n")), &out, &stderr); err != nil {
		t.Fatal(err, stderr.String())
	}
	answers := map[string]json.RawMessage{}
	for line := range strings.Lines(out.String()) {
		var reply struct {
			JSONRPC string          `json:"jsonrpc"`
			ID      json.RawMessage `json:"id"`
			Result  json.RawMessage `json:"result"`
			Error   json.RawMessage `json:"error"`
		}
		if err := json.Unmarshal([]byte(line), &reply); err != nil || reply.JSONRPC != "2.0" {
			t.Fatalf("a reply that is not JSON-RPC: %q", line)
		}
		answers[string(reply.ID)] = reply.Result
		if reply.Error != nil {
			answers[string(reply.ID)] = reply.Error
		}
	}
	return answers
}

// toolAnswer is what a tools/call answered: its text, and whether it failed
func toolAnswer(t *testing.T, result json.RawMessage) (string, bool) {
	t.Helper()
	var called struct {
		Content []struct{ Type, Text string } `json:"content"`
		IsError bool                          `json:"isError"`
	}
	if err := json.Unmarshal(result, &called); err != nil || len(called.Content) != 1 {
		t.Fatalf("a tool's answer: %s", result)
	}
	return called.Content[0].Text, called.IsError
}

func callTool(id int, name, arguments string) string {
	return `{"jsonrpc":"2.0","id":` + strconv.Itoa(id) + `,"method":"tools/call","params":{"name":"` + name +
		`","arguments":` + arguments + `}}`
}

// an MCP client is answered as the protocol says, a notification with nothing,
// and its tools read what the application wrote: logs, kv, jobs and SQL, from
// a snapshot that writes nothing
func TestAnAgentReadsTheStoreOverMCP(t *testing.T) {
	dir, conn := servedStore(t)
	appendLines(t, conn, "api", "info", "started", "error", "GET /users/0 500")
	sessions := mustOpen(t, conn, wire.KVOpen, wire.KVBucket{Name: "sessions"})
	mustCall(t, conn, wire.KVSet, wire.KVCall{
		Handle: sessions, Owners: []string{"42"}, Key: "t1",
		Value: wire.KVValue{Kind: wire.KVBytes, Bytes: []byte(`{"device":"phone"}`)},
	})
	videos := mustOpen(t, conn, wire.JobsOpen, wire.JobsQueue{Name: "videos"})
	mustCall(t, conn, wire.JobsEnqueue, wire.JobsBatch{Handle: videos, Jobs: []wire.JobsJob{
		{Value: `{"video":7}`, Key: "v7"}, {Value: `{"video":8}`, Key: "v8"},
	}})
	notes := mustOpen(t, conn, wire.SQLOpen, wire.SQLDatabase{Name: "app", Migrations: []wire.SQLMigration{
		{Name: "0001_notes.sql", Text: "create table notes (id integer primary key, body text not null);"},
	}})
	mustCall(t, conn, wire.SQLExec, wire.SQLStatement{
		Handle: notes, SQL: "insert into notes (body) values (?)",
		Args: []any{"buy milk"},
	})

	answers := mcpSession(t, dir,
		`{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},`+
			`"clientInfo":{"name":"test","version":"1"}}}`,
		`{"jsonrpc":"2.0","method":"notifications/initialized"}`,
		`{"jsonrpc":"2.0","id":2,"method":"tools/list"}`,
		callTool(3, "logs", `{"level":"error"}`),
		callTool(4, "kv_get", `{"bucket":"sessions","owners":["42"],"key":"t1"}`),
		callTool(5, "jobs_get", `{"queue":"videos","key":"v8"}`),
		callTool(6, "sql_query", `{"database":"app","sql":"select id, body from notes where id = ?","args":[1]}`),
		callTool(7, "sql_query", `{"database":"app","sql":"delete from notes"}`),
		callTool(8, "status", `{}`),
		`{"jsonrpc":"2.0","id":9,"method":"ping"}`,
		`{"jsonrpc":"2.0","id":10,"method":"no/such"}`,
	)
	if len(answers) != 10 {
		t.Fatalf("%d answers to ten requests and a notification: %v", len(answers), answers)
	}
	if !strings.Contains(string(answers["1"]), `"protocolVersion":"2025-06-18"`) ||
		!strings.Contains(string(answers["1"]), `"tools":{}`) {
		t.Fatalf("initialize: %s", answers["1"])
	}
	for _, tool := range []string{"status", "logs", "kv_get", "kv_scan", "jobs_get", "jobs_scan", "sql_query"} {
		if !strings.Contains(string(answers["2"]), `"name":"`+tool+`"`) {
			t.Fatalf("tools/list lacks %s: %s", tool, answers["2"])
		}
	}
	expectTool(t, answers["3"], false, `"msg": "GET /users/0 500"`, "started")
	expectTool(t, answers["4"], false, `"device": "phone"`, "")
	expectTool(t, answers["5"], false, `"state": "waiting"`, "")
	expectTool(t, answers["5"], false, `"ahead": 1`, "")
	expectTool(t, answers["6"], false, `"body": "buy milk"`, "")
	expectTool(t, answers["7"], true, "", "")
	expectTool(t, answers["8"], false, `"answers": true`, "")
	if string(answers["9"]) != "{}" || !strings.Contains(string(answers["10"]), "-32601") {
		t.Fatalf("ping %s, an unknown method %s", answers["9"], answers["10"])
	}

	body, err := conn.Call(t.Context(), wire.SQLBatch, wire.SQLStatements{
		Handle: notes, Read: true,
		Statements: []wire.SQLStatement{{SQL: "select count(*) from notes", Rows: true}},
	})
	var results wire.SQLResults
	if err == nil {
		err = results.Decode(body)
	}
	if err != nil || len(results.Results) != 1 || results.Results[0].Rows[0][0] != int64(1) {
		t.Fatalf("the agent's delete wrote: %+v, %v", results, err)
	}
}

// an agent's read of an engine the store does not keep is refused, and makes
// no file for it
func TestAnAgentMakesNoEnginesFile(t *testing.T) {
	dir, _ := servedStore(t)
	answers := mcpSession(t, dir,
		callTool(1, "kv_get", `{"bucket":"sessions","key":"t1"}`),
		callTool(2, "jobs_scan", `{"queue":"videos"}`),
		callTool(3, "logs", `{}`),
	)
	for id, file := range map[string]string{"1": "kv.db", "2": "jobs.db", "3": "records.db"} {
		expectTool(t, answers[id], true, file+" is not in", "")
		if _, err := os.Stat(filepath.Join(dir, file)); !errors.Is(err, fs.ErrNotExist) {
			t.Fatalf("an agent's read made %s: %v", file, err)
		}
	}
}

// expectTool checks a tool's answer: whether it failed, a text it holds and
// one it does not
func expectTool(t *testing.T, result json.RawMessage, failed bool, holds, lacks string) {
	t.Helper()
	text, isError := toolAnswer(t, result)
	if isError != failed || !strings.Contains(text, holds) || lacks != "" && strings.Contains(text, lacks) {
		t.Fatalf("a tool answered (error %v):\n%s\nwant error %v, holding %q and not %q", isError, text, failed,
			holds, lacks)
	}
}

func mustOpen(t *testing.T, conn *reach.Conn, method wire.Method, open reach.Message) uint64 {
	t.Helper()
	var handle wire.Handle
	if err := handle.Decode(mustCall(t, conn, method, open)); err != nil {
		t.Fatal(err)
	}
	return handle.Handle
}

func mustCall(t *testing.T, conn *reach.Conn, method wire.Method, request reach.Message) []byte {
	t.Helper()
	body, err := conn.Call(t.Context(), method, request)
	if err != nil {
		t.Fatal(err)
	}
	return body
}
