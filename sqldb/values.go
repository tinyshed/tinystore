package sqldb

import (
	"database/sql"
	"database/sql/driver"
	"encoding/hex"
	"errors"
	"fmt"
	"math"
	"reflect"
	"strings"
	"sync"
	"time"

	"github.com/tinyshed/tinystore"
)

// kind is what a Go type is to sqldb: what its column stores, and what a value
// read back must be to decode into it
type kind int

const (
	kindText kind = iota + 1
	kindBytes
	kindBool
	kindInteger
	kindReal
	kindTime
	kindDuration
	kindUUID
	kindArray // [N]byte of a type sqldb does not know by name
	kindDate
	kindJSON
	kindCustom // a type with its own Scan and Value
)

var kindNames = [...]string{
	kindText: "text", kindBytes: "bytes", kindBool: "bool", kindInteger: "integer", kindReal: "real",
	kindTime: "time", kindDuration: "duration", kindUUID: "uuid", kindArray: "bytes", kindDate: "date",
	kindJSON: "json", kindCustom: "custom",
}

func (k kind) String() string { return kindNames[k] }

// storage is the STRICT column a kind makes; a custom type's is what Storage says
func (k kind) storage() StorageClass {
	switch k {
	case kindText, kindUUID, kindDate, kindJSON:
		return Text
	case kindBytes, kindArray:
		return Blob
	case kindBool, kindInteger, kindTime, kindDuration:
		return Integer
	case kindReal:
		return Real
	}
	return 0
}

// wrapping is what makes a type nullable around its kind
type wrapping int

const (
	bare     wrapping = iota
	pointer           // *T: nil is NULL
	nullable          // sql.Null[T] and sql.NullString's kin: Valid false is NULL
)

// valueType is a Go type as sqldb stores it: its kind once a pointer or a
// sql.Null around it is taken off
type valueType struct {
	kind   kind
	wrap   wrapping
	inner  reflect.Type // the type inside the wrapping
	length int          // N of a [N]byte
}

var (
	timeType     = reflect.TypeFor[time.Time]()
	durationType = reflect.TypeFor[time.Duration]()
	dateType     = reflect.TypeFor[Date]()
	scannerType  = reflect.TypeFor[sql.Scanner]()
	valuerType   = reflect.TypeFor[driver.Valuer]()
	jsonType     = reflect.TypeFor[jsonValue]()
)

// uuidPackages are those whose UUID sqldb knows by name, without importing
// either. Sixteen bytes of any other type are bytes, since reflection cannot
// tell a uuid from an MD5.
var uuidPackages = map[string]bool{
	"github.com/google/uuid":   true,
	"github.com/gofrs/uuid":    true,
	"github.com/gofrs/uuid/v5": true,
}

var classified sync.Map

type classification struct {
	value valueType
	err   error
}

func classify(t reflect.Type) (valueType, error) {
	if cached, ok := classified.Load(t); ok {
		if found, isKnown := cached.(classification); isKnown {
			return found.value, found.err
		}
	}
	value, err := classifyType(t)
	classified.Store(t, classification{value: value, err: err})
	return value, err
}

func classifyType(t reflect.Type) (valueType, error) {
	wrap, inner := unwrap(t)
	if twice, _ := unwrap(inner); wrap != bare && twice != bare {
		return valueType{}, fmt.Errorf("%s is nullable twice; one pointer or sql.Null makes it nullable", t)
	}
	k, length, err := kindOf(inner)
	if err != nil {
		return valueType{}, err
	}
	return valueType{kind: k, wrap: wrap, inner: inner, length: length}, nil
}

func unwrap(t reflect.Type) (wrapping, reflect.Type) {
	if t.Kind() == reflect.Pointer {
		return pointer, t.Elem()
	}
	if isNull(t) {
		return nullable, t.Field(0).Type
	}
	return bare, t
}

// isNull is sql.Null[T], or sql.NullString and the others that came before
// it: a value, then Valid
func isNull(t reflect.Type) bool {
	return t.Kind() == reflect.Struct && t.PkgPath() == "database/sql" && strings.HasPrefix(t.Name(), "Null") &&
		t.NumField() == 2 && t.Field(1).Name == "Valid" && t.Field(1).Type.Kind() == reflect.Bool
}

