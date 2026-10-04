package kv

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

type appConfig struct {
	Port    int           `json:"port"`
	DBURL   string        `json:"dbUrl" env:"DATABASE_URL" secret:"true"`
	Origins []string      `json:"origins"`
	Timeout time.Duration `json:"timeout"`
	Limits  struct {
		RPS   int  `json:"rps"`
		Burst int  `json:"burst"`
		On    bool `json:"on"`
	} `json:"limits"`
}

func openTestConfig(t *testing.T, state *testState, options ...ConfigOption) *Config[appConfig] {
	t.Helper()
	config, err := OpenConfig[appConfig](t.Context(), state.Store, "app", options...)
	if err != nil {
		t.Fatal(err)
	}
	return config
}

// writeDotenv writes a .env file in a test's directory and returns its path
func writeDotenv(t *testing.T, text string) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), ".env")
	if err := os.WriteFile(path, []byte(text), 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}

// each layer is over the one before it: the code's defaults, a file's, the
// .env, the process's environment, and what Update kept, which outlives a
// restart; Reset gives a field back to the layers under it
func TestAConfigIsItsDefaultsThenItsEnvironmentThenWhatWasKept(t *testing.T) {
	state := openTestState(t, t.TempDir())
	dotenv := writeDotenv(t, "APP_PORT=3000\nAPP_ORIGINS=a.com, b.com\nAPP_TIMEOUT=1h30m\n")
	t.Setenv("APP_LIMITS_RPS", "50")
	t.Setenv("DATABASE_URL", "postgres://secret")
	file := map[string]any{"port": 8000, "limits": map[string]any{"burst": 10, "on": true}, "timeout": "30s"}
	options := []ConfigOption{Defaults(appConfig{Port: 8080}), Defaults(file), FromEnv("APP", dotenv)}
	config := openTestConfig(t, state, options...)

	got := config.Get()
	if got.Port != 3000 || got.DBURL != "postgres://secret" || !slices.Equal(got.Origins, []string{"a.com", "b.com"}) ||
		got.Timeout != 90*time.Minute || got.Limits.RPS != 50 || got.Limits.Burst != 10 || !got.Limits.On {
		t.Fatalf("the layers made %+v", got)
	}

	err := config.Update(t.Context(), func(c *appConfig) { c.Port, c.Limits.Burst = 4000, 20 })
	if err != nil {
		t.Fatal(err)
	}
	if got = config.Get(); got.Port != 4000 || got.Limits.Burst != 20 || got.Limits.RPS != 50 {
		t.Fatalf("after Update: %+v", got)
	}

	state = state.reopen(t)
	config = openTestConfig(t, state, options...)
	if got = config.Get(); got.Port != 4000 || got.Limits.Burst != 20 {
		t.Fatalf("after a restart: %+v", got)
	}
	if err = config.Reset(t.Context(), "port"); err != nil {
		t.Fatal(err)
	}
	if got = config.Get(); got.Port != 3000 || got.Limits.Burst != 20 {
		t.Fatalf("after Reset of port: %+v", got)
	}
	if err = config.Reset(t.Context(), "limits"); err != nil {
		t.Fatal(err)
	}
	if got = config.Get(); got.Limits.Burst != 10 {
		t.Fatalf("after Reset of limits: %+v", got)
	}
}

