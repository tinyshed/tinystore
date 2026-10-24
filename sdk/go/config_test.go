package tinystore_test

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/ways"
)

func is(t *testing.T, err error, kind error) {
	t.Helper()
	if !errors.Is(err, kind) {
		t.Fatalf("got %v, want %v", err, kind)
	}
}

func says(t *testing.T, err error, text string) {
	t.Helper()
	if err == nil || !strings.Contains(err.Error(), text) {
		t.Fatalf("got %v, want it to say %q", err, text)
	}
}

func TestASecretPrintsAsStarsWhereverItGoesAndRevealGivesIt(t *testing.T) {
	url := tinystore.NewSecret("postgres://user:password@host")
	type DB struct {
		URL tinystore.Secret `json:"url"`
	}
	var logged bytes.Buffer
	slog.New(slog.NewJSONHandler(&logged, nil)).Info("connect", "url", url, "db", DB{url})
	encoded, err := json.Marshal(DB{url})
	if err != nil {
		t.Fatal(err)
	}
	shown := []string{
		fmt.Sprint(url), fmt.Sprintf("%s %v %+v %#v %q %x", url, url, url, url, url, url),
		fmt.Sprintf("%v %+v %#v", DB{url}, DB{url}, &DB{url}), fmt.Sprint(map[string]any{"url": url}),
		fmt.Sprint([]tinystore.Secret{url}), fmt.Errorf("bad %v", url).Error(), string(encoded), logged.String(),
	}
	for _, text := range shown {
		if strings.Contains(text, "password") {
			t.Fatalf("a secret printed as %s", text)
		}
	}
	if url.Reveal() != "postgres://user:password@host" {
		t.Fatalf("reveal %q", url.Reveal())
	}
}

type db struct {
	URL  tinystore.Secret `json:"url" config:"required"`
	Pool int              `json:"pool"`
}

type limits struct {
	RPS   int `json:"rps" config:"live,min=0"`
	Burst int `json:"burst" config:"live"`
}

type settings struct {
	Port    int           `json:"port" config:"min=1,max=65535"`
	Level   string        `json:"level" config:"oneof=info debug warn error"`
	Timeout time.Duration `json:"timeout"`
	Origins []string      `json:"origins"`
	DB      db            `json:"db"`
	Limits  limits        `json:"limits"`
}

var defaults = settings{
	Port: 8080, Level: "info", Timeout: 30 * time.Second, Origins: []string{"localhost"},
	DB: db{Pool: 10}, Limits: limits{RPS: 100, Burst: 10},
}

func TestAConfigIsItsDeclarationThenEachLayerInTheOrderWritten(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		yaml := filepath.Join(t.TempDir(), "app.yaml")
		ways.Must(0, os.WriteFile(yaml, []byte("port: 8000\nlevel: debug\nlimits:\n  rps: 50\n"), 0o600))
		config := ways.Must(tinystore.OpenConfig(ctx, store, "ordered", defaults,
			tinystore.File(yaml),
			tinystore.Env("APP", tinystore.EnvFrom(map[string]string{"APP_PORT": "9000", "APP_DB_URL": "postgres://local"})),
		))
		value := config.Get()
		if value.Port != 9000 || value.Limits.RPS != 50 || value.Level != "debug" || value.Timeout != 30*time.Second {
			t.Fatalf("value %+v", value)
		}
		if value.DB.URL.Reveal() != "postgres://local" || value.DB.Pool != 10 || !reflect.DeepEqual(value.Origins, []string{"localhost"}) {
			t.Fatalf("db %v %d, origins %v", value.DB.URL.Reveal(), value.DB.Pool, value.Origins)
		}
		lines := map[string]tinystore.Line{}
		for _, line := range config.Explain() {
			lines[line.Path] = line
		}
		port, over := lines["port"], "8000 from "+yaml+", 8080 from the declaration"
		if port.Path != "port" || port.Value != "9000" || port.From != "APP_PORT" || port.Over != over {
			t.Fatalf("port %+v", port)
		}
		if lines["db.url"].Value != "***" || lines["timeout"].Value != "30s" {
			t.Fatalf("db.url %+v, timeout %+v", lines["db.url"], lines["timeout"])
		}
	})
}

