// Package temp carries what tinystoretest asks of tinystore and no program
// may: an option that opens embedded unless the test asks for a child.
package temp

// EmbeddedUnlessPrivate is a tinystore.Option, which tinystore sets as it
// loads; an any, since this package cannot name tinystore's type.
var EmbeddedUnlessPrivate any