// a change through one handle is seen by every other at once, and by a
// watcher, without a read of the file
func TestAChangeIsSeenByEveryHandleAtOnce(t *testing.T) {
	state := openTestState(t, t.TempDir())
	first := openTestConfig(t, state, Defaults(appConfig{Port: 8080}))
	second := openTestConfig(t, state, Defaults(appConfig{Port: 8080}))

	watched := make(chan int, 4)
	ctx, stop := context.WithCancel(t.Context())
	done := make(chan struct{})
	go func() {
		defer close(done)
		for c := range second.Watch(ctx) {
			watched <- c.Port
		}
	}()
	if port := <-watched; port != 8080 {
		t.Fatalf("a watcher began with %d", port)
	}

	if err := first.Update(t.Context(), func(c *appConfig) { c.Port = 9090 }); err != nil {
		t.Fatal(err)
	}
	if port := second.Get().Port; port != 9090 {
		t.Fatalf("the other handle reads %d", port)
	}
	select {
	case port := <-watched:
		if port != 9090 {
			t.Fatalf("the watcher saw %d", port)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("the watcher saw no change")
	}
	stop()
	<-done
}

// a change that fails Validate, or that sets a secret, keeps nothing
func TestAChangeThatFailsItsCheckOrSetsASecretKeepsNothing(t *testing.T) {
	state := openTestState(t, t.TempDir())
	check := Validate(func(c appConfig) error {
		if c.Port < 1 || c.Port > 65535 {
			return fmt.Errorf("port %d is no port", c.Port)
		}
		return nil
	})
	config := openTestConfig(t, state, Defaults(appConfig{Port: 8080}), check)

	if err := config.Update(t.Context(), func(c *appConfig) { c.Port = 70000 }); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a port past 65535: %v", err)
	}
	if err := config.Update(t.Context(), func(c *appConfig) { c.DBURL = "leaked" }); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a secret changed: %v", err)
	}
	if kept, _ := (&RawConfig{hub: config.hub}).Kept(); len(kept) != 0 {
		t.Fatalf("refused changes kept %v", kept)
	}
	if port := config.Get().Port; port != 8080 {
		t.Fatalf("the config reads %d", port)
	}
	if _, err := OpenConfig[appConfig](t.Context(), state.Store, "bad", Defaults(appConfig{}), check); !errors.Is(err,
		tinystore.ErrInvalid) {
		t.Fatalf("defaults that fail their check: %v", err)
	}
}

// a kept value that no longer fits its field, from another program or an
// older version of this one, or that names a secret, is left out and named,
// and the rest still apply
func TestAKeptValueThatNoLongerFitsIsLeftOutAndNamed(t *testing.T) {
	state := openTestState(t, t.TempDir())
	raw, err := OpenRawConfig(t.Context(), state.Store, "app")
	if err != nil {
		t.Fatal(err)
	}
	kept := map[string]json.RawMessage{
		"port": json.RawMessage(`"not a number"`), "limits.rps": json.RawMessage(`7`),
		"gone": json.RawMessage(`true`), "dbUrl": json.RawMessage(`"leaked"`),
	}
	if err = raw.Change(t.Context(), kept, nil); err != nil {
		t.Fatal(err)
	}

	config := openTestConfig(t, state, Defaults(appConfig{Port: 8080}))
	if got := config.Get(); got.Port != 8080 || got.Limits.RPS != 7 || got.DBURL != "" {
		t.Fatalf("the config reads %+v", got)
	}
	sources := config.Sources()
	port := sources[slices.IndexFunc(sources, func(s Source) bool { return s.Path == "port" })]
	rps := sources[slices.IndexFunc(sources, func(s Source) bool { return s.Path == "limits.rps" })]
	secret := sources[slices.IndexFunc(sources, func(s Source) bool { return s.Path == "dbUrl" })]
	if !strings.Contains(port.Ignored, "is no int") || port.From != "default" || rps.From != "kept" || rps.Value != "7" ||
		secret.Value != "***" || secret.Ignored != "the field is a secret" {
		t.Fatalf("sources: port %+v, rps %+v, dbUrl %+v", port, rps, secret)
	}
}

