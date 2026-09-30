// Package dbstat divides a SQLite file's pages among the tables and indexes
// that own them, as SQLite's dbstat table does, for measurements: SQLite as the
// driver builds it leaves dbstat out.
//
// It reads the file and not a connection, so a page the write-ahead log still
// holds is not counted: close the file, or checkpoint it, first.
package dbstat

import (
	"context"
	"encoding/binary"
	"errors"
	"fmt"
	"math"
	"os"
	"unicode/utf16"
)

// Object is one b-tree of the file, sqlite_schema's included.
type Object struct {
	Name  string
	Pages int64 // its b-tree's pages with their overflow pages
	Bytes int64 // what dbstat's sum(pgsize) gives
}

// Read divides the file at path, each object in the order sqlite_schema lists
// it. It fails unless every page of the file is an object's, the freelist's, a
// pointer map's or the one that holds SQLite's lock bytes, each counted once.
func Read(ctx context.Context, path string) (_ []Object, err error) {
	if err = ctx.Err(); err != nil {
		return nil, err
	}

	f, err := os.Open(path) //nolint:gosec // The caller chooses the closed database to measure.
	if err != nil {
		return nil, fmt.Errorf("dbstat: %w", err)
	}
	defer func() { err = errors.Join(err, f.Close()) }()

	file, err := readHeader(ctx, f)
	if err != nil {
		return nil, fmt.Errorf("dbstat: %s: %w", path, err)
	}

	objects, err := file.objects()
	if err != nil {
		return nil, fmt.Errorf("dbstat: %s: %w", path, err)
	}
	return objects, nil
}

// file is a database file's pages, numbered from 1, and which of them the
// walk has counted
type file struct {
	ctx      context.Context
	source   *os.File
	pageSize uint32
	usable   uint32 // a page less the bytes an extension reserves at its end
	pages    uint32
	encoding uint32
	vacuums  bool // auto_vacuum keeps pointer-map pages
	trunk    uint32
	counted  []bool
	depth    int
}

// header offsets, from https://sqlite.org/fileformat2.html
const (
	headerSize     = 100
	magic          = "SQLite format 3\x00"
	lockByte       = 0x40000000 // SQLite never writes the page holding it
	tableInterior  = 0x05
	tableLeaf      = 0x0d
	indexInterior  = 0x02
	indexLeaf      = 0x0a
	schemaColumns  = 5 // type, name, tbl_name, rootpage, sql
	schemaName     = 1
	schemaRootPage = 3
)

func readHeader(ctx context.Context, source *os.File) (*file, error) {
	var header [headerSize]byte
	if _, err := source.ReadAt(header[:], 0); err != nil {
		return nil, fmt.Errorf("read the header: %w", err)
	}
	if string(header[:16]) != magic {
		return nil, errors.New("not a SQLite file")
	}

	f := &file{ctx: ctx, source: source, pageSize: uint32(binary.BigEndian.Uint16(header[16:]))}
	if f.pageSize == 1 {
		f.pageSize = 65536
	}
	if f.pageSize < 512 || f.pageSize > 65536 || f.pageSize&(f.pageSize-1) != 0 {
		return nil, fmt.Errorf("invalid page size %d", f.pageSize)
	}

	f.usable = f.pageSize - uint32(header[20])
	if f.usable < 480 {
		return nil, fmt.Errorf("a page has only %d usable bytes", f.usable)
	}
	if header[21] != 64 || header[22] != 32 || header[23] != 32 {
		return nil, errors.New("invalid payload fractions")
	}

	var err error
	f.pages, err = pagesInFile(source, header[:], f.pageSize)
	if err != nil {
		return nil, err
	}

	f.encoding = binary.BigEndian.Uint32(header[56:])
	if f.encoding < 1 || f.encoding > 3 {
		return nil, fmt.Errorf("invalid text encoding %d", f.encoding)
	}

	f.trunk = binary.BigEndian.Uint32(header[32:])
	f.vacuums = binary.BigEndian.Uint32(header[52:]) != 0
	f.counted = make([]bool, f.pages+1)
	return f, nil
}

func pagesInFile(source *os.File, header []byte, pageSize uint32) (uint32, error) {
	info, err := source.Stat()
	if err != nil {
		return 0, err
	}
	pages := info.Size() / int64(pageSize)
	if pages < 1 || pages > math.MaxUint32-1 || info.Size()%int64(pageSize) != 0 {
		return 0, errors.New("the file is not a whole number of SQLite pages")
	}
	declared := binary.BigEndian.Uint32(header[28:])
	if declared != 0 && binary.BigEndian.Uint32(header[24:]) == binary.BigEndian.Uint32(header[92:]) &&
		int64(declared) != pages {
		return 0, fmt.Errorf("the header declares %d pages, the file has %d", declared, pages)
	}
	return uint32(pages), nil
}

