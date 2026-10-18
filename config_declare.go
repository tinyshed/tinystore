package tinystore

import (
	"bytes"
	"encoding/json"
	"math"
	"reflect"
	"strconv"
	"strings"
	"time"

	"github.com/tinyshed/tinystore/internal/core"
)

// A declaration is the settings' type read through reflection: each field a
// setting or a group, its key its json name, its default the defaults'
// value, and its tag config's marks and rules:
//
//	Port    int           `json:"port" config:"min=1,max=65535"`
//	Level   string        `json:"level" config:"oneof=info debug warn error"`
//	Hosts   []string      `json:"hosts" config:"items=1-20"`
//	Workers int           `json:"workers" config:"required,min=1"`
//	RPS     int           `json:"rps" config:"live,min=0"`
//
// The core reads it as data, and a value comes back as JSON in the same
// shape, a span its milliseconds and a secret its text.

// kind is how the core keeps a setting's value.
type kind string

const (
	kindText     kind = "text"
	kindInteger  kind = "integer"
	kindNumber   kind = "number"
	kindBoolean  kind = "boolean"
	kindDuration kind = "duration"
	kindObject   kind = "object"
	kindAny      kind = "any"
	kindList     kind = "list"
	kindGroup    kind = "group"
	kindSecret   kind = "secret"
)

var (
	durationType = reflect.TypeFor[time.Duration]()
	secretType   = reflect.TypeFor[Secret]()
)

// setting is a node of a declaration: a group's settings by their fields,
// or one setting's kind, or a list's item.
type setting struct {
	key   string
	index []int // the field's index in its struct, through embedded ones
	kind  kind
	typ   reflect.Type
	group []*setting // a group's, or a list item's group's
	item  *setting   // a list's
}

// declared is a declaration as the core reads it and what reads its values back.
type declared struct {
	root *setting
	data map[string]any
}

// declare reads the declaration of T, its defaults those given.
func declare(defaults reflect.Value) (*declared, error) {
	t := defaults.Type()
	if t.Kind() != reflect.Struct {
		return nil, core.Errorf(core.Invalid, "the declaration is a struct of settings, not %s", t)
	}
	root := &setting{kind: kindGroup, typ: t}
	data, err := declareGroup(root, defaults, "", false)
	if err != nil {
		return nil, err
	}
	return &declared{root: root, data: data}, nil
}

// declareGroup reads a struct's fields into its group; item says it is a
// list's item, whose settings have no default but their zero value.
func declareGroup(group *setting, defaults reflect.Value, path string, item bool) (map[string]any, error) {
	out := map[string]any{}
	err := eachField(group.typ, nil, func(field reflect.StructField, index []int) error {
		key, skip := jsonKey(field)
		if skip {
			return nil
		}
		at := joinPath(path, key)
		var given reflect.Value
		if defaults.IsValid() {
			given = defaults.FieldByIndex(index)
		} else {
			given = reflect.Zero(field.Type)
		}
		node, data, err := declareSetting(field, given, at, item)
		if err != nil {
			return err
		}
		node.key, node.index = key, index
		if _, twice := out[key]; twice {
			return core.Errorf(core.Invalid, "%s: two fields name one setting", at)
		}
		group.group = append(group.group, node)
		out[key] = data
		return nil
	})
	return out, err
}

// eachField calls fn for each field a setting may be, an embedded struct's
// fields as its own, as encoding/json reads them.
func eachField(t reflect.Type, outer []int, fn func(reflect.StructField, []int) error) error {
	for i := range t.NumField() {
		field := t.Field(i)
		index := append(append([]int(nil), outer...), i)
		if field.Anonymous && field.Type.Kind() == reflect.Struct && field.Tag.Get("json") == "" {
			if err := eachField(field.Type, index, fn); err != nil {
				return err
			}
			continue
		}
		if !field.IsExported() {
			continue
		}
		if err := fn(field, index); err != nil {
			return err
		}
	}
	return nil
}

func jsonKey(field reflect.StructField) (key string, skip bool) {
	name, _, _ := strings.Cut(field.Tag.Get("json"), ",")
	switch name {
	case "-":
		return "", true
	case "":
		return field.Name, false
	}
	return name, false
}

