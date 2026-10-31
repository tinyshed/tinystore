package limits

import (
	"encoding/json"
	"os"
	"reflect"
	"testing"
)

// The package is every bound of testdata/wire/limits.json, which Rust writes.
func TestEveryBoundOfTheTable(t *testing.T) {
	text, err := os.ReadFile("../../../testdata/wire/limits.json")
	if err != nil {
		t.Fatal(err)
	}
	var table map[string]int64
	if err := json.Unmarshal(text, &table); err != nil {
		t.Fatal(err)
	}
	exported := make(map[string]int64, len(all))
	for _, limit := range all {
		exported[limit.Name] = limit.Bound
	}
	if len(exported) != len(all) || !reflect.DeepEqual(exported, table) {
		t.Fatalf("the bounds are not the table's:\n%v\n%v", exported, table)
	}
}