// objects walks sqlite_schema's b-tree for the others', then each of those,
// then the pages that belong to no object
func (f *file) objects() ([]Object, error) {
	type root struct {
		name string
		page uint32
	}
	var roots []root
	schemaPages, err := f.walk(1, func(record []byte) error {
		name, page, err := schemaRow(record, f.encoding)
		if err == nil && page > 0 {
			roots = append(roots, root{name, page})
		}
		return err
	})
	if err != nil {
		return nil, fmt.Errorf("sqlite_schema: %w", err)
	}
	objects := []Object{{Name: "sqlite_schema", Pages: schemaPages}}
	for _, r := range roots {
		pages, err := f.walk(r.page, nil)
		if err != nil {
			return nil, fmt.Errorf("%s: %w", r.name, err)
		}
		objects = append(objects, Object{Name: r.name, Pages: pages})
	}
	for i := range objects {
		objects[i].Bytes = objects[i].Pages * int64(f.pageSize)
	}
	return objects, f.countTheRest()
}

// walk counts the pages of the b-tree rooted at page, overflow pages
// included, and hands each row of a table's leaves to visit when it is set
func (f *file) walk(page uint32, visit func(record []byte) error) (int64, error) {
	if f.depth >= 64 {
		return 0, errors.New("a b-tree is deeper than 64 pages")
	}
	f.depth++
	defer func() { f.depth-- }()

	tree, err := f.readTree(page)
	if err != nil {
		return 0, err
	}

	count := int64(1)
	for i := 0; i < len(tree.pointers); i += 2 {
		at := int(binary.BigEndian.Uint16(tree.pointers[i:]))
		if at < tree.headerEnd {
			return 0, fmt.Errorf("page %d holds a cell over its header", page)
		}
		pages, err := f.cell(tree, at, visit)
		if err != nil {
			return 0, err
		}
		count += pages
	}

	if tree.kind == tableInterior || tree.kind == indexInterior {
		pages, err := f.walk(tree.right, visit)
		if err != nil {
			return 0, err
		}
		count += pages
	}
	return count, nil
}

type treePage struct {
	number    uint32
	kind      byte
	data      []byte
	pointers  []byte
	headerEnd int
	right     uint32
}

func (f *file) readTree(page uint32) (treePage, error) {
	data, err := f.read(page)
	if err != nil {
		return treePage{}, err
	}

	start := 0
	if page == 1 {
		start = headerSize
	}
	tree := treePage{number: page, kind: data[start], data: data[:f.usable]}
	interior := tree.kind == tableInterior || tree.kind == indexInterior
	if !interior && tree.kind != tableLeaf && tree.kind != indexLeaf {
		return tree, fmt.Errorf("page %d is not a b-tree page but type %#x", page, tree.kind)
	}

	pointers := start + 8
	if interior {
		pointers = start + 12
		tree.right = binary.BigEndian.Uint32(data[start+8:])
	}
	cells := int(binary.BigEndian.Uint16(data[start+3:]))
	tree.headerEnd = pointers + 2*cells
	if tree.headerEnd > len(tree.data) {
		return tree, fmt.Errorf("page %d says it holds %d cells", page, cells)
	}
	tree.pointers = data[pointers:tree.headerEnd]
	return tree, nil
}

// cell counts what one cell holds outside its page: the subtree to the left
// of an interior cell, and the overflow pages of a payload too long for it.
//
//	table interior   child(4) rowid(varint)
//	table leaf       size(varint) rowid(varint) payload [overflow(4)]
//	index interior   child(4) size(varint) payload [overflow(4)]
//	index leaf       size(varint) payload [overflow(4)]
func (f *file) cell(page treePage, at int, visit func([]byte) error) (int64, error) {
	cell := page.data
	if at+4 > len(cell) { // SQLite gives even the smallest cell four bytes
		return 0, pastItsEnd(page.number)
	}

	var count int64
	if page.kind == tableInterior || page.kind == indexInterior {
		pages, err := f.walk(binary.BigEndian.Uint32(cell[at:]), visit)
		if err != nil {
			return 0, err
		}
		if page.kind == tableInterior {
			if _, _, err = varint(cell[at+4:]); err != nil {
				return 0, err
			}
			return pages, nil
		}
		count, at = pages, at+4
	}

	pages, err := f.payload(page, at, visit)
	return count + pages, err
}

