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
// Options.Memory: cgroup v2's memory.max, or v1's memory.limit_in_bytes,
// including the limits of its ancestors. It is 0, no budget, outside Linux
// and when the container sets no limit.
//
//	tinystore.Options{Memory: tinystore.FromCgroup(0.5)}  // half of a 32 MiB container: 16 MiB
func FromCgroup(fraction float64) int64 {
	limit := cgroupLimit(os.DirFS("/"))
	if limit <= 0 || fraction <= 0 {
		return 0
	}
	return int64(float64(limit) * min(fraction, 1))
}

// cgroupLimit reads the smallest limit of the process's cgroup and its
// ancestors, v2 before v1. An unlimited child still has its parent's limit.
func cgroupLimit(root fs.FS) int64 {
	if limit := hierarchyLimit(root, "sys/fs/cgroup", ownCgroup(root, ""), "memory.max"); limit > 0 {
		return limit
	}
	return hierarchyLimit(root, "sys/fs/cgroup/memory", ownCgroup(root, "memory"), "memory.limit_in_bytes")
}

func hierarchyLimit(root fs.FS, base, own, file string) int64 {
	directory := strings.TrimPrefix(path.Clean("/"+own), "/")
	var smallest int64
	for {
		if limit := readCgroupLimit(root, path.Join(base, directory, file)); limit > 0 {
			if smallest == 0 || limit < smallest {
				smallest = limit
			}
		}
		if directory == "" {
			return smallest
		}
		directory = path.Dir(directory)
		if directory == "." {
			directory = ""
		}
	}
}

func readCgroupLimit(root fs.FS, name string) int64 {
	text, err := fs.ReadFile(root, name)
	if err != nil {
		return 0
	}
	limit, err := strconv.ParseInt(strings.TrimSpace(string(text)), 10, 64)
	// v1 says "no limit" with the largest page-aligned int64.
	if err != nil || limit <= 0 || limit >= 1<<62 {
		return 0
	}
	return limit
}

// ownCgroup finds the v2 path, or the v1 path of the requested controller.
func ownCgroup(root fs.FS, controller string) string {
	text, err := fs.ReadFile(root, "proc/self/cgroup")
	if err != nil {
		return ""
	}
	lines := bufio.NewScanner(bytes.NewReader(text))
	for lines.Scan() {
		fields := strings.SplitN(lines.Text(), ":", 3)
		if len(fields) != 3 {
			continue
		}
		v2 := controller == "" && fields[0] == "0" && fields[1] == ""
		v1 := controller != "" && strings.Contains(","+fields[1]+",", ","+controller+",")
		if v2 || v1 {
			return strings.TrimPrefix(fields[2], "/")
		}
	}
	return ""
}