// a variable is read by its field's type, and one that cannot be is refused,
// naming it
func TestAVariableIsReadByItsFieldsType(t *testing.T) {
	state := openTestState(t, t.TempDir())
	t.Setenv("PORT", "abc")
	_, err := OpenConfig[appConfig](t.Context(), state.Store, "app", FromEnv(""))
	if !errors.Is(err, tinystore.ErrInvalid) || !strings.Contains(err.Error(), "PORT") {
		t.Fatalf("PORT=abc: %v", err)
	}
	t.Setenv("PORT", "8081")
	t.Setenv("ORIGINS", `["a,b.com","c.com"]`)
	t.Setenv("LIMITS_ON", "true")
	config := openTestConfig(t, state, FromEnv(""))
	if got := config.Get(); got.Port != 8081 || !slices.Equal(got.Origins, []string{"a,b.com", "c.com"}) ||
		!got.Limits.On {
		t.Fatalf("the environment made %+v", got)
	}
}

// what a config may keep is bounded: a path, a value, and all of them
func TestARawConfigKeepsOnlyJSONWithinItsBounds(t *testing.T) {
	state := openTestState(t, t.TempDir())
	raw, err := OpenRawConfig(t.Context(), state.Store, "raw")
	if err != nil {
		t.Fatal(err)
	}
	for _, refused := range []struct {
		set  map[string]json.RawMessage
		kind error
	}{
		{map[string]json.RawMessage{"": json.RawMessage(`1`)}, tinystore.ErrInvalid},
		{map[string]json.RawMessage{"a": json.RawMessage(`{`)}, tinystore.ErrInvalid},
		{map[string]json.RawMessage{"a": json.RawMessage(`"` + strings.Repeat("x", maxConfigValue) + `"`)}, tinystore.ErrLimit},
	} {
		if err = raw.Change(t.Context(), refused.set, nil); !errors.Is(err, refused.kind) {
			t.Errorf("%v: %v", refused.set, err)
		}
	}
	if _, err = OpenBucket[int](t.Context(), state.Store, "raw"); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a config's name as a bucket: %v", err)
	}
}

// configVectors is testdata/config.json, which the Python SDK reads too
type configVectors struct {
	Names  [][3]string `json:"names"`
	Dotenv []struct {
		Name    string            `json:"name"`
		Text    string            `json:"text"`
		Want    map[string]string `json:"want"`
		Refused int               `json:"refused"`
	} `json:"dotenv"`
}

func TestVariablesAndDotenvFilesAreTheVectors(t *testing.T) {
	text, err := os.ReadFile(filepath.Join("testdata", "config.json"))
	if err != nil {
		t.Fatal(err)
	}
	var vectors configVectors
	if err = json.Unmarshal(text, &vectors); err != nil {
		t.Fatal(err)
	}
	for _, name := range vectors.Names {
		if got := envName(name[0], name[1]); got != name[2] {
			t.Errorf("%q, %q: %s, want %s", name[0], name[1], got, name[2])
		}
	}
	for _, v := range vectors.Dotenv {
		got, err := parseDotenv(v.Text)
		switch {
		case v.Refused != 0 && (err == nil || !strings.Contains(err.Error(), fmt.Sprintf("line %d:", v.Refused))):
			t.Errorf("%s: %v, want line %d refused", v.Name, err, v.Refused)
		case v.Refused == 0 && (err != nil || fmt.Sprint(got) != fmt.Sprint(v.Want)):
			t.Errorf("%s: %q, %v; want %q", v.Name, got, err, v.Want)
		}
	}
}

