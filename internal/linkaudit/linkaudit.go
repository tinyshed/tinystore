// Package linkaudit is a canary for the linker's method pruning. The size
// probe prints a Canary, so its type reaches an interface, and calls none of
// its methods: the linker drops them unless something links
// reflect.Value.MethodByName with a name it cannot see, which keeps every
// exported method of every such type, in the probe and in any program
// importing TinyStore. task size fails when the probe still holds Unreached.
package linkaudit

// Canary is printed and never called.
type Canary struct{ N int }

// Unreached is the method task size looks for.
func (Canary) Unreached() int { return 7919 }
