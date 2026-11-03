package tinystore

import (
	"context"
	"encoding/json"
	"time"

	"tinyshed.org/tinystore/internal/core"
)

// A declaration as data is the JSON every SDK sends the core, for settings
// whose shape a program learns only as it runs, such as a plugin's: the
// core checks it, and its value comes back as a map read by its kinds.
//
//	{ "port": { "kind": "integer", "default": 8080, "min": 1 },
//	  "db": { "group": { "url": { "kind": "text", "secret": true, "required": true } } },
//	  "instance": { "entries": {
//	      "plugin": { "kind": "text", "required": true },
//	      "settings": { "chosenBy": "plugin", "groups": { "postgres": { … }, "stripe": { … } } } } } }

// OpenConfigJSON opens a config declared as data, its declaration the JSON
// every SDK sends: each setting with its kind, default, required, secret,
// live and rules, a group, a map of entries, or a group a text setting
// beside it chooses by name. Its value is a map whose secrets are Secret,
// durations time.Duration, integers int64 and numbers float64; layers,
// rules, secrets and Explain are as OpenConfig's. A declaration that means
// nothing is Invalid, naming where.
//
//	config, err := tinystore.OpenConfigJSON(ctx, store, "dashbin", declaration,
//		tinystore.File("dashbin.toml"), tinystore.Env("DASHBIN"))
//	port := config.Get()["instance"].(map[string]any)["aimee"].(map[string]any)["settings"].(map[string]any)["port"]
func OpenConfigJSON(
	ctx context.Context, store *Store, name string, declaration json.RawMessage, opts ...ConfigOption,
) (*Config[map[string]any], error) {
	return openConfig[map[string]any](ctx, store, name, func() (*declared, error) { return declareJSON(declaration) }, opts)
}

// declareJSON reads a declaration as data, which the core checks when the
// config opens.
func declareJSON(declaration json.RawMessage) (*declared, error) {
	parsed, err := exact(declaration)
	group, isMap := parsed.(map[string]any)
	if err != nil || !isMap {
		return nil, core.Errorf(core.Invalid, "the declaration is JSON of a map of settings")
	}
	return &declared{root: dataGroup(group), data: group, dynamic: true}, nil
}

// dataGroup is a group of a declaration as data, as its values are read:
// the core checks the declaration, so a node that does not read here is
// read as any value.
func dataGroup(group map[string]any) *setting {
	node := &setting{kind: kindGroup}
	for key, raw := range group {
		inner := dataNode(raw)
		inner.key = key
		node.group = append(node.group, inner)
	}
	return node
}

func dataNode(raw any) *setting {
	described, _ := raw.(map[string]any)
	if group, ok := described["group"].(map[string]any); ok {
		return dataGroup(group)
	}
	if item, ok := described["entries"].(map[string]any); ok {
		return &setting{kind: kindEntries, item: dataGroup(item)}
	}
	if by, ok := described["chosenBy"].(string); ok {
		groups, _ := described["groups"].(map[string]any)
		node := &setting{kind: kindChosen, by: by, choices: map[string]*setting{}}
		for name, group := range groups {
			fields, _ := group.(map[string]any)
			node.choices[name] = dataGroup(fields)
		}
		return node
	}
	if described["secret"] == true {
		return &setting{kind: kindSecret}
	}
	return dataKind(described["kind"])
}

func dataKind(given any) *setting {
	if text, ok := given.(string); ok {
		switch kind(text) {
		case kindText, kindInteger, kindNumber, kindBoolean, kindDuration, kindObject:
			return &setting{kind: kind(text)}
		}
		return &setting{kind: kindAny}
	}
	list, _ := given.(map[string]any)
	item, ok := list["list"].(map[string]any)
	if !ok {
		return &setting{kind: kindAny}
	}
	if group, ok := item["group"].(map[string]any); ok {
		return &setting{kind: kindList, item: dataGroup(group)}
	}
	return &setting{kind: kindList, item: dataKind(item["kind"])}
}

