package records

import "testing"

func TestSelfStatsCountDamageWithoutCopyingEveryRow(t *testing.T) {
	store := &Store{}
	store.damaged.found = make(map[damageKey]Damage)
	for id := int64(1); id <= 100; id++ {
		store.damaged.note(Damage{HeadRow: id})
	}
	if got := store.Stats().Damaged; got != 100 {
		t.Fatalf("damaged count: %d", got)
	}
	if allocations := testing.AllocsPerRun(100, func() { _ = store.Stats() }); allocations != 0 {
		t.Fatalf("stats copied damage rows: %g allocations", allocations)
	}
}