// contract: CFG-09, CFG-06
func TestExplainGivesEveryValueASettingWasGivenNewestFirstWithWhereEachCameFrom(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		dir := t.TempDir()
		yaml, dotenv := filepath.Join(dir, "given.yaml"), filepath.Join(dir, "given.env")
		secret := filepath.Join(dir, "db-url")
		ways.Must(0, os.WriteFile(yaml, []byte("port: 8000\n"), 0o600))
		ways.Must(0, os.WriteFile(dotenv, []byte("APP_TIMEOUT=1m\n"), 0o600))
		ways.Must(0, os.WriteFile(secret, []byte("postgres://user:password@host\n"), 0o600))
		variables := map[string]string{"APP_PORT": "9000", "APP_DB_URL_FILE": secret}
		config := ways.Must(tinystore.OpenConfig(ctx, store, "given", defaults,
			tinystore.File(yaml),
			tinystore.Env("APP", tinystore.DotEnv(dotenv), tinystore.EnvFrom(variables)),
		))
		before := time.Now()
		ways.Must(0, config.Update(ctx, func(s *settings) { s.Limits.RPS = 200 }))
		lines := map[string]tinystore.Line{}
		for _, line := range config.Explain() {
			lines[line.Path] = line
		}
		want := map[string][]tinystore.Given{
			"port": {
				{Value: "9000", From: envOrigin("APP_PORT", "", "")},
				{Value: "8000", From: tinystore.Origin{Kind: tinystore.OriginFile, Path: yaml}},
				declared("8080"),
			},
			"timeout": {{Value: "1m", From: envOrigin("APP_TIMEOUT", dotenv, "")}, declared("30s")},
			"db.url":  {{Value: "***", From: envOrigin("APP_DB_URL_FILE", "", secret)}},
		}
		for path, given := range want {
			if !reflect.DeepEqual(lines[path].Given, given) {
				t.Fatalf("%s was given %+v, want %+v", path, lines[path].Given, given)
			}
		}
		changed(t, lines["limits.rps"].Given, config.Version(), before)
		givenAsText(t, config.Explain())
		if text := fmt.Sprintf("%+v", lines["db.url"].Given); strings.Contains(text, "password") {
			t.Fatalf("a secret printed as %s", text)
		}
	})
}

func declared(value string) tinystore.Given {
	return tinystore.Given{Value: value, From: tinystore.Origin{Kind: tinystore.OriginDeclaration}}
}

func envOrigin(variable, dotenv, file string) tinystore.Origin {
	return tinystore.Origin{Kind: tinystore.OriginEnv, Variable: variable, DotEnv: dotenv, File: file}
}

// changed checks that a change gave limits.rps 200 over the declaration's 100,
// at the config's version and after before.
func changed(t *testing.T, given []tinystore.Given, version int64, before time.Time) {
	t.Helper()
	if len(given) != 2 || given[0].Value != "200" || given[1] != declared("100") {
		t.Fatalf("limits.rps was given %+v", given)
	}
	change := given[0].From
	if change.Kind != tinystore.OriginChange || change.Version != version {
		t.Fatalf("a change %+v, at version %d", change, version)
	}
	if change.At.Before(before.Add(-time.Second)) || change.At.After(time.Now().Add(time.Second)) {
		t.Fatalf("a change at %v, made after %v", change.At, before)
	}
}

// givenAsText checks that each line's From and Over are its Given as text.
func givenAsText(t *testing.T, lines []tinystore.Line) {
	t.Helper()
	for _, line := range lines {
		if len(line.Given) == 0 {
			t.Fatalf("%s was given nothing", line.Path)
		}
		own, over := line.Given[0], line.Given[1:]
		if own.Value != line.Value || own.From.String() != line.From {
			t.Fatalf("%s: %+v, but the line says %q from %q", line.Path, own, line.Value, line.From)
		}
		said := make([]string, len(over))
		for i, given := range over {
			said[i] = given.Value + " from " + given.From.String()
		}
		if strings.Join(said, ", ") != line.Over {
			t.Fatalf("%s went over %q, but the line says %q", line.Path, said, line.Over)
		}
	}
}

