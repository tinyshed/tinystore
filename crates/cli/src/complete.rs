//! Tab's completion: `tinystore completion <shell>` prints the lines a shell
//! sources, which ask `tinystore __complete <words>` on each Tab, so that
//! bash, zsh, fish and PowerShell offer the same words: commands, verbs,
//! flags and a flag's values from the help the parser reads, and the names
//! a store holds, read beside it. It makes no store, writes nothing, waits
//! for no server, and offers nothing when anything fails.
//!
//! ```text
//! tinystore __complete kv ./data get +se                      sessions
//! tinystore __complete metrics ./data read rtt --where +t     target=
//! tinystore __complete files ./data put avatars a.png +       @files
//! ```
//!
//! The word being completed comes last, after a `+`, since Windows
//! PowerShell 5.1 drops an empty argument. `@dirs` and `@files` ask the
//! shell for its own directories or files. When no word starts with what
//! was typed, the words it is a typo of are offered instead, as a misspelt
//! command is told its nearest: `get sesions` gives `sessions`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::asked::{self, Asked, Done, wrong};
use crate::help::{self, Command};
use crate::names::Names;

const SHELLS: [&str; 4] = ["bash", "zsh", "fish", "powershell"];

pub(crate) fn completion(args: &[String]) -> ExitCode {
    asked::run(&help::COMPLETION, args, script)
}

fn script(asked: Asked) -> Done {
    let script = match asked.words.as_slice() {
        [shell] if shell == "bash" => BASH,
        [shell] if shell == "zsh" => ZSH,
        [shell] if shell == "fish" => FISH,
        [shell] if shell == "powershell" || shell == "pwsh" => POWERSHELL,
        _ => return Err(wrong(format!("completion takes a shell: {}", SHELLS.join(", ")))),
    };
    out!("{script}")
}

/// Prints what may come in place of the last word, a line each.
pub(crate) fn run(args: &[String]) -> ExitCode {
    let Some((current, before)) = args.split_last() else { return ExitCode::SUCCESS };
    let current = current.strip_prefix('+').unwrap_or(current);
    for word in shown(before, current) {
        if outln!("{word}").is_err() {
            break;
        }
    }
    ExitCode::SUCCESS
}

/// What Tab offers after `before`: the words that start with `current`, or,
/// when none does, those whose start it is a typo of; and the shell's own
/// directories or files when they may come.
fn shown(before: &[String], current: &str) -> Vec<String> {
    let (asked, words): (Vec<String>, Vec<String>) =
        candidates(before, current).into_iter().partition(|word| word.starts_with('@'));
    let mut starting: Vec<String> = words.iter().filter(|word| word.starts_with(current)).cloned().collect();
    if starting.is_empty() {
        let edits = crate::typo_edits(current);
        starting = words.into_iter().filter(|word| edits > 0 && typo_of(current, word, edits)).collect();
    }
    starting.into_iter().chain(asked).collect()
}

/// Whether `typed` is within `edits` of the start of `word`: of a start one
/// character shorter or longer too, since a typo may drop or add one.
fn typo_of(typed: &str, word: &str, edits: usize) -> bool {
    let (typed, word): (Vec<char>, Vec<char>) = (typed.chars().collect(), word.chars().collect());
    let shortest = typed.len().saturating_sub(edits);
    (shortest..=(typed.len() + edits).min(word.len())).any(|end| distance(&typed, &word[..end]) <= edits)
}

/// The edits that make `a` into `b`: a character added, dropped, changed,
/// or two side by side swapped.
fn distance(a: &[char], b: &[char]) -> usize {
    let mut rows = vec![(0..=b.len()).collect::<Vec<usize>>()];
    for (i, &left) in a.iter().enumerate() {
        let mut row = vec![i + 1; b.len() + 1];
        for (j, &right) in b.iter().enumerate() {
            let changed = rows[i][j] + usize::from(left != right);
            row[j + 1] = changed.min(rows[i][j + 1] + 1).min(row[j] + 1);
            if i > 0 && j > 0 && left == b[j - 1] && a[i - 1] == right {
                row[j + 1] = row[j + 1].min(rows[i - 1][j - 1] + 1);
            }
        }
        rows.push(row);
    }
    rows[a.len()][b.len()]
}

