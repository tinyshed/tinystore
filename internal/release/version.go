package main

import (
	"cmp"
	"regexp"
	"strconv"
	"strings"
)

// the release's version as a tag says it; the packages drop the v
var releaseVersion = regexp.MustCompile(`^v(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)$`)

// a pre-release every registry spells: npm and Go as it is, PyPI as 0.1.0rc1
var publishable = regexp.MustCompile(`^v\d+\.\d+\.\d+(?:-(?:alpha|beta|rc)\.\d+)?$`)

// compareVersions orders two release tags as semantic versioning does: by
// their numbers, then a pre-release before its release, its identifiers
// compared one by one, numbers by value and before any word.
//
//	v0.1.0-rc.1 < v0.1.0-rc.2 < v0.1.0 < v0.1.1 < v0.10.0
func compareVersions(a, b string) int {
	aRelease, aPre, _ := strings.Cut(strings.TrimPrefix(a, "v"), "-")
	bRelease, bPre, _ := strings.Cut(strings.TrimPrefix(b, "v"), "-")
	if order := compareIdentifiers(aRelease, bRelease); order != 0 {
		return order
	}
	switch {
	case aPre == bPre:
		return 0
	case aPre == "":
		return 1
	case bPre == "":
		return -1
	}
	return compareIdentifiers(aPre, bPre)
}

func compareIdentifiers(a, b string) int {
	as, bs := strings.Split(a, "."), strings.Split(b, ".")
	for i := range min(len(as), len(bs)) {
		aNumber, aErr := strconv.ParseUint(as[i], 10, 64)
		bNumber, bErr := strconv.ParseUint(bs[i], 10, 64)
		var order int
		switch {
		case aErr == nil && bErr == nil:
			order = cmp.Compare(aNumber, bNumber)
		case aErr == nil:
			order = -1
		case bErr == nil:
			order = 1
		default:
			order = strings.Compare(as[i], bs[i])
		}
		if order != 0 {
			return order
		}
	}
	return cmp.Compare(len(as), len(bs))
}

// isPreRelease says whether a release tag has a pre-release part: v0.1.0-rc.1.
func isPreRelease(tag string) bool {
	return strings.Contains(tag, "-")
}
