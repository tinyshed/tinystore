package blobs

import (
	"fmt"
	"testing"
)

// Clear empties a folder and every folder under it at once: under its bound in
// one transaction, over it by a mark that hides the objects before maintenance
// deletes them. A neighbour whose name the folder's begins stays, and an object
// written after the Clear is a new object.
func TestClearEmptiesAFolderAndThoseUnderIt(t *testing.T) {
	for _, bound := range []int{clearAtOnce, 10} {
		t.Run(fmt.Sprint("bound ", bound), func(t *testing.T) {
			s := openTestStore(t, t.TempDir())
			s.clearBound = bound
			media := openTestBucket(t, s, "media")
			for i := range 30 {
				size := 100
				if i%10 == 0 {
					size = inlineSize + 1
				}
				mustPut(t, media, fmt.Sprintf("users/4/%d/photo", i), randomBytes(uint64(i), size))
			}
			mustPut(t, media, "users/42/kept", []byte("neighbour"))
			if err := media.Of("users", 4).Clear(t.Context()); err != nil {
				t.Fatal(err)
			}
			mustPut(t, media, "users/4/0/photo", []byte("after"))
			usage, err := media.Usage(t.Context())
			if err != nil || usage.Objects != 2 {
				t.Fatalf("after the Clear the bucket holds %+v, %v", usage, err)
			}
			mustHold(t, media, "users/42/kept", []byte("neighbour"))
			mustHold(t, media, "users/4/0/photo", []byte("after"))
			mustBeAbsent(t, media, "users/4/1/photo")

			done := s.maintain(t)
			if (bound == 10) != (done.Cleared == 29) {
				t.Fatalf("maintenance deleted %d objects a mark hid", done.Cleared)
			}
			s.mustHoldFiles(t, 0)
			usage, err = media.Usage(t.Context())
			if err != nil || usage.Objects != 2 {
				t.Fatalf("after maintenance the bucket holds %+v, %v", usage, err)
			}
			if err = media.Clear(t.Context()); err != nil {
				t.Fatal(err)
			}
			s.maintain(t)
			if usage, _ = media.Usage(t.Context()); usage.Objects != 0 {
				t.Fatalf("the bucket's root cleared holds %+v", usage)
			}
		})
	}
}