func TestAnOriginOfAKindFromANewerServerKeepsItsKindWhichIsAlsoItsText(t *testing.T) {
	if text := (tinystore.Origin{Kind: "rule"}).String(); text != "rule" {
		t.Fatalf("an origin of the kind rule says %q", text)
	}
}

func TestARuleOfOneValueIsTheCoresAndARefusalNamesTheLayerThatGaveIt(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		opening := func(given map[string]string) (*tinystore.Config[settings], error) {
			variables := map[string]string{"APP_DB_URL": "u"}
			for name, value := range given {
				variables[name] = value
			}
			return tinystore.OpenConfig(ctx, store, "rules", defaults, tinystore.Env("APP", tinystore.EnvFrom(variables)))
		}
		_, err := opening(map[string]string{"APP_PORT": "70000", "APP_LEVEL": "verbose"})
		is(t, err, tinystore.ErrInvalid)
		says(t, err, "APP_PORT: 70000 is above 65535")
		says(t, err, `APP_LEVEL: "verbose" is not one of info, debug, warn, error`)
		config := ways.Must(opening(map[string]string{"APP_TIMEOUT": "1m30s", "APP_ORIGINS": "a.com, b.com"}))
		if value := config.Get(); value.Timeout != 90*time.Second || !reflect.DeepEqual(value.Origins, []string{"a.com", "b.com"}) {
			t.Fatalf("value %+v", value)
		}
	})
}

type needed struct {
	Region  string           `json:"region" config:"required"`
	Workers uint8            `json:"workers" config:"required,min=1"`
	Token   tinystore.Secret `json:"token" config:"required"`
	DSN     tinystore.Secret `json:"dsn"`
}

func TestARequiredSettingIsGivenByALayerOrTheConfigDoesNotOpenNamingWhatWould(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		opening := func(given map[string]string) (*tinystore.Config[needed], error) {
			return tinystore.OpenConfig(ctx, store, "required", needed{}, tinystore.Env("APP", tinystore.EnvFrom(given)))
		}
		_, err := opening(map[string]string{"APP_REGION": ""})
		says(t, err, "region is required: set APP_REGION")
		says(t, err, "token is required: set APP_TOKEN")
		_, err = opening(map[string]string{"APP_REGION": "eu", "APP_WORKERS": "300", "APP_TOKEN": "t"})
		says(t, err, "APP_WORKERS: 300 is above 255")
		config := ways.Must(opening(map[string]string{"APP_REGION": "eu", "APP_WORKERS": "4", "APP_TOKEN": "t"}))
		if value := config.Get(); value.Region != "eu" || value.Workers != 4 || value.Token.Reveal() != "t" || value.DSN.Reveal() != "" {
			t.Fatalf("value %+v", value)
		}
	})
}

type port struct {
	Port int `json:"port"`
}

func TestAFileAbsentIsRefusedUnlessOptionalAndAKeyNoSettingHasIsNamed(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		dir := t.TempDir()
		missing := filepath.Join(dir, "missing.toml")
		_, err := tinystore.OpenConfig(ctx, store, "files", port{8080}, tinystore.File(missing))
		says(t, err, "no such file")
		config := ways.Must(tinystore.OpenConfig(ctx, store, "files", port{8080}, tinystore.File(missing, tinystore.Optional())))
		if notes := config.Notes(); !reflect.DeepEqual(notes, []string{missing + ": absent"}) {
			t.Fatalf("notes %q", notes)
		}
		jsonc := filepath.Join(dir, "app.jsonc")
		ways.Must(0, os.WriteFile(jsonc, []byte("{\n  // a comment\n  \"prot\": 1,\n}\n"), 0o600))
		_, err = tinystore.OpenConfig(ctx, store, "files", port{8080}, tinystore.File(jsonc))
		says(t, err, "prot: no such setting")
	})
}