func (f *file) payload(page treePage, at int, visit func([]byte) error) (int64, error) {
	cell := page.data
	size, n, err := varint(cell[at:])
	if err != nil {
		return 0, err
	}
	at += n
	if page.kind == tableLeaf {
		_, n, err = varint(cell[at:])
		if err != nil {
			return 0, err
		}
		at += n
	}

	if size > uint64(f.pages)*uint64(f.usable) {
		return 0, fmt.Errorf("page %d holds a payload larger than the file", page.number)
	}
	local := f.local(size, page.kind == tableLeaf)
	end := at + int(local)
	if uint64(local) < size {
		end += 4
	}
	if end > len(cell) {
		return 0, pastItsEnd(page.number)
	}

	var count int64
	payload := cell[at : at+int(local) : at+int(local)] // an append copies rather than overwrite the page
	if uint64(local) < size {
		rest, pages, err := f.overflow(binary.BigEndian.Uint32(cell[end-4:]), size-uint64(local), visit != nil)
		if err != nil {
			return 0, fmt.Errorf("the cell at %d of page %d: %w", at, page.number, err)
		}
		payload, count = append(payload, rest...), count+pages
	}
	if visit != nil && page.kind == tableLeaf {
		if err := visit(payload); err != nil {
			return 0, err
		}
	}
	return count, nil
}

func pastItsEnd(page uint32) error {
	return fmt.Errorf("page %d holds a cell past its end", page)
}

// local is how much of a payload of size bytes its cell keeps on the page;
// the rest goes to overflow pages
func (f *file) local(size uint64, tableLeaf bool) uint16 {
	usable := uint64(f.usable)
	most := usable - 35
	if !tableLeaf {
		most = (usable-12)*64/255 - 23
	}
	kept := size
	if size > most {
		least := (usable-12)*32/255 - 23
		kept = least + (size-least)%(usable-4)
		if kept > most {
			kept = least
		}
	}
	return uint16(kept) //nolint:gosec // A local payload is smaller than the checked page size.
}

// overflow follows a chain of overflow pages, each the next page's number and
// then its share of the payload, and reads the payload when keep is set
func (f *file) overflow(page uint32, size uint64, keep bool) ([]byte, int64, error) {
	var payload []byte
	var count int64
	for size > 0 {
		data, err := f.read(page)
		if err != nil {
			return nil, 0, err
		}
		count++
		share := min(size, uint64(f.usable-4))
		if keep {
			payload = append(payload, data[4:4+share]...)
		}
		size -= share
		page = binary.BigEndian.Uint32(data)
	}
	if page != 0 {
		return nil, 0, fmt.Errorf("an overflow chain goes on past its payload to page %d", page)
	}
	return payload, count, nil
}

// countTheRest counts the freelist, the pointer maps and the page of the lock
// bytes, and fails if any page is then left over
func (f *file) countTheRest() error {
	if err := f.countFreelist(); err != nil {
		return fmt.Errorf("the freelist: %w", err)
	}
	lockPage := lockByte/f.pageSize + 1
	if lockPage <= f.pages {
		f.counted[lockPage] = true
	}
	if f.vacuums {
		if err := f.countPointerMaps(lockPage); err != nil {
			return fmt.Errorf("the pointer maps: %w", err)
		}
	}
	var left []uint32
	for page := uint32(1); page <= f.pages; page++ {
		if err := f.ctx.Err(); err != nil {
			return err
		}
		if !f.counted[page] {
			left = append(left, page)
		}
	}
	if len(left) > 0 {
		return fmt.Errorf("%d pages belong to nothing the walk found, the first %d", len(left), left[0])
	}
	return nil
}

// countFreelist follows the freelist's trunk pages, each the next trunk's
// number, how many pages it lists, and their numbers
func (f *file) countFreelist() error {
	for trunk := f.trunk; trunk != 0; {
		data, err := f.read(trunk)
		if err != nil {
			return err
		}
		leaves := int(binary.BigEndian.Uint32(data[4:]))
		if int64(leaves) > int64(f.usable-8)/4 {
			return fmt.Errorf("page %d says it lists %d pages", trunk, leaves)
		}
		for i := range leaves {
			if err = f.count(binary.BigEndian.Uint32(data[8+4*i:])); err != nil {
				return err
			}
		}
		trunk = binary.BigEndian.Uint32(data)
	}
	return nil
}

// countPointerMaps counts the maps auto_vacuum keeps: page 2, then one after
// every usable/5 pages it maps, moved past the lock page
func (f *file) countPointerMaps(lockPage uint32) error {
	for first := uint64(2); first <= uint64(f.pages); first += uint64(f.usable/5) + 1 {
		page := first
		if page == uint64(lockPage) {
			page++
		}
		if page > uint64(f.pages) || page > math.MaxUint32 {
			break
		}
		if err := f.count(uint32(page)); err != nil {
			return err
		}
	}
	return nil
}