// a required field the layers leave empty keeps the config from opening, and
// the error names the variable that would give it
func TestARequiredFieldIsGivenOrTheConfigDoesNotOpen(t *testing.T) {
	type deployment struct {
		URL     string `json:"url" env:"REQUIRED_DATABASE_URL" secret:"true" required:"true"`
		Region  string `json:"region" required:"true"`
		Workers int    `json:"workers"`
	}
	state := openTestState(t, t.TempDir())
	t.Setenv("REQUIRED_DATABASE_URL", "")
	t.Setenv("REQUIRED_REGION", "eu")
	open := func(options ...ConfigOption) (*Config[deployment], error) {
		return OpenConfig[deployment](t.Context(), state.Store, "required", options...)
	}
	_, err := open(FromEnv("REQUIRED"))
	if !errors.Is(err, tinystore.ErrInvalid) || !strings.Contains(err.Error(), "url is required: set REQUIRED_DATABASE_URL") {
		t.Fatalf("an empty secret: %v", err)
	}
	if _, err = open(Defaults(deployment{URL: "postgres://default"})); !errors.Is(err, tinystore.ErrInvalid) ||
		!strings.HasSuffix(err.Error(), "region is required") {
		t.Fatalf("no environment: %v", err)
	}

	t.Setenv("REQUIRED_DATABASE_URL", "postgres://secret")
	config, err := open(FromEnv("REQUIRED"))
	if err != nil {
		t.Fatal(err)
	}
	if got := config.Get(); got.URL != "postgres://secret" || got.Region != "eu" {
		t.Fatalf("the config reads %+v", got)
	}
	if err = config.Update(t.Context(), func(d *deployment) { d.Region = "" }); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a required field emptied: %v", err)
	}
}

// each layer goes over the one before it in the order the options give them
func TestLayersGoOverEachOtherInTheOrderGiven(t *testing.T) {
	state := openTestState(t, t.TempDir())
	t.Setenv("ORDER_PORT", "3000")
	file := map[string]any{"port": 9000}
	for _, c := range []struct {
		options []ConfigOption
		want    int
	}{
		{[]ConfigOption{Defaults(file), FromEnv("ORDER")}, 3000},
		{[]ConfigOption{FromEnv("ORDER"), Defaults(file)}, 9000},
		{[]ConfigOption{Defaults(appConfig{Port: 8080})}, 8080},
	} {
		config, err := OpenConfig[appConfig](t.Context(), state.Store, "order", c.options...)
		if err != nil {
			t.Fatal(err)
		}
		if port := config.Get().Port; port != c.want {
			t.Errorf("port %d, want %d", port, c.want)
		}
	}
}

// a fixed field comes from the layers alone: Update refuses it, a kept value
// is left out, and Sources shows where it came from
func TestAFixedFieldRefusesUpdateAndSaysWhereItCameFrom(t *testing.T) {
	type settings struct {
		Addr     string `json:"addr" fixed:"true"`
		Instance string `json:"instanceName"`
	}
	state := openTestState(t, t.TempDir())
	t.Setenv("FIXED_ADDR", ":8080")
	config, err := OpenConfig[settings](t.Context(), state.Store, "fixed", FromEnv("FIXED"))
	if err != nil {
		t.Fatal(err)
	}
	err = config.Update(t.Context(), func(s *settings) { s.Addr = ":9000" })
	if !errors.Is(err, tinystore.ErrInvalid) || !strings.Contains(err.Error(), "addr is fixed, set by the layers alone") {
		t.Fatalf("an update of a fixed field: %v", err)
	}
	if err = config.Update(t.Context(), func(s *settings) { s.Instance = "eu-1" }); err != nil {
		t.Fatal(err)
	}
	if got := config.Get(); got.Addr != ":8080" || got.Instance != "eu-1" {
		t.Fatalf("the config reads %+v", got)
	}
	if err = config.hub.change(t.Context(), map[string][]byte{"addr": []byte(`":1"`)}, nil); err != nil {
		t.Fatal(err)
	}
	sources := config.Sources()
	if sources[0] != (Source{Path: "addr", Value: `":8080"`, From: "env FIXED_ADDR", Ignored: "the field is fixed"}) {
		t.Fatalf("addr's source is %+v", sources[0])
	}
}

