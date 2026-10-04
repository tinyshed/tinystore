package sqlite

import (
	"slices"
	"strings"
	"sync"

	"github.com/ncruces/go-sqlite3"
)

// A virtual table module is a package of its own, which registers itself as
// it loads, as image/png registers its decoder: a program that never makes
// such a table does not link its code.
var (
	modulesMu sync.RWMutex
	modules   []module
)

type module struct {
	names    []string // what CREATE VIRTUAL TABLE … USING names
	register func(*sqlite3.Conn) error
}

// RegisterModule gives each connection RegisterModules runs on the modules
// names, through register. A module package's init calls it.
func RegisterModule(names []string, register func(*sqlite3.Conn) error) {
	modulesMu.Lock()
	defer modulesMu.Unlock()
	modules = append(modules, module{names: slices.Clone(names), register: register})
}

// RegisterModules registers every module the program linked on conn, for a
// file's Connected.
func RegisterModules(conn *sqlite3.Conn) error {
	modulesMu.RLock()
	defer modulesMu.RUnlock()
	for _, m := range modules {
		if err := m.register(conn); err != nil {
			return err
		}
	}
	return nil
}

// ModuleLinked says whether the program linked the module of that name.
func ModuleLinked(name string) bool {
	modulesMu.RLock()
	defer modulesMu.RUnlock()
	for _, m := range modules {
		if slices.ContainsFunc(m.names, func(n string) bool { return strings.EqualFold(n, name) }) {
			return true
		}
	}
	return false
}