// read counts a page and reads it
func (f *file) read(page uint32) ([]byte, error) {
	if err := f.count(page); err != nil {
		return nil, err
	}
	data := make([]byte, f.pageSize)
	if _, err := f.source.ReadAt(data, int64(page-1)*int64(f.pageSize)); err != nil {
		return nil, fmt.Errorf("read page %d: %w", page, err)
	}
	return data, nil
}

func (f *file) count(page uint32) error {
	if err := f.ctx.Err(); err != nil {
		return err
	}
	switch {
	case page == 0 || page > f.pages:
		return fmt.Errorf("page %d is outside the file's %d", page, f.pages)
	case f.counted[page]:
		return fmt.Errorf("page %d is counted twice", page)
	}
	f.counted[page] = true
	return nil
}

// schemaRow reads the name and the root page of one row of sqlite_schema: a
// record is a header of serial types, then the values they give the sizes of
func schemaRow(record []byte, encoding uint32) (string, uint32, error) {
	values, err := schemaValues(record)
	if err != nil {
		return "", 0, err
	}

	named := values[schemaName]
	if named.serial < 13 || named.serial%2 == 0 {
		return "", 0, errors.New("a schema row's name is not text")
	}
	name, err := text(named.bytes, encoding)
	if err != nil {
		return "", 0, err
	}

	root := values[schemaRootPage]
	if root.serial > 9 || root.serial == 7 {
		return "", 0, errors.New("a schema row's root is not an integer")
	}
	page := integer(root.serial, root.bytes)
	if page < 0 || page > math.MaxUint32-1 {
		return "", 0, errors.New("a schema row's root is not a page number")
	}
	return name, uint32(page), nil
}

type recordValue struct {
	serial uint64
	bytes  []byte
}

func schemaValues(record []byte) ([schemaColumns]recordValue, error) {
	var values [schemaColumns]recordValue
	headerEnd, at, err := varint(record)
	if err != nil {
		return values, err
	}
	if headerEnd > math.MaxInt32 || headerEnd > uint64(len(record)) {
		return values, errors.New("a row's header runs past it")
	}
	body := int(headerEnd)
	if body < at {
		return values, errors.New("a row's header ends before its length")
	}

	for column := range schemaColumns {
		serial, n, err := varint(record[at:headerEnd])
		if err != nil {
			return values, err
		}
		at += n
		size := serialSize(serial)
		if size > math.MaxInt32 || serial == 10 || serial == 11 {
			return values, errors.New("a row's value is too large or has a reserved serial type")
		}
		if int(size) > len(record)-body {
			return values, errors.New("a row's value runs past it")
		}
		values[column] = recordValue{serial, record[body : body+int(size)]}
		body += int(size)
	}

	if at != int(headerEnd) || body != len(record) {
		return values, errors.New("a schema row does not have five columns")
	}
	return values, nil
}

func text(value []byte, encoding uint32) (string, error) {
	if encoding == 1 {
		return string(value), nil
	}
	if len(value)%2 != 0 {
		return "", errors.New("a UTF-16 name has an incomplete code unit")
	}
	units := make([]uint16, len(value)/2)
	for i := range units {
		if encoding == 2 {
			units[i] = binary.LittleEndian.Uint16(value[2*i:])
		} else {
			units[i] = binary.BigEndian.Uint16(value[2*i:])
		}
	}
	return string(utf16.Decode(units)), nil
}

// serialSize is the bytes a value of a record's serial type takes
func serialSize(serial uint64) uint64 {
	switch {
	case serial >= 12:
		return (serial - 12) / 2
	case serial <= 4:
		return serial
	case serial == 5:
		return 6
	case serial == 6 || serial == 7:
		return 8
	}
	return 0 // 8 and 9 are the integers 0 and 1, which take no bytes
}

// integer is a record's big-endian two's-complement integer
func integer(serial uint64, value []byte) int64 {
	switch serial {
	case 8:
		return 0
	case 9:
		return 1
	}
	var n int64
	if len(value) > 0 && value[0]&0x80 != 0 {
		n = -1
	}
	for _, b := range value {
		n = n<<8 | int64(b)
	}
	return n
}

// varint reads SQLite's variable-length integer, big-endian and seven bits a
// byte, of which the ninth byte gives all eight
func varint(b []byte) (uint64, int, error) {
	var v uint64
	for i := 0; i < len(b); i++ {
		if i == 8 {
			return v<<8 | uint64(b[i]), 9, nil
		}
		v = v<<7 | uint64(b[i]&0x7f)
		if b[i] < 0x80 {
			return v, i + 1, nil
		}
	}
	return 0, 0, errors.New("an incomplete variable-length integer")
}
