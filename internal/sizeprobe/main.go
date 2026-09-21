// Command sizeprobe links the public API so task size can report what importing it costs.
package main

import (
	"fmt"

	"github.com/tinyshed/tinystore/codec"
)

func main() {
	blocks, err := codec.New()
	if err != nil {
		panic(err)
	}
	defer func() { _ = blocks.Close() }()

	payload, err := blocks.Encode([]codec.Sample{{At: 1, Value: 1}})
	if err != nil {
		panic(err)
	}
	fmt.Println(len(payload))
}