type otherLimits struct {
	RPS   int `json:"rps" config:"live"`
	Burst int `json:"burst" config:"live"`
}

type other struct {
	Limits otherLimits `json:"limits"`
}

func TestALiveSettingChangesForEveryHandleAndResetGivesTheLayersBack(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		env := tinystore.Env("APP", tinystore.EnvFrom(map[string]string{"APP_DB_URL": "u", "APP_LIMITS_RPS": "1000"}))
		config := ways.Must(tinystore.OpenConfig(ctx, store, "live", defaults, env))
		theirs := ways.Must(tinystore.OpenConfig(ctx, store, "live", other{otherLimits{1, 1}}))
		ways.Must(0, config.Update(ctx, func(s *settings) { s.Limits.RPS = 200 }))
		if config.Get().Limits.RPS != 200 {
			t.Fatalf("rps %d", config.Get().Limits.RPS)
		}
		var rps tinystore.Line
		for _, line := range config.Explain() {
			if line.Path == "limits.rps" {
				rps = line
			}
		}
		if !strings.HasPrefix(rps.From, "a change, version ") || rps.Over != "1000 from APP_LIMITS_RPS, 100 from the declaration" {
			t.Fatalf("explain %+v", rps)
		}
		ways.Until(t, func() bool { return theirs.Get().Limits.RPS == 200 })
		err := config.Update(ctx, func(s *settings) { s.Port = 1 })
		says(t, err, "port is not live")
		ways.Must(0, config.Reset(ctx, "limits.rps"))
		if config.Get().Limits.RPS != 1000 {
			t.Fatalf("rps %d after reset", config.Get().Limits.RPS)
		}
		ways.Until(t, func() bool { return theirs.Get().Limits.RPS == 1 })
		is(t, config.Reset(ctx, "limits.rsp"), tinystore.ErrInvalid)
	})
}

type two struct {
	A int `json:"a" config:"live"`
	B int `json:"b" config:"live"`
}

type three struct {
	A int `json:"a" config:"live"`
	B int `json:"b" config:"live"`
	C int `json:"c"`
}

func TestIfVersionRefusesAChangeMadeSinceAndTwoChangesOfDifferentSettingsBothStay(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		first := ways.Must(tinystore.OpenConfig(ctx, store, "versions", two{1, 2}))
		second := ways.Must(tinystore.OpenConfig(ctx, store, "versions", three{1, 2, 3}))
		shown := first.Version()
		ways.Must(0, first.Update(ctx, func(s *two) { s.A = 10 }))
		ways.Must(0, second.Update(ctx, func(s *three) { s.B = 20 }))
		ways.Until(t, func() bool { return first.Get().B == 20 && second.Get().A == 10 })
		is(t, first.Update(ctx, func(s *two) { s.A = 30 }, tinystore.IfVersion(shown)), tinystore.ErrConflict)
		ways.Must(0, first.Update(ctx, func(s *two) { s.A = 40 }, tinystore.IfVersion(first.Version())))
		if first.Get().A != 40 {
			t.Fatalf("a %d", first.Get().A)
		}
		// a change runs on the value as it is now, which another handle changed
		ways.Must(0, first.Update(ctx, func(s *two) { s.B += 1 }))
		if first.Get().B != 21 {
			t.Fatalf("b %d", first.Get().B)
		}
	})
}

type rate struct {
	RPS int `json:"rps" config:"live"`
}

func TestAWatchGivesTheSettingsNowThenEachChangeUntilItsContextEnds(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		config := ways.Must(tinystore.OpenConfig(ctx, store, "watched", rate{100}))
		var seen []int
		for s := range config.Watch(ctx) {
			seen = append(seen, s.RPS)
			switch len(seen) {
			case 1:
				ways.Must(0, config.Update(ctx, func(s *rate) { s.RPS = 200 }))
			case 2:
				ways.Must(0, config.Update(ctx, func(s *rate) { s.RPS = 300 }))
				ways.Must(0, config.Update(ctx, func(s *rate) { s.RPS = 400 }))
			}
			if len(seen) == 3 {
				break
			}
		}
		if !reflect.DeepEqual(seen, []int{100, 200, 400}) {
			t.Fatalf("seen %v", seen)
		}
	})
}