func joinPath(path, key string) string {
	if path == "" {
		return key
	}
	return path + "." + key
}

// marks are what a field's config tag says.
type marks struct {
	live, required bool
	min, max       *float64
	oneOf          []string
	itemsMin       *float64
	itemsMax       *float64
}

func readMarks(tag, path string, spans bool) (marks, error) {
	var m marks
	if tag == "" {
		return m, nil
	}
	for _, part := range strings.Split(tag, ",") {
		name, value, _ := strings.Cut(strings.TrimSpace(part), "=")
		var err error
		switch name {
		case "live":
			m.live = true
		case "required":
			m.required = true
		case "min":
			m.min, err = bound(value, spans, path+": its min")
		case "max":
			m.max, err = bound(value, spans, path+": its max")
		case "oneof":
			m.oneOf = strings.Fields(value)
		case "items":
			low, high, _ := strings.Cut(value, "-")
			if m.itemsMin, err = bound(low, false, path+": its items"); err == nil {
				m.itemsMax, err = bound(high, false, path+": its items")
			}
		case "":
		default:
			err = core.Errorf(core.Invalid, "%s: config:%q: %q is not live, required, min, max, oneof or items", path, tag, name)
		}
		if err != nil {
			return m, err
		}
	}
	return m, nil
}

// bound is a rule's number, or a span's milliseconds; "" is none.
func bound(text string, spans bool, what string) (*float64, error) {
	if text == "" {
		return nil, nil
	}
	if spans {
		ms, err := core.ParseSpan(text, what)
		if err != nil {
			return nil, err
		}
		n := float64(ms)
		return &n, nil
	}
	n, err := strconv.ParseFloat(text, 64)
	if err != nil {
		return nil, core.Errorf(core.Invalid, "%s %q: a number", what, text)
	}
	return &n, nil
}

// declareSetting reads one field: a group of settings, or a setting as the core reads it.
func declareSetting(field reflect.StructField, given reflect.Value, path string, item bool) (*setting, map[string]any, error) {
	t := field.Type
	spans := t == durationType || t.Kind() == reflect.Slice && t.Elem() == durationType
	m, err := readMarks(field.Tag.Get("config"), path, spans)
	if err != nil {
		return nil, nil, err
	}
	node := &setting{typ: t}
	if t.Kind() == reflect.Struct && t != secretType {
		if m.live || m.min != nil || m.max != nil || m.oneOf != nil || m.itemsMin != nil || m.itemsMax != nil {
			return nil, nil, core.Errorf(core.Invalid, "%s: a group takes no rule; its settings do", path)
		}
		node.kind = kindGroup
		var defaults reflect.Value
		if !item {
			defaults = given
		}
		group, err := declareGroup(node, defaults, path, item)
		return node, map[string]any{"group": group}, err
	}
	data := map[string]any{}
	if err := describe(node, data, t, m, path); err != nil {
		return nil, nil, err
	}
	switch {
	case m.live && (item || node.kind == kindSecret):
		return nil, nil, core.Errorf(core.Invalid, "%s: a secret is never live, nor a list's item", path)
	case m.live:
		data["live"] = true
	}
	if m.required {
		data["required"] = true
	} else {
		data["default"] = wireOf(node, given)
	}
	return node, data, nil
}

// describe writes a setting's kind and rules as the core reads them.
func describe(node *setting, data map[string]any, t reflect.Type, m marks, path string) error {
	rules := func(at map[string]any) {
		if m.min != nil {
			at["min"] = *m.min
		}
		if m.max != nil {
			at["max"] = *m.max
		}
		if m.oneOf != nil {
			at["oneOf"] = m.oneOf
		}
	}
	switch {
	case t == secretType:
		node.kind = kindSecret
		data["kind"], data["secret"] = kindText, true
		rules(data)
		return nil
	case t == durationType:
		node.kind = kindDuration
	case t.Kind() == reflect.Slice && t.Elem().Kind() != reflect.Uint8:
		node.kind = kindList
		item, err := describeItem(t.Elem(), m, path+"[]")
		if err != nil {
			return err
		}
		node.item = item.node
		data["kind"] = map[string]any{"list": item.data}
		if m.itemsMin != nil {
			data["min"] = *m.itemsMin
		}
		if m.itemsMax != nil {
			data["max"] = *m.itemsMax
		}
		return nil
	default:
		k, err := kindOf(t, path)
		if err != nil {
			return err
		}
		node.kind = k
		low, high := implied(t)
		m.min, m.max = tighter(m.min, low, math.Max), tighter(m.max, high, math.Min)
	}
	if m.itemsMin != nil || m.itemsMax != nil {
		return core.Errorf(core.Invalid, "%s: items bounds a list", path)
	}
	data["kind"] = node.kind
	rules(data)
	return nil
}