/// The words that may come after `before`, every one, `shown` keeping those
/// that fit what was typed.
fn candidates(before: &[String], current: &str) -> Vec<String> {
    let Some(first) = before.first() else {
        let mut words = help::first_words();
        words.push("completion");
        return words.into_iter().map(str::to_owned).collect();
    };
    if let Some(values) = before.last().and_then(|flag| flag_values(before, flag, current)) {
        return values;
    }
    if help::is_group(first) {
        return in_group(first, &before[1..], current);
    }
    let command =
        before.get(1).and_then(|second| help::command(&format!("{first} {second}"))).or_else(|| help::command(first));
    if current.starts_with('-') {
        return command.map(flags_of).unwrap_or_default();
    }
    match first.as_str() {
        "help" => help::first_words().into_iter().map(str::to_owned).collect(),
        "completion" => SHELLS.iter().map(|shell| (*shell).to_owned()).collect(),
        "sql" | "schema" if positional(&before[1..]).len() == 1 => {
            on_disk_databases(Path::new(&positional(&before[1..])[0]))
        }
        _ => vec!["@dirs".to_owned()],
    }
}

/// What may come after a group's word: its directory and its verbs, then
/// what the verb takes.
fn in_group(group: &str, after: &[String], current: &str) -> Vec<String> {
    let verbs: Vec<&Command> = verbs_of(group);
    let words = positional(after);
    let at = words.iter().position(|word| verbs.iter().any(|verb| verb_word(verb) == *word));
    let Some(at) = at else {
        let mut offered: Vec<String> = verbs.iter().map(|verb| verb_word(verb).to_owned()).collect();
        if words.is_empty() {
            offered.push("@dirs".to_owned());
        }
        return offered;
    };
    let verb = verbs.iter().find(|verb| verb_word(verb) == words[at]).copied();
    if current.starts_with('-') {
        return verb.map(flags_of).unwrap_or_default();
    }
    let dir = if at == 1 { PathBuf::from(&words[0]) } else { PathBuf::from(".") };
    named(group, &words[at], &words[at + 1..], &dir)
}

/// The names a verb's next word may be, read from the store at `dir`.
fn named(group: &str, verb: &str, given: &[String], dir: &Path) -> Vec<String> {
    let names = || Names::of(dir);
    match (group, verb, given.len()) {
        ("kv", _, 0) => {
            let buckets = names().buckets.into_iter();
            buckets.filter(|(_, role)| role == "values" || role == "encrypted values").map(|(name, _)| name).collect()
        }
        ("jobs", _, 0) => names().queues,
        ("files", _, 0) => names().files,
        ("files", "put" | "get", 2) => vec!["@files".to_owned()],
        ("events", _, 0) => names().streams,
        ("config", _, 0) => names().configs,
        ("config", "update" | "reset", _) => settings_of(dir, &given[0], verb == "update"),
        ("metrics", "read" | "aggregate" | "delete", 0) => names().metrics,
        ("metrics", "delete", _) => labels_of(dir, &given[0]).into_iter().map(|label| format!("{label}=")).collect(),
        _ => Vec::new(),
    }
}

/// The values a flag that takes one may be given, when `flag` is one.
fn flag_values(before: &[String], flag: &str, current: &str) -> Option<Vec<String>> {
    let words = |list: &[&str]| Some(list.iter().map(|word| (*word).to_owned()).collect());
    match flag {
        "--as" => words(&["text", "int", "json"]),
        "--state" => words(&["scheduled", "waiting", "running", "done", "failed"]),
        "--level" => words(&["trace", "debug", "info", "warn", "error"]),
        "--op" => {
            words(&["count", "sum", "avg", "min", "max", "increase", "rate", "delta", "first", "last", "p95", "p99"])
        }
        "--kind" => {
            let (name, _) = current.split_once('=')?;
            Some(["counter", "gauge", "histogram"].iter().map(|kind| format!("{name}={kind}")).collect())
        }
        "--database" => Some(on_disk_databases(&dir_of(before))),
        "--where" | "--by" => {
            let (dir, name) = (dir_of(before), metric_of(before)?);
            let labels = labels_of(&dir, &name);
            match current.split_once('=') {
                Some((label, _)) => {
                    Some(values_of(&dir, &name, label).into_iter().map(|value| format!("{label}={value}")).collect())
                }
                None if flag == "--by" => Some(labels),
                None => Some(labels.into_iter().map(|label| format!("{label}=")).collect()),
            }
        }
        _ => None,
    }
}

/// The directory a group's words name: the word before its verb, or the
/// current one.
fn dir_of(before: &[String]) -> PathBuf {
    let words = positional(&before[1..]);
    let verbs = before.first().map(|group| verbs_of(group)).unwrap_or_default();
    match words.first() {
        Some(word) if !verbs.iter().any(|verb| verb_word(verb) == *word) => PathBuf::from(word),
        _ => PathBuf::from("."),
    }
}

/// The metric a `metrics read` or `aggregate` names: the word after its verb.
fn metric_of(before: &[String]) -> Option<String> {
    let words = positional(&before[1..]);
    let at = words.iter().position(|word| word == "read" || word == "aggregate" || word == "delete")?;
    words.get(at + 1).cloned()
}