func TestValidateRefusesAValueAtOpenAndAChangeItRefusesKeepsNothing(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		atMost := func(most int) tinystore.ConfigOption {
			return tinystore.Validate(func(s rate) error {
				if s.RPS > most {
					return fmt.Errorf("rps past %d", most)
				}
				return nil
			})
		}
		_, err := tinystore.OpenConfig(ctx, store, "validated", rate{5000}, atMost(1000))
		is(t, err, tinystore.ErrInvalid)
		says(t, err, "rps past 1000")
		config := ways.Must(tinystore.OpenConfig(ctx, store, "validated", rate{100}, atMost(1000)))
		says(t, config.Update(ctx, func(s *rate) { s.RPS = 2000 }), "rps past 1000")
		if config.Get().RPS != 100 {
			t.Fatalf("rps %d", config.Get().RPS)
		}
		_, err = tinystore.OpenConfig(ctx, store, "validated", rate{100}, tinystore.Validate(func(port) error { return nil }))
		is(t, err, tinystore.ErrInvalid)
	})
}

type upstream struct {
	Host string `json:"host" config:"required"`
	Port int    `json:"port"`
}

type lists struct {
	Hosts     []string   `json:"hosts" config:"items=-2"`
	Upstreams []upstream `json:"upstreams"`
	Ports     []uint16   `json:"ports" config:"min=1"`
}

func TestAListChecksEachItemAndAListOfGroupsNeedsItsRequiredFields(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		opening := func(given map[string]string) (*tinystore.Config[lists], error) {
			return tinystore.OpenConfig(ctx, store, "lists", lists{Hosts: []string{"localhost"}}, tinystore.Env("APP", tinystore.EnvFrom(given)))
		}
		config := ways.Must(opening(map[string]string{"APP_UPSTREAMS": `[{"host":"a.com","port":8080}]`, "APP_PORTS": "80,443"}))
		value := config.Get()
		if !reflect.DeepEqual(value.Upstreams, []upstream{{"a.com", 8080}}) || !reflect.DeepEqual(value.Hosts, []string{"localhost"}) ||
			!reflect.DeepEqual(value.Ports, []uint16{80, 443}) {
			t.Fatalf("value %+v", value)
		}
		_, err := opening(map[string]string{"APP_HOSTS": "a,b,c"})
		says(t, err, "APP_HOSTS: 3 items is above 2 items")
		_, err = opening(map[string]string{"APP_UPSTREAMS": `[{"port":1}]`})
		says(t, err, "host is required")
		_, err = opening(map[string]string{"APP_PORTS": "0"})
		is(t, err, tinystore.ErrInvalid)
	})
}

func TestADeclarationTheCoreCannotKeepIsRefusedWhereItIsWritten(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		type pointed struct {
			Region *string `json:"region"`
		}
		_, err := tinystore.OpenConfig(ctx, store, "refused", pointed{})
		says(t, err, "one type")
		type livedSecret struct {
			Key tinystore.Secret `json:"key" config:"live"`
		}
		_, err = tinystore.OpenConfig(ctx, store, "refused", livedSecret{})
		says(t, err, "a secret is never live")
		_, err = tinystore.OpenConfig(ctx, store, "refused", 8080)
		says(t, err, "declaration")
		type marked struct {
			Port int `json:"port" config:"lively"`
		}
		_, err = tinystore.OpenConfig(ctx, store, "refused", marked{})
		is(t, err, tinystore.ErrInvalid)
	})
}

func TestAStoreClosedAsItsConfigBeginsToFollowLetsGoOfWhatServedItAtOnce(t *testing.T) {
	ways.EachWay(t, func(t *testing.T, way ways.Way) {
		store := way.Open(t, t.TempDir())
		ways.Must(tinystore.OpenConfig(context.Background(), store, "app", port{8080}))
		started := time.Now()
		ways.Must(0, store.Close())
		if took := time.Since(started); took > 3*time.Second {
			t.Fatalf("the close took %s", took)
		}
	})
}
