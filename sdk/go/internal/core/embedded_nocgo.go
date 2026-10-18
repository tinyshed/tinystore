//go:build !windows && !cgo

package core

func loadLibrary(path string) (coreLibrary, error) {
	return nil, Errorf(Closed, "the core in this process needs cgo on this platform, to load %s", path)
}