func kindOf(t reflect.Type) (kind, int, error) {
	switch {
	case t == timeType:
		return kindTime, 0, nil
	case t == durationType:
		return kindDuration, 0, nil
	case t == dateType:
		return kindDate, 0, nil
	case t.Implements(jsonType):
		return kindJSON, 0, nil
	case knownUUID(t):
		return kindUUID, 16, nil
	case reflect.PointerTo(t).Implements(scannerType), reflect.PointerTo(t).Implements(valuerType):
		return kindCustom, 0, nil
	}
	switch t.Kind() {
	case reflect.String:
		return kindText, 0, nil
	case reflect.Bool:
		return kindBool, 0, nil
	case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64,
		reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64:
		return kindInteger, 0, nil
	case reflect.Float32, reflect.Float64:
		return kindReal, 0, nil
	case reflect.Slice:
		if t.Elem().Kind() == reflect.Uint8 {
			return kindBytes, 0, nil
		}
	case reflect.Array:
		if t.Elem().Kind() == reflect.Uint8 {
			return kindArray, t.Len(), nil
		}
	default:
	}
	if t.Kind() == reflect.Struct || t.Kind() == reflect.Map || t.Kind() == reflect.Slice || t.Kind() == reflect.Array {
		return 0, 0, fmt.Errorf("sqldb does not store %s; sqldb.JSON[%s] keeps it as JSON", t, t)
	}
	return 0, 0, fmt.Errorf("sqldb does not store %s", t)
}

func knownUUID(t reflect.Type) bool {
	return t.Kind() == reflect.Array && t.Len() == 16 && t.Elem().Kind() == reflect.Uint8 &&
		t.Name() == "UUID" && uuidPackages[t.PkgPath()]
}

var (
	errMismatch = errors.New("")
	errNull     = errors.New("")
)

// decode sets v, a field or a whole value, from what its column returned; a
// custom type is handed NULL as database/sql hands it, to decide itself
func (vt valueType) decode(v reflect.Value, raw any) error {
	switch {
	case raw == nil && vt.wrap == bare && vt.kind != kindCustom:
		return errNull
	case raw == nil && vt.wrap != bare:
		v.SetZero()
		return nil
	case vt.wrap == pointer:
		if v.IsNil() {
			v.Set(reflect.New(vt.inner))
		}
		return vt.decodeInner(v.Elem(), raw)
	case vt.wrap == nullable:
		if err := vt.decodeInner(v.Field(0), raw); err != nil {
			return err
		}
		v.Field(1).SetBool(true)
		return nil
	}
	return vt.decodeInner(v, raw)
}

func (vt valueType) decodeInner(v reflect.Value, raw any) error {
	switch vt.kind {
	case kindText:
		text, ok := raw.(string)
		if !ok {
			return errMismatch
		}
		v.SetString(text)
	case kindBytes:
		return setBytes(v, raw)
	case kindBool:
		n, ok := raw.(int64)
		if !ok || n < 0 || n > 1 {
			return errMismatch
		}
		v.SetBool(n == 1)
	case kindInteger:
		return setInteger(v, raw)
	case kindReal:
		return setReal(v, raw)
	case kindTime:
		return setTime(v, raw)
	case kindDuration:
		n, ok := raw.(int64)
		if !ok {
			return errMismatch
		}
		if n > math.MaxInt64/int64(time.Millisecond) || n < math.MinInt64/int64(time.Millisecond) {
			return errors.New("past time.Duration's range")
		}
		v.SetInt(n * int64(time.Millisecond))
	case kindUUID:
		return setUUID(v, raw)
	case kindArray:
		data, ok := raw.([]byte)
		if !ok || len(data) != vt.length {
			return errMismatch
		}
		copy(v.Bytes(), data)
	case kindDate:
		return setDate(v, raw)
	case kindJSON:
		text, ok := raw.(string)
		if !ok {
			return errMismatch
		}
		target, isJSON := v.Addr().Interface().(jsonTarget)
		if !isJSON {
			return errMismatch
		}
		return target.readJSON([]byte(text))
	case kindCustom:
		scanner, ok := v.Addr().Interface().(sql.Scanner)
		if !ok {
			return fmt.Errorf("%s has no Scan method", v.Type())
		}
		return scanner.Scan(raw)
	}
	return nil
}

func setBytes(v reflect.Value, raw any) error {
	switch data := raw.(type) {
	case []byte:
		v.SetBytes(data)
	case string:
		v.SetBytes([]byte(data))
	default:
		return errMismatch
	}
	return nil
}

func setInteger(v reflect.Value, raw any) error {
	n, ok := raw.(int64)
	if !ok {
		return errMismatch
	}
	if v.CanInt() {
		if v.OverflowInt(n) {
			return fmt.Errorf("past %s's range", v.Type())
		}
		v.SetInt(n)
		return nil
	}
	if n < 0 || v.OverflowUint(uint64(n)) {
		return fmt.Errorf("past %s's range", v.Type())
	}
	v.SetUint(uint64(n))
	return nil
}