type itemOf struct {
	node *setting
	data map[string]any
}

// describeItem is a list's item: a group, or a value with the field's rules.
func describeItem(t reflect.Type, m marks, path string) (itemOf, error) {
	node := &setting{typ: t}
	if t.Kind() == reflect.Struct && t != secretType && t != durationType {
		node.kind = kindGroup
		group, err := declareGroup(node, reflect.Value{}, path, true)
		return itemOf{node, map[string]any{"group": group}}, err
	}
	if t == secretType || t.Kind() == reflect.Slice && t.Elem().Kind() != reflect.Uint8 {
		return itemOf{}, core.Errorf(core.Invalid, "%s: a list's items are a value or a group, never a secret or a list", path)
	}
	data := map[string]any{}
	err := describe(node, data, t, marks{min: m.min, max: m.max, oneOf: m.oneOf}, path)
	return itemOf{node, data}, err
}

// kindOf is how the core keeps a value of a Go type.
func kindOf(t reflect.Type, path string) (kind, error) {
	switch t.Kind() {
	case reflect.Bool:
		return kindBoolean, nil
	case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64,
		reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64:
		return kindInteger, nil
	case reflect.Float32, reflect.Float64:
		return kindNumber, nil
	case reflect.String:
		return kindText, nil
	case reflect.Map:
		if t.Key().Kind() == reflect.String {
			return kindObject, nil
		}
	case reflect.Interface:
		if t.NumMethod() == 0 {
			return kindAny, nil
		}
	case reflect.Pointer:
		return "", core.Errorf(core.Invalid, "%s: %s: a setting has one type; one a layer may leave out has a default", path, t)
	}
	return "", core.Errorf(core.Invalid, "%s: %s is no type a config keeps", path, t)
}

// implied is the range a sized or unsigned integer holds, which a value
// past it could not be read into.
func implied(t reflect.Type) (low, high *float64) {
	n := func(v float64) *float64 { return &v }
	switch t.Kind() {
	case reflect.Int8, reflect.Int16, reflect.Int32, reflect.Uint8, reflect.Uint16, reflect.Uint32:
	case reflect.Uint, reflect.Uint64:
		return n(0), nil
	default:
		return nil, nil
	}
	bits := t.Bits()
	switch t.Kind() {
	case reflect.Int8, reflect.Int16, reflect.Int32:
		return n(-math.Pow(2, float64(bits-1))), n(math.Pow(2, float64(bits-1)) - 1)
	}
	return n(0), n(math.Pow(2, float64(bits)) - 1)
}

// tighter is the bound of the two that holds less, pick choosing it.
func tighter(given, implied *float64, pick func(a, b float64) float64) *float64 {
	switch {
	case given == nil:
		return implied
	case implied == nil:
		return given
	}
	v := pick(*given, *implied)
	return &v
}

// wireOf is a value as the core keeps it: a span its milliseconds, a secret
// its text, a group by its settings' keys.
func wireOf(node *setting, v reflect.Value) any {
	switch node.kind {
	case kindGroup:
		out := map[string]any{}
		for _, inner := range node.group {
			out[inner.key] = wireOf(inner, v.FieldByIndex(inner.index))
		}
		return out
	case kindDuration:
		return v.Int() / int64(time.Millisecond)
	case kindSecret:
		return v.Interface().(Secret).text
	case kindInteger:
		if v.CanUint() {
			return v.Uint()
		}
		return v.Int()
	case kindNumber:
		return v.Float()
	case kindBoolean:
		return v.Bool()
	case kindText:
		return v.String()
	case kindList:
		out := make([]any, v.Len())
		for i := range out {
			out[i] = wireOf(node.item, v.Index(i))
		}
		return out
	}
	// an object or anything else, as encoding/json writes it
	if node.kind == kindObject && v.IsNil() {
		return map[string]any{}
	}
	text, err := json.Marshal(v.Interface())
	if err != nil {
		return nil
	}
	value, _ := exact(text)
	return value
}

