package records

import (
	"context"
	"encoding/json"
	"log/slog"
	"math"
	"sync"

	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/logline"
	"tinyshed.org/tinystore/internal/wire"
)

// A store's handler reads its options from the config log, so that a change
// of its level reaches the handler of every process of the store at once.
// The code's options are the declaration's defaults, and the variables it
// read, LOG_LEVEL, LOG_FORMAT and LOG_TIME, go over them; only the level is
// live. Records and the config meet here, as Rust's log_config does:
//
//	level   live, one of trace, debug, info, warn, error   the code's, or trace: every level
//	format  one of pretty, json, off                       the code's, or what its console chose
//	time    one of clock, full, off                        the code's, or clock

// logConfig is the config every store's handler reads.
const logConfig = "log"

// unfollowed says once a process that a handler follows no config log.
var unfollowed sync.Once

// leveled is a handler's level, which the config log sets once it opens: the
// code's and the environment's until then, every level for a handler told none.
func leveled(told logline.Told) *slog.LevelVar {
	level := new(slog.LevelVar)
	if told.Level != nil {
		level.Set(told.Level.Level())
	} else {
		level.Set(slog.Level(math.MinInt32))
	}
	return level
}

// followLog opens the config log as told declares it, in the background,
// since a handler is made without waiting, and sets level from each value.
func followLog(store *core.Store, told logline.Told, level *slog.LevelVar) {
	message := wire.ConfigOpen{Name: logConfig, Declaration: string(logDeclaration(told)), Layers: string(logLayers(told))}
	go func() {
		config, err := core.SharedConfig(context.Background(), store, message)
		if err != nil {
			if !store.Link.Closed() {
				unfollowed.Do(func() { core.SayOwn("WARN", "a logger follows no config log", "error", err) })
			}
			return
		}
		config.OnChange(func(value json.RawMessage) {
			var log struct {
				Level string `json:"level"`
			}
			if json.Unmarshal(value, &log) != nil {
				return
			}
			if named, ok := logline.ReadLevel(log.Level); ok {
				level.Set(named)
			}
		})
	}()
}

// logDeclaration is the config log as a handler declares it, each set its
// default first, as every SDK's oneOf lists it.
func logDeclaration(told logline.Told) []byte {
	level := "trace"
	if told.Code.Level != nil {
		level = logline.LevelName(told.Code.Level.Level())
	}
	shown, time := logline.Shown(told.Code), logline.TimeName(told.Code.Time)
	setting := func(given string, every []string, live bool) map[string]any {
		allowed := []string{given}
		for _, name := range every {
			if name != given {
				allowed = append(allowed, name)
			}
		}
		out := map[string]any{"kind": "text", "oneOf": allowed, "default": given}
		if live {
			out["live"] = true
		}
		return out
	}
	declaration, _ := json.Marshal(map[string]any{
		"level":  setting(level, logline.LevelNames, true),
		"format": setting(shown, []string{"pretty", "json", "off"}, false),
		"time":   setting(time, []string{"clock", "full", "off"}, false),
	})
	return declaration
}

// logLayers is the environment the handler read, LOG_ after its prefix,
// none when it reads none.
func logLayers(told logline.Told) []byte {
	if told.Code.Env.Lookup == nil {
		return []byte("[]")
	}
	prefix := "LOG"
	if told.Code.Env.Prefix != "" {
		prefix = told.Code.Env.Prefix + "_LOG"
	}
	layers, _ := json.Marshal([]any{map[string]any{"env": map[string]any{"prefix": prefix, "variables": told.Spelled}}})
	return layers
}