// one restart shows every variable that does not read, and every required field missing
func TestEveryBadVariableIsReportedAtOnce(t *testing.T) {
	type settings struct {
		Port     int           `json:"port"`
		Shutdown time.Duration `json:"shutdownTimeout"`
		Debug    bool          `json:"debug"`
		Region   string        `json:"region" required:"true"`
		Zone     string        `json:"zone" required:"true"`
	}
	state := openTestState(t, t.TempDir())
	t.Setenv("BAD_PORT", "eighty")
	t.Setenv("BAD_SHUTDOWN_TIMEOUT", "5 sec")
	t.Setenv("BAD_DEBUG", "yes")
	_, err := OpenConfig[settings](t.Context(), state.Store, "bad", FromEnv("BAD"))
	if !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("bad variables: %v", err)
	}
	for _, want := range []string{`BAD_PORT: "eighty" is no int`, `BAD_SHUTDOWN_TIMEOUT: "5 sec" is no duration`, `BAD_DEBUG: "yes" is not true or false`} {
		if !strings.Contains(err.Error(), want) {
			t.Errorf("%v\nsays nothing of %s", err, want)
		}
	}

	t.Setenv("BAD_PORT", "80")
	t.Setenv("BAD_SHUTDOWN_TIMEOUT", "5s")
	t.Setenv("BAD_DEBUG", "true")
	_, err = OpenConfig[settings](t.Context(), state.Store, "bad", FromEnv("BAD"))
	if !errors.Is(err, tinystore.ErrInvalid) || !strings.Contains(err.Error(), "region is required: set BAD_REGION") ||
		!strings.Contains(err.Error(), "zone is required: set BAD_ZONE") {
		t.Fatalf("two required fields missing: %v", err)
	}
}

// NAME_FILE gives a field the file's text, as Docker and Kubernetes give a
// secret, its last newline taken off; NAME and NAME_FILE both set is refused
func TestASecretReadsItsFile(t *testing.T) {
	type settings struct {
		Password string `json:"smtpPassword" secret:"true"`
		Port     int    `json:"port"`
	}
	state := openTestState(t, t.TempDir())
	secret := filepath.Join(t.TempDir(), "smtp")
	if err := os.WriteFile(secret, []byte("hunter2\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("FILED_SMTP_PASSWORD_FILE", secret)
	config, err := OpenConfig[settings](t.Context(), state.Store, "filed", FromEnv("FILED"))
	if err != nil {
		t.Fatal(err)
	}
	if got := config.Get().Password; got != "hunter2" {
		t.Fatalf("the password is %q", got)
	}
	if source := config.Sources()[0]; source.From != "env FILED_SMTP_PASSWORD_FILE" || source.Value != "***" {
		t.Fatalf("the password's source is %+v", source)
	}

	t.Setenv("FILED_SMTP_PASSWORD", "other")
	t.Setenv("FILED_PORT_FILE", filepath.Join(t.TempDir(), "missing"))
	_, err = OpenConfig[settings](t.Context(), state.Store, "filed", FromEnv("FILED"))
	if !errors.Is(err, tinystore.ErrInvalid) ||
		!strings.Contains(err.Error(), "both FILED_SMTP_PASSWORD and FILED_SMTP_PASSWORD_FILE are set") ||
		!strings.Contains(err.Error(), "FILED_PORT_FILE: ") {
		t.Fatalf("both set and a missing file: %v", err)
	}
}

// FromLookup reads the host's function and nothing of the process
func TestAConfigsLookupIsTheOnlyEnvironmentRead(t *testing.T) {
	state := openTestState(t, t.TempDir())
	t.Setenv("LOOKED_PORT", "1")
	lookup := func(name string) (string, bool) {
		if name == "LOOKED_PORT" {
			return "3000", true
		}
		return "", false
	}
	config := openTestConfig(t, state, Defaults(appConfig{Port: 8080}), FromLookup("LOOKED", lookup))
	if got := config.Get().Port; got != 3000 {
		t.Fatalf("the port is %d", got)
	}
	if source := config.Sources()[0]; source.From != "env LOOKED_PORT" {
		t.Fatalf("the port's source is %+v", source)
	}
}
