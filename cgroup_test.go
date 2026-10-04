package tinystore

import (
	"testing"
	"testing/fstest"
)

// a container's memory limit is read from its own cgroup, then the root one,
// v2 before v1, and no limit is no budget
func TestFromCgroupIsAFractionOfTheContainersLimit(t *testing.T) {
	file := func(text string) *fstest.MapFile { return &fstest.MapFile{Data: []byte(text)} }
	for name, c := range map[string]struct {
		files fstest.MapFS
		want  int64
	}{
		"v2, the container's root": {fstest.MapFS{"sys/fs/cgroup/memory.max": file("33554432\n")}, 33554432},
		"v2, the process's own cgroup": {fstest.MapFS{
			"proc/self/cgroup": file("0::/app.slice/web.service\n"),
			"sys/fs/cgroup/app.slice/web.service/memory.max": file("67108864\n"),
		}, 67108864},
		"v2 without a limit": {fstest.MapFS{"sys/fs/cgroup/memory.max": file("max\n")}, 0},
		"v1":                 {fstest.MapFS{"sys/fs/cgroup/memory/memory.limit_in_bytes": file("33554432\n")}, 33554432},
		"v1 without a limit": {fstest.MapFS{"sys/fs/cgroup/memory/memory.limit_in_bytes": file("9223372036854771712\n")}, 0},
		"no cgroup":          {fstest.MapFS{}, 0},
	} {
		if got := cgroupLimit(c.files); got != c.want {
			t.Errorf("%s: %d, want %d", name, got, c.want)
		}
	}
	if FromCgroup(0) != 0 {
		t.Error("a fraction of 0 is a budget")
	}
}