func setReal(v reflect.Value, raw any) error {
	var value float64
	switch n := raw.(type) {
	case float64:
		value = n
	case int64:
		value = float64(n)
	default:
		return errMismatch
	}
	if v.Kind() == reflect.Float32 && !math.IsInf(value, 0) &&
		(value > math.MaxFloat32 || value < -math.MaxFloat32) {
		return fmt.Errorf("past %s's range", v.Type())
	}
	v.SetFloat(value)
	return nil
}

// textTimes are the ways SQLite's date functions and RFC 3339 spell a time; a
// time without a zone is UTC
var textTimes = []string{
	"2006-01-02 15:04:05.999999999", "2006-01-02T15:04:05.999999999",
	"2006-01-02 15:04:05.999999999Z07:00", time.RFC3339Nano,
	"2006-01-02 15:04", "2006-01-02T15:04", time.DateOnly,
}

func setTime(v reflect.Value, raw any) error {
	var at time.Time
	switch value := raw.(type) {
	case int64:
		at = time.UnixMilli(value).UTC()
	case time.Time:
		at = value.UTC()
	case string:
		parsed, err := parseTime(value)
		if err != nil {
			return err
		}
		at = parsed
	default:
		return errMismatch
	}
	target, isTime := v.Addr().Interface().(*time.Time)
	if !isTime {
		return errMismatch
	}
	*target = at
	return nil
}

func parseTime(text string) (time.Time, error) {
	for _, layout := range textTimes {
		if at, err := time.Parse(layout, text); err == nil {
			return at.UTC(), nil
		}
	}
	return time.Time{}, errMismatch
}

func setUUID(v reflect.Value, raw any) error {
	switch value := raw.(type) {
	case string:
		return parseUUID(value, v.Bytes())
	case []byte:
		if len(value) != 16 {
			return errMismatch
		}
		copy(v.Bytes(), value)
		return nil
	}
	return errMismatch
}

// parseUUID reads the 8-4-4-4-12 text of a uuid, in either case, into id
func parseUUID(text string, id []byte) error {
	if len(text) != 36 || text[8] != '-' || text[13] != '-' || text[18] != '-' || text[23] != '-' {
		return errMismatch
	}
	digits := text[0:8] + text[9:13] + text[14:18] + text[19:23] + text[24:36]
	if _, err := hex.Decode(id, []byte(digits)); err != nil {
		return errMismatch
	}
	return nil
}

// uuidText is a uuid's text, lower case:
//
//	01 92 f2 a4 … → "0192f2a4-…"
func uuidText(id []byte) string {
	var text [36]byte
	hex.Encode(text[0:8], id[0:4])
	hex.Encode(text[9:13], id[4:6])
	hex.Encode(text[14:18], id[6:8])
	hex.Encode(text[19:23], id[8:10])
	hex.Encode(text[24:36], id[10:16])
	text[8], text[13], text[18], text[23] = '-', '-', '-', '-'
	return string(text[:])
}

func setDate(v reflect.Value, raw any) error {
	text, ok := raw.(string)
	if !ok {
		return errMismatch
	}
	day, err := parseDate(text)
	if err != nil {
		return errMismatch
	}
	target, isDate := v.Addr().Interface().(*Date)
	if !isDate {
		return errMismatch
	}
	*target = day
	return nil
}

var (
	errNaN       = errors.New("a NaN, which a REAL column stores as NULL")
	errBeyond    = errors.New("past the int64 an INTEGER column holds")
	errNotADay   = errors.New("not a day of the calendar")
	errNotStored = errors.New("")
)

// encode is v as its column holds it; asBytes writes a uuid as its 16 bytes
func (vt valueType) encode(v reflect.Value, asBytes bool) (any, error) {
	switch vt.wrap {
	case pointer:
		if v.IsNil() {
			return nil, nil
		}
		v = v.Elem()
	case nullable:
		if !v.Field(1).Bool() {
			return nil, nil
		}
		v = v.Field(0)
	default:
	}
	return vt.encodeInner(v, asBytes)
}

