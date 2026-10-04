package tinystore

import (
	"bufio"
	"bytes"
	"io/fs"
	"os"
	"path"
	"strconv"
	"strings"
)

// FromCgroup is fraction of the memory the process's container may use, for
// Options.Memory: cgroup v2's memory.max, or v1's memory.limit_in_bytes. It
// is 0, no budget, outside Linux and when the container sets no limit.
//
//	tinystore.Options{Memory: tinystore.FromCgroup(0.5)}  // half of a 32 MiB container: 16 MiB
func FromCgroup(fraction float64) int64 {
	limit := cgroupLimit(os.DirFS("/"))
	if limit <= 0 || fraction <= 0 {
		return 0
	}
	return int64(float64(limit) * min(fraction, 1))
}

// cgroupLimit reads the memory limit of the process's own cgroup, then of
// the root one a container sees as its own, v2 before v1; 0 when none is set
func cgroupLimit(root fs.FS) int64 {
	var candidates []string
	if own := ownCgroup(root); own != "" {
		candidates = append(candidates, path.Join("sys/fs/cgroup", own, "memory.max"))
	}
	candidates = append(candidates, "sys/fs/cgroup/memory.max", "sys/fs/cgroup/memory/memory.limit_in_bytes")
	for _, name := range candidates {
		text, err := fs.ReadFile(root, name)
		if err != nil {
			continue
		}
		value := strings.TrimSpace(string(text))
		if value == "max" {
			return 0
		}
		limit, err := strconv.ParseInt(value, 10, 64)
		// v1 says "no limit" with the largest page-aligned int64
		if err != nil || limit <= 0 || limit >= 1<<62 {
			return 0
		}
		return limit
	}
	return 0
}

// ownCgroup is the v2 path /proc/self/cgroup gives the process: 0::/app.slice/web.service
func ownCgroup(root fs.FS) string {
	text, err := fs.ReadFile(root, "proc/self/cgroup")
	if err != nil {
		return ""
	}
	lines := bufio.NewScanner(bytes.NewReader(text))
	for lines.Scan() {
		if rest, ok := strings.CutPrefix(lines.Text(), "0::"); ok {
			return strings.TrimPrefix(rest, "/")
		}
	}
	return ""
}
