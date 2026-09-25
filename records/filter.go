package records

import "slices"

// keepRows marks the rows of a block that may match, reading only the columns
// the query names: time always, then the level, name, trace, attribute and
// context columns it asks about. A row it keeps is rebuilt and checked whole;
// a row it drops is never rebuilt.
func (d *decoder) keepRows(block *openedBlock, q *checkedQuery) ([]bool, error) {
	keep, err := d.rowsInRange(block, q)
	if err != nil {
		return nil, err
	}
	narrowings := []func(*openedBlock, *checkedQuery, []bool) error{
		d.keepLevels, d.keepNames, d.keepTrace, d.keepAttrs, d.keepContexts,
	}
	for _, narrow := range narrowings {
		if err = narrow(block, q, keep); err != nil {
			return nil, err
		}
	}
	return keep, nil
}

// rowsInRange reads the time column, the first slot, which every record has
func (d *decoder) rowsInRange(block *openedBlock, q *checkedQuery) ([]bool, error) {
	times, err := d.intColumn(block, 0)
	keep := make([]bool, block.count)
	for row, at := range times {
		keep[row] = at >= q.first && at <= q.last
	}
	return keep, err
}

func (d *decoder) keepLevels(block *openedBlock, q *checkedQuery, keep []bool) error {
	if q.asked.MinLevel == nil {
		return nil
	}
	minimum := int64(*q.asked.MinLevel)
	return d.keepByInts(block, block.schema.slotOf(slotLevel, 0), keep, func(level int64) bool {
		return level >= minimum
	})
}

func (d *decoder) keepContexts(block *openedBlock, q *checkedQuery, keep []bool) error {
	if len(q.asked.Context) == 0 {
		return nil
	}
	matching := make([]bool, len(block.schema.contexts))
	for id, fields := range block.schema.contexts {
		matching[id] = containsAll(fields, q.asked.Context)
	}
	return d.keepByInts(block, block.schema.slotOf(slotContext, 0), keep, func(id int64) bool {
		return id >= 0 && id < int64(len(matching)) && matching[id]
	})
}

func containsAll(fields, wanted []Field) bool {
	for _, want := range wanted {
		if !slices.Contains(fields, want) {
			return false
		}
	}
	return true
}

// keepByInts keeps the rows whose value in an integer column passes; a row
// without a value in it, or a block without the column, keeps nothing
func (d *decoder) keepByInts(block *openedBlock, index int, keep []bool, pass func(int64) bool) error {
	has := make([]bool, block.count)
	if index >= 0 {
		values, err := d.intColumn(block, index)
		if err != nil {
			return err
		}
		block.eachValue(index, func(row, value int) { has[row] = pass(values[value]) })
	}
	both(keep, has)
	return nil
}

func (d *decoder) keepNames(block *openedBlock, q *checkedQuery, keep []bool) error {
	if len(q.asked.Names) == 0 {
		return nil
	}
	for row, id := range block.names {
		keep[row] = keep[row] && slices.Contains(q.asked.Names, block.schema.names[id])
	}
	return nil
}

func (d *decoder) keepTrace(block *openedBlock, q *checkedQuery, keep []bool) error {
	if q.asked.TraceID == (TraceID{}) {
		return nil
	}
	want := string(q.asked.TraceID[:])
	has := make([]bool, block.count)
	if index := block.schema.slotOf(slotTrace, 0); index >= 0 {
		traces, err := d.textColumn(block, index, len(TraceID{}))
		if err != nil {
			return err
		}
		block.eachValue(index, func(row, value int) { has[row] = traces[value] == want })
	}
	both(keep, has)
	return nil
}

// keepAttrs keeps, for each attribute asked for, the rows holding its value in
// any occurrence of its key; a row whose attributes are serialized is kept
// for the whole check
func (d *decoder) keepAttrs(block *openedBlock, q *checkedQuery, keep []bool) error {
	s := block.schema
	for _, want := range q.asked.Attrs {
		has := make([]bool, block.count)
		for row, shape := range block.shapes {
			has[row] = s.shapes[shape].presence&rawAttrs != 0
		}
		for _, column := range s.columnsOf(want.Key) {
			index := s.attrSlot + column
			values, err := d.valueColumn(block, index)
			if err != nil {
				return err
			}
			block.eachValue(index, func(row, value int) { has[row] = has[row] || values[value] == want.Value })
		}
		both(keep, has)
	}
	return nil
}

// eachValue visits the rows that have a value in a slot, with the value's
// place in its column
func (b *openedBlock) eachValue(index int, visit func(row, value int)) {
	carries := make([]bool, len(b.schema.shapes))
	for id, slots := range b.schema.shapeSlots {
		carries[id] = slices.Contains(slots, index)
	}
	value := 0
	for row, shape := range b.shapes {
		if carries[shape] {
			visit(row, value)
			value++
		}
	}
}

func both(keep, has []bool) {
	for row := range keep {
		keep[row] = keep[row] && has[row]
	}
}