/// The settings of the config `name` as the store keeps it, each with an
/// `=` after it when a value follows.
fn settings_of(dir: &Path, name: &str, valued: bool) -> Vec<String> {
    let Some(store) = beside(dir) else { return Vec::new() };
    let Ok(Some(config)) = tinystore::client::config_kept(&store, name) else { return Vec::new() };
    let paths = config.explain().settings.into_iter().map(|line| line.path);
    paths.map(|path| if valued { format!("{path}=") } else { path }).collect()
}

fn labels_of(dir: &Path, name: &str) -> Vec<String> {
    // the store stays open while its metrics read: they hold it weakly
    let Some(store) = beside(dir) else { return Vec::new() };
    let Ok(metrics) = store.metrics() else { return Vec::new() };
    metrics.label_names(name).page().map(|page| page.labels).unwrap_or_default()
}

fn values_of(dir: &Path, name: &str, label: &str) -> Vec<String> {
    let Some(store) = beside(dir) else { return Vec::new() };
    let Ok(metrics) = store.metrics() else { return Vec::new() };
    metrics.label_values(name, label).page().map(|page| page.values).unwrap_or_default()
}

fn beside(dir: &Path) -> Option<tinystore::Store> {
    dir.join("LOCK").is_file().then(|| tinystore::Store::open_beside(dir).ok()).flatten()
}

fn on_disk_databases(dir: &Path) -> Vec<String> {
    crate::on_disk::databases(dir)
}

/// The words that are no flag and no flag's value.
fn positional(words: &[String]) -> Vec<String> {
    let mut found = Vec::new();
    let mut skip = false;
    for word in words {
        if std::mem::take(&mut skip) {
            continue;
        }
        if word.starts_with('-') && word.len() > 1 && word.parse::<f64>().is_err() {
            skip = !word.contains('=') && takes_value(word);
            continue;
        }
        found.push(word.clone());
    }
    found
}

/// Whether some command's flag of this name takes a value.
fn takes_value(flag: &str) -> bool {
    help::GROUPS
        .iter()
        .flat_map(|(_, commands)| commands.iter())
        .flat_map(|command| command.flags.iter())
        .any(|known| known.name == flag && !known.value.is_empty())
        || flag == "--dir"
}

fn verbs_of(group: &str) -> Vec<&'static Command> {
    let commands = help::GROUPS.iter().flat_map(|(_, commands)| commands.iter().copied());
    commands.filter(|command| command.name.split_once(' ').is_some_and(|(of, _)| of == group)).collect()
}

fn verb_word(command: &Command) -> &str {
    command.name.split_once(' ').map_or(command.name, |(_, verb)| verb)
}

fn flags_of(command: &Command) -> Vec<String> {
    let shown = command.flags.iter().filter(|flag| flag.heading != help::HIDDEN);
    shown.map(|flag| flag.name.to_owned()).chain(["--help".to_owned()]).collect()
}

const BASH: &str = r#"# tinystore's Tab completion for bash: eval "$(tinystore completion bash)"
_tinystore() {
    local typed="${COMP_LINE:0:COMP_POINT}" words line
    read -ra words <<< "$typed"
    [[ "$typed" == *" " ]] && words+=("")
    local current="${words[-1]}" piece="${COMP_WORDS[COMP_CWORD]}"
    local before="${current%"$piece"}"
    COMPREPLY=()
    while IFS= read -r line; do
        case "$line" in
            @dirs) while IFS= read -r line; do COMPREPLY+=("$line"); done < <(compgen -d -- "$piece") ;;
            @files) while IFS= read -r line; do COMPREPLY+=("$line"); done < <(compgen -f -- "$piece") ;;
            *) COMPREPLY+=("${line#"$before"}") ;;
        esac
    done < <(tinystore __complete "${words[@]:1:${#words[@]}-2}" "+$current" 2>/dev/null)
}
complete -o default -F _tinystore tinystore
"#;

