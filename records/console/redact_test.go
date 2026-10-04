package console

import (
	"bytes"
	"log/slog"
	"slices"
	"strings"
	"testing"
)

func TestWordsAreAsAPersonReadsThem(t *testing.T) {
	for key, want := range map[string]string{
		"DB_PASSWORD":       "db password",
		"PasswordHash":      "password hash",
		"APIKey":            "api key",
		"apiKey":            "api key",
		"APIKEY":            "apikey",
		"signing-key":       "signing key",
		"db.password":       "db password",
		"oauth2Token":       "oauth2 token",
		"connection string": "connection string",
		"":                  "",
	} {
		if got := strings.Join(words(key), " "); got != want {
			t.Errorf("%q has the words %q, want %q", key, got, want)
		}
	}
}

// Dashbin's list: every spelling of a secret is hidden by Secrets
func TestRedactHidesASecretWhateverItsKeySpelling(t *testing.T) {
	r := newRedactor(Secrets, false)
	for _, key := range []string{
		"password", "DB_PASSWORD", "passwd", "secret", "bot_token", "credential", "Authorization",
		"api_key", "apikey", "private_key", "dsn", "accessKey", "connection_string", "PasswordHash", "signing-key",
		"x-api-key", "client_secret", "Set-Cookie", "db.password",
	} {
		if !r.hides(key) {
			t.Errorf("%s is shown", key)
		}
	}
}

func TestACounterNamedLikeASecretStaysVisible(t *testing.T) {
	r := newRedactor(Secrets, false)
	for _, key := range []string{"tokens_used", "passwords", "secretary", "keyboard", "author", "user"} {
		if r.hides(key) {
			t.Errorf("%s is hidden", key)
		}
	}
	if !newRedactor([]string{"token"}, false).hides("token_count") {
		t.Error("token hides token_count, a word of its own")
	}
}

func TestAPasswordInsideAURLIsHidden(t *testing.T) {
	for text, want := range map[string]string{
		`"postgres://dashbin:hunter2@db.internal:5432/app"`:                 `"postgres://dashbin:[redacted]@db.internal:5432/app"`,
		`"see https://ann:p%40ss@example.com/x and redis://:pw@cache:6379"`: `"see https://ann:[redacted]@example.com/x and redis://:[redacted]@cache:6379"`,
		`{"source":"mysql://u:p@h/db","n":1}`:                               `{"source":"mysql://u:[redacted]@h/db","n":1}`,
		`"https://ann@example.com/x"`:                                       `"https://ann@example.com/x"`,
		`"https://example.com:8443/x?user=a:b@c"`:                           `"https://example.com:8443/x?user=a:b@c"`,
		`"user:pass@host, no scheme"`:                                       `"user:pass@host, no scheme"`,
		`"ftp://u:@host"`:                                                   `"ftp://u:@host"`,
		`"://u:p@host"`:                                                     `"://u:p@host"`,
	} {
		if got := hideURLPasswords(text); got != want {
			t.Errorf("%s\n got %s\nwant %s", text, got, want)
		}
	}
}

// a logger hides a URL's password unless told to keep it, in the store and on the console alike
func TestAURLsPasswordIsHiddenUnlessKept(t *testing.T) {
	var hidden, kept bytes.Buffer
	slog.New(Handler("api", JSON, writeTo{&hidden})).Info("connect", "source", "postgres://ann:hunter2@db/app")
	slog.New(Handler("api", JSON, KeepURLPasswords, writeTo{&kept})).Info("connect", "source", "postgres://ann:hunter2@db/app")
	if !strings.Contains(hidden.String(), `"source":"postgres://ann:[redacted]@db/app"`) {
		t.Errorf("hidden: %s", hidden.String())
	}
	if !strings.Contains(kept.String(), `"source":"postgres://ann:hunter2@db/app"`) {
		t.Errorf("kept: %s", kept.String())
	}
}

// ReplaceAttr sees each attribute with its groups, as slog's handlers call it
func TestReplaceAttrChangesAndDropsAttributes(t *testing.T) {
	var console bytes.Buffer
	var seen []string
	replace := func(groups []string, a slog.Attr) slog.Attr {
		seen = append(seen, strings.Join(append(slices.Clone(groups), a.Key), "."))
		switch a.Key {
		case "email":
			return slog.String(a.Key, "a***@example.com")
		case "internal":
			return slog.Attr{}
		}
		return a
	}
	logger := slog.New(Handler("api", JSON, ReplaceAttr(replace), writeTo{&console})).With("email", "ann@example.com")
	logger.WithGroup("req").Info("signed in", "internal", 1, slog.Group("user", "id", 7))
	want := `"email":"a***@example.com","req.user.id":7}`
	if !strings.HasSuffix(strings.TrimSpace(console.String()), want) {
		t.Errorf("the console has %s, want it to end %s", console.String(), want)
	}
	if strings.Join(seen, " ") != "email req.internal req.user.id" {
		t.Errorf("ReplaceAttr saw %q", seen)
	}
}

// AddSource says where the line was logged: the record keeps slog's spelling,
// a pretty line its directory, file and line
func TestASourceIsWhereTheLineWasLogged(t *testing.T) {
	var lines, pretty bytes.Buffer
	slog.New(Handler("api", JSON, AddSource, writeTo{&lines})).Info("started")
	slog.New(Handler("api", Pretty, TimeOff, AddSource, writeTo{&pretty})).Info("started")
	if !strings.Contains(lines.String(), `"source":{"function":"github.com/tinyshed/tinystore/records/console.TestASourceIsWhereTheLineWasLogged","file":"`) ||
		!strings.Contains(lines.String(), `redact_test.go","line":`) {
		t.Errorf("JSON: %s", lines.String())
	}
	if !strings.HasPrefix(pretty.String(), "INFO  api  started  source=console/redact_test.go:") {
		t.Errorf("pretty: %s", pretty.String())
	}
}
