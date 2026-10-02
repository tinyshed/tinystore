package main

import "testing"

func TestVersionsOrderAsSemanticVersioningSays(t *testing.T) {
	ordered := []string{
		"v0.1.0-alpha.1", "v0.1.0-alpha.beta", "v0.1.0-beta.2", "v0.1.0-beta.11",
		"v0.1.0-rc.1", "v0.1.0-rc.2", "v0.1.0", "v0.1.1", "v0.2.0", "v0.10.0", "v1.0.0",
	}
	for i, earlier := range ordered {
		for _, later := range ordered[i+1:] {
			if compareVersions(earlier, later) >= 0 || compareVersions(later, earlier) <= 0 {
				t.Errorf("%s does not come before %s", earlier, later)
			}
		}
		if compareVersions(earlier, earlier) != 0 {
			t.Errorf("%s is not itself", earlier)
		}
	}
}

func TestOnlyAPreReleaseEveryRegistrySpellsIsPublished(t *testing.T) {
	for version, want := range map[string]bool{
		"v0.1.0": true, "v0.1.0-alpha.1": true, "v0.1.0-beta.2": true, "v0.1.0-rc.10": true,
		"v0.1.0-trial.1": false, "v0.1.0-rc": false, "v0.1.0-rc.1.2": false, "0.1.0": false,
	} {
		if got := publishable.MatchString(version); got != want {
			t.Errorf("%s: publishable %v, want %v", version, got, want)
		}
	}
}