const ZSH: &str = r#"# tinystore's Tab completion for zsh: eval "$(tinystore completion zsh)"
_tinystore() {
    local -a found
    local line
    for line in "${(@f)$(tinystore __complete "${(@)words[2,CURRENT-1]}" "+${words[CURRENT]}" 2>/dev/null)}"; do
        case $line in
            @dirs) _files -/ ;;
            @files) _files ;;
            '') ;;
            *) found+=("$line") ;;
        esac
    done
    (( ${#found} )) && compadd -U -Q -- "${found[@]}"
}
compdef _tinystore tinystore
"#;

const FISH: &str = r#"# tinystore's Tab completion for fish: tinystore completion fish | source
function __tinystore_complete
    set -l before (commandline -opc)
    set -e before[1]
    set -l current (commandline -ct)
    for line in (tinystore __complete $before "+$current" 2>/dev/null)
        switch $line
            case @dirs
                __fish_complete_directories $current
            case @files
                __fish_complete_path $current
            case '*'
                echo $line
        end
    end
end
complete -c tinystore -f -a '(__tinystore_complete)'
"#;

const POWERSHELL: &str = r#"# tinystore's Tab completion for PowerShell:
#   tinystore completion powershell | Out-String | Invoke-Expression
Register-ArgumentCompleter -Native -CommandName tinystore -ScriptBlock {
    param($current, $ast, $cursor)
    $elements = @($ast.CommandElements | Select-Object -Skip 1 |
        Where-Object { $_.Extent.EndOffset -le $cursor })
    if ($current -ne '') { $elements = @($elements | Select-Object -SkipLast 1) }
    $before = @($elements | ForEach-Object { $_.Extent.Text })
    $result = [System.Management.Automation.CompletionResult]
    & tinystore __complete @before "+$current" 2>$null | ForEach-Object {
        if ($_ -eq '@dirs') {
            Get-ChildItem -Directory -Path "$current*" -ErrorAction SilentlyContinue |
                ForEach-Object { $result::new($_.Name, $_.Name, 'ProviderContainer', $_.Name) }
        } elseif ($_ -eq '@files') {
            Get-ChildItem -Path "$current*" -ErrorAction SilentlyContinue |
                ForEach-Object { $result::new($_.Name, $_.Name, 'ProviderItem', $_.Name) }
        } else {
            $result::new($_, $_, 'ParameterValue', $_)
        }
    }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use tinystore::metrics::Series;
    use tinystore::{Options, Store};

    fn offered(words: &str, current: &str) -> Vec<String> {
        let before: Vec<String> = words.split_whitespace().map(str::to_owned).collect();
        shown(&before, current)
    }

    #[test]
    fn commands_verbs_flags_and_values_come_from_the_help() {
        assert_eq!(offered("", "me"), ["metrics"]);
        assert_eq!(offered("kv", ""), ["get", "list", "set", "delete", "@dirs"]);
        assert_eq!(offered("kv ./data", "s"), ["set"]);
        assert_eq!(offered("kv ./data set flags beta on", "--t"), ["--ttl"]);
        assert_eq!(offered("kv ./data set flags beta on --as", ""), ["text", "int", "json"]);
        assert_eq!(
            offered("metrics ./data ingest -f --kind", "lost="),
            ["lost=counter", "lost=gauge", "lost=histogram"]
        );
        assert_eq!(offered("files ./data put avatars a.png", ""), ["@files"]);
        assert_eq!(offered("completion", "p"), ["powershell"]);
        assert_eq!(
            positional(&["./data".into(), "--since".into(), "1h".into(), "read".into(), "-f".into()]),
            ["./data", "read"]
        );
    }

    // contract: CLI-07
    #[test]
    fn names_come_from_the_store_read_beside_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
        store.bucket::<String>("sessions").open().unwrap().set("42", &"x".to_owned()).unwrap();
        store.counters("hits").open().unwrap();
        store.queue::<serde_json::Value>("emails").open().unwrap();
        store.files("avatars").open().unwrap();
        let now = std::time::SystemTime::now();
        let rtt = |target: &str| Series::gauge("rtt").label("target", target).sample(now, 1.0);
        store.metrics().unwrap().ingest(&[rtt("poland"), rtt("sweden")]).unwrap();
        let data = dir.path().display().to_string();
        assert_eq!(offered(&format!("kv {data} get"), ""), ["sessions"], "a bucket of values, not counters");
        assert_eq!(offered(&format!("jobs {data} cancel"), "e"), ["emails"]);
        assert_eq!(offered(&format!("files {data} list"), ""), ["avatars"]);
        assert_eq!(offered(&format!("metrics {data} read"), ""), ["rtt"]);
        assert_eq!(offered(&format!("metrics {data} read rtt --where"), ""), ["target="]);
        assert_eq!(offered(&format!("metrics {data} read rtt --where"), "target=s"), ["target=sweden"]);
        assert_eq!(offered(&format!("kv {data} get"), "sesions"), ["sessions"], "a typo is told its word");
        assert_eq!(offered(&format!("kv {data} get"), "sesi"), ["sessions"], "and a typo of its start");
        assert_eq!(offered(&format!("kv {data} get"), "sx"), Vec::<String>::new(), "two letters are no typo");
        let empty = tempfile::tempdir().unwrap();
        assert!(offered(&format!("kv {} get", empty.path().display()), "").is_empty());
        assert!(!empty.path().join("kv.db").exists() && !empty.path().join("LOCK").exists(), "it makes nothing");
    }
}