// exact is JSON decoded with its numbers as they were written.
func exact(text []byte) (any, error) {
	decoder := json.NewDecoder(bytes.NewReader(text))
	decoder.UseNumber()
	var value any
	err := decoder.Decode(&value)
	return value, err
}

// read sets v from a value the core sent, in the shape wireOf writes.
func read(node *setting, value any, v reflect.Value) error {
	if value == nil {
		return nil
	}
	switch node.kind {
	case kindGroup:
		fields, _ := value.(map[string]any)
		for _, inner := range node.group {
			if err := read(inner, fields[inner.key], v.FieldByIndex(inner.index)); err != nil {
				return err
			}
		}
		return nil
	case kindSecret:
		text, _ := value.(string)
		v.Set(reflect.ValueOf(Secret{text: text}))
		return nil
	case kindDuration:
		ms, err := integer(value)
		v.SetInt(ms * int64(time.Millisecond))
		return err
	case kindInteger:
		if v.CanUint() {
			n, err := strconv.ParseUint(string(numberOf(value)), 10, 64)
			v.SetUint(n)
			return err
		}
		n, err := integer(value)
		v.SetInt(n)
		return err
	case kindNumber:
		n, err := numberOf(value).Float64()
		v.SetFloat(n)
		return err
	case kindBoolean:
		b, _ := value.(bool)
		v.SetBool(b)
		return nil
	case kindText:
		text, _ := value.(string)
		v.SetString(text)
		return nil
	case kindList:
		items, _ := value.([]any)
		list := reflect.MakeSlice(v.Type(), len(items), len(items))
		for i, each := range items {
			if err := read(node.item, each, list.Index(i)); err != nil {
				return err
			}
		}
		v.Set(list)
		return nil
	}
	text, err := json.Marshal(value)
	if err != nil {
		return err
	}
	return json.Unmarshal(text, v.Addr().Interface())
}

func numberOf(value any) json.Number {
	if n, ok := value.(json.Number); ok {
		return n
	}
	return "0"
}

func integer(value any) (int64, error) {
	return strconv.ParseInt(string(numberOf(value)), 10, 64)
}

// valueOf is T as a value the core sent reads.
func valueOf[T any](d *declared, text json.RawMessage) (T, error) {
	var out T
	value, err := exact(text)
	if err != nil {
		return out, core.Wrap(core.Protocol, err, "a config's value that does not read")
	}
	if err := read(d.root, value, reflect.ValueOf(&out).Elem()); err != nil {
		return out, core.Wrap(core.Protocol, err, "a config's value that does not read as its declaration")
	}
	return out, nil
}

// changesOf is what a change made of before, as the core takes a change: each
// setting whose value it changed, in the declaration's shape, none for none.
func changesOf(node *setting, before, after reflect.Value) map[string]any {
	out := map[string]any{}
	for _, inner := range node.group {
		was, now := before.FieldByIndex(inner.index), after.FieldByIndex(inner.index)
		if inner.kind == kindGroup {
			if changed := changesOf(inner, was, now); changed != nil {
				out[inner.key] = changed
			}
			continue
		}
		then, next := wireOf(inner, was), wireOf(inner, now)
		if !sameJSON(then, next) {
			out[inner.key] = next
		}
	}
	if len(out) == 0 {
		return nil
	}
	return out
}

func sameJSON(a, b any) bool {
	left, err := json.Marshal(a)
	if err != nil {
		return false
	}
	right, err := json.Marshal(b)
	return err == nil && bytes.Equal(left, right)
}

// copied is a copy of v that shares nothing a change of it could reach.
func copied[T any](d *declared, v T) T {
	out, err := valueOf[T](d, mustJSON(wireOf(d.root, reflect.ValueOf(v))))
	if err != nil {
		return v
	}
	return out
}

func mustJSON(value any) json.RawMessage {
	text, _ := json.Marshal(value)
	return text
}