// dataOf is a value the core sent as a declaration as data reads it: a
// secret a Secret, a duration a time.Duration, an integer an int64, a
// number a float64, and a chosen group the group its chooser names.
func dataOf(node *setting, value any) any {
	switch node.kind {
	case kindGroup:
		fields, _ := value.(map[string]any)
		out := map[string]any{}
		for _, inner := range node.group {
			given, ok := fields[inner.key]
			switch {
			case !ok:
			case given == nil:
				out[inner.key] = nil // a setting declared "default": null, which no layer gave
			case inner.kind == kindChosen:
				if group := inner.picked(fields); group != nil {
					out[inner.key] = dataOf(group, given)
				}
			default:
				out[inner.key] = dataOf(inner, given)
			}
		}
		return out
	case kindEntries:
		each, _ := value.(map[string]any)
		out := make(map[string]any, len(each))
		for key, entry := range each {
			out[key] = dataOf(node.item, entry)
		}
		return out
	case kindSecret:
		text, _ := value.(string)
		return Secret{text: text}
	case kindDuration:
		ms, _ := integer(value)
		return time.Duration(ms) * time.Millisecond
	case kindInteger:
		n, _ := integer(value)
		return n
	case kindNumber:
		n, _ := numberOf(value).Float64()
		return n
	case kindList:
		items, _ := value.([]any)
		out := make([]any, len(items))
		for i, each := range items {
			out[i] = dataOf(node.item, each)
		}
		return out
	}
	return plainJSON(value)
}

// picked is the group of a chosen group that its chooser in fields names,
// nil for none.
func (s *setting) picked(fields map[string]any) *setting {
	name, _ := fields[s.by].(string)
	return s.choices[name]
}

// plainJSON is a value of any kind as encoding/json reads one: a number a float64.
func plainJSON(value any) any {
	switch v := value.(type) {
	case json.Number:
		n, _ := v.Float64()
		return n
	case map[string]any:
		out := make(map[string]any, len(v))
		for key, inner := range v {
			out[key] = plainJSON(inner)
		}
		return out
	case []any:
		out := make([]any, len(v))
		for i, inner := range v {
			out[i] = plainJSON(inner)
		}
		return out
	}
	return value
}

// wireOfData is a value of a declaration as data as the core keeps it, as
// wireOf is a struct's: a secret its text, a duration its milliseconds.
func wireOfData(node *setting, value any) any {
	switch node.kind {
	case kindGroup:
		fields, _ := value.(map[string]any)
		out := map[string]any{}
		for _, inner := range node.group {
			given, ok := fields[inner.key]
			switch {
			case !ok:
			case inner.kind == kindChosen:
				if group := inner.picked(fields); group != nil {
					out[inner.key] = wireOfData(group, given)
				}
			default:
				out[inner.key] = wireOfData(inner, given)
			}
		}
		return out
	case kindEntries:
		each, _ := value.(map[string]any)
		out := make(map[string]any, len(each))
		for key, entry := range each {
			out[key] = wireOfData(node.item, entry)
		}
		return out
	case kindSecret:
		if secret, ok := value.(Secret); ok {
			return secret.text
		}
	case kindDuration:
		if span, ok := value.(time.Duration); ok {
			return span.Milliseconds()
		}
	case kindList:
		items, _ := value.([]any)
		out := make([]any, len(items))
		for i, each := range items {
			out[i] = wireOfData(node.item, each)
		}
		return out
	}
	return value
}

// changesOfData is what a change of a value of a declaration as data made
// of before, as changesOf is a struct's: each setting whose value it changed,
// a map of entries or a chosen group whole, which the core refuses.
func changesOfData(node *setting, before, after map[string]any) map[string]any {
	then, _ := wireOfData(node, before).(map[string]any)
	next, _ := wireOfData(node, after).(map[string]any)
	return changedIn(node, then, next)
}

// changedIn is each setting of a group whose value as the core keeps it
// differs between then and next.
func changedIn(node *setting, then, next map[string]any) map[string]any {
	out := map[string]any{}
	for _, inner := range node.group {
		if inner.kind == kindGroup {
			was, _ := then[inner.key].(map[string]any)
			now, _ := next[inner.key].(map[string]any)
			if changed := changedIn(inner, was, now); changed != nil {
				out[inner.key] = changed
			}
			continue
		}
		if !sameJSON(then[inner.key], next[inner.key]) {
			out[inner.key] = next[inner.key]
		}
	}
	if len(out) == 0 {
		return nil
	}
	return out
}