func (vt valueType) encodeInner(v reflect.Value, asBytes bool) (any, error) {
	switch vt.kind {
	case kindText:
		return v.String(), nil
	case kindBytes:
		if data := v.Bytes(); data != nil {
			return data, nil
		}
		return []byte{}, nil
	case kindBool:
		return integerOf(v.Bool()), nil
	case kindInteger:
		if v.CanInt() {
			return v.Int(), nil
		}
		if n := v.Uint(); n <= math.MaxInt64 {
			return int64(n), nil
		}
		return nil, errBeyond
	case kindReal:
		if n := v.Float(); !math.IsNaN(n) {
			return n, nil
		}
		return nil, errNaN
	case kindTime, kindDate, kindJSON:
		return encodeTyped(v.Interface())
	case kindDuration:
		return time.Duration(v.Int()).Milliseconds(), nil
	case kindUUID:
		if asBytes {
			return arrayBytes(v), nil
		}
		return uuidText(arrayBytes(v)), nil
	case kindArray:
		return arrayBytes(v), nil
	case kindCustom:
		return customValue(v)
	}
	return nil, errNotStored
}

func encodeTyped(value any) (any, error) {
	switch typed := value.(type) {
	case time.Time:
		return typed.UnixMilli(), nil
	case Date:
		return dateText(typed)
	case jsonValue:
		return typed.jsonText()
	}
	return nil, errNotStored
}

func integerOf(b bool) int64 {
	if b {
		return 1
	}
	return 0
}

func arrayBytes(v reflect.Value) []byte {
	out := make([]byte, v.Len())
	reflect.Copy(reflect.ValueOf(out), v)
	return out
}

func dateText(day Date) (string, error) {
	if !day.valid() {
		return "", fmt.Errorf("%w: %s", errNotADay, day)
	}
	return day.String(), nil
}

// customValue is what a custom type's Value writes, by a pointer receiver
// when it has one
func customValue(v reflect.Value) (any, error) {
	valuer, ok := v.Interface().(driver.Valuer)
	if !ok && v.CanAddr() {
		valuer, ok = v.Addr().Interface().(driver.Valuer)
	}
	if !ok {
		return nil, fmt.Errorf("%s has no Value method", v.Type())
	}
	value, err := valuer.Value()
	if err != nil {
		return nil, err
	}
	if n, isReal := value.(float64); isReal && math.IsNaN(n) {
		return nil, errNaN
	}
	return value, nil
}

// encodeArguments writes each argument by its Go type, as its column would
// hold it, and copies args only when one changes, since they are the caller's
func encodeArguments(args []any) ([]any, error) {
	var encoded []any
	for i, arg := range args {
		value, changed, err := encodeArgument(arg)
		if err != nil {
			return nil, fmt.Errorf("%w: argument %d: %w", tinystore.ErrInvalid, i+1, err)
		}
		if changed && encoded == nil {
			encoded = append(make([]any, 0, len(args)), args...)
		}
		if changed {
			encoded[i] = value
		}
	}
	if encoded == nil {
		return args, nil
	}
	return encoded, nil
}

func encodeArgument(arg any) (value any, changed bool, err error) {
	switch v := arg.(type) {
	case nil, string, []byte, int64, int, int32, int16, int8, uint32, uint16, uint8:
		return arg, false, nil
	case float64:
		if math.IsNaN(v) {
			return nil, false, errNaN
		}
		return arg, false, nil
	case bool:
		return integerOf(v), true, nil
	case time.Time:
		return v.UnixMilli(), true, nil
	case time.Duration:
		return v.Milliseconds(), true, nil
	case Date:
		text, err := dateText(v)
		return text, true, err
	case jsonValue:
		text, err := v.jsonText()
		return text, true, err
	case sql.NamedArg:
		inner, _, err := encodeArgument(v.Value)
		return sql.Named(v.Name, inner), true, err
	}
	return encodeByType(arg)
}

// encodeByType writes an argument whose type the switch above does not name:
// a named type, a pointer, a sql.Null, a uuid, a custom type
func encodeByType(arg any) (any, bool, error) {
	v := reflect.ValueOf(arg)
	vt, err := classify(v.Type())
	if err != nil {
		return nil, false, err
	}
	value, err := vt.encode(v, false)
	return value, true, err
}

func describe(raw any) string {
	switch value := raw.(type) {
	case nil:
		return "NULL"
	case int64:
		return fmt.Sprintf("INTEGER %d", value)
	case float64:
		return fmt.Sprintf("REAL %v", value)
	case string:
		if len(value) > 40 {
			value = value[:37] + "…"
		}
		return fmt.Sprintf("TEXT %q", value)
	case []byte:
		return fmt.Sprintf("BLOB of %d bytes", len(value))
	case time.Time:
		return "TEXT " + value.Format(time.RFC3339Nano)
	}
	return fmt.Sprintf("%T", raw)
}
