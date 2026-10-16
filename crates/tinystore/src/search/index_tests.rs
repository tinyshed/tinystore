use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::*;
use crate::ErrorKind;
use crate::search::document::{self, Analyzed};
use crate::search::fixture::{Fixture, Random, fixture, open};
use crate::search::read::{checked, forms};
use crate::search::score::{Part, average, idf, points, score};
use crate::search::settings::Settings;
use crate::search::{analyzer, maintain};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Article {
    title: String,
    body: String,
}

fn article(title: &str, body: &str) -> Article {
    Article { title: title.to_owned(), body: body.to_owned() }
}

fn ids<T>(found: &Found<T>) -> Vec<&str> {
    found.hits.iter().map(|hit| hit.id.as_str()).collect()
}

fn search(index: &Index<Article>, text: &str) -> Found<Article> {
    checked(|| index.search(text).run()).unwrap()
}

#[test]
fn a_search_finds_documents_by_any_of_its_words_the_best_first() {
    let fixture = fixture();
    let articles = fixture.articles::<Article>("articles");
    articles.set("a", &article("Sourdough bread", "flour, water and salt")).unwrap();
    articles.set("b", &article("Rye", "bread from rye flour")).unwrap();
    articles.set("c", &article("Butter", "milk and cream")).unwrap();
    let found = search(&articles, "bread salt");
    assert_eq!(ids(&found), ["a", "b"], "a holds both words and one in its heavier title");
    assert!(found.hits[0].score > found.hits[1].score);
    assert_eq!(found.hits[0].value, article("Sourdough bread", "flour, water and salt"));
    assert_eq!(found.corrected, None);
    assert!(search(&articles, "").hits.is_empty());
}

#[test]
fn a_word_is_found_as_written_and_by_its_stem_in_any_script() {
    let fixture = fixture();
    let notes = fixture.store.index::<String>("notes").open().unwrap();
    notes.set(1, &"Кошки спят на печи".to_owned()).unwrap();
    notes.set(2, &"東京タワーの夜景".to_owned()).unwrap();
    notes.set(3, &"Running in the CAFÉ".to_owned()).unwrap();
    let found = |text: &str| -> Vec<String> {
        checked(|| notes.search(text).run()).unwrap().hits.into_iter().map(|hit| hit.id).collect()
    };
    assert_eq!(found("кошка"), ["1"], "Russian by its stem");
    assert_eq!(found("кошкам"), ["1"]);
    assert_eq!(found("東京"), ["2"], "Japanese by its pairs of letters");
    assert_eq!(found("cafe running"), ["3"], "accents folded, case ignored");
    assert!(found("run").is_empty(), "English is not stemmed: search-plan measured a loss");
}

#[test]
fn a_set_is_seen_by_the_next_search_and_a_deleted_document_never_answers() {
    let fixture = fixture();
    let articles = fixture.articles::<Article>("articles");
    articles.set("a", &article("Sourdough", "starter")).unwrap();
    assert_eq!(ids(&search(&articles, "sourdough")), ["a"]);
    articles.set("a", &article("Baguette", "crust")).unwrap();
    assert!(search(&articles, "sourdough").hits.is_empty(), "a set replaces the document whole");
    assert_eq!(ids(&search(&articles, "baguette")), ["a"]);
    maintain(&fixture.store).unwrap();
    assert_eq!(ids(&search(&articles, "crust")), ["a"]);
    assert!(articles.delete("a").unwrap());
    assert!(!articles.delete("a").unwrap(), "deleting an id that is not there does nothing");
    assert!(search(&articles, "baguette crust").hits.is_empty());
    maintain(&fixture.store).unwrap();
    assert!(search(&articles, "baguette crust").hits.is_empty());
}

#[test]
fn get_answers_the_value_set_or_none() {
    let fixture = fixture();
    let articles = fixture.articles::<Article>("articles");
    articles.set("a-17", &article("Хлеб на закваске", "…")).unwrap();
    assert_eq!(articles.get("a-17").unwrap(), Some(article("Хлеб на закваске", "…")));
    assert_eq!(articles.get("a-18").unwrap(), None);
    let other = fixture.articles::<Article>("other");
    assert_eq!(other.get("a-17").unwrap(), None, "each index keeps its own documents");
}

#[test]
fn set_many_sets_every_document_in_one_commit_or_none() {
    let fixture = fixture();
    let articles = fixture.articles::<Article>("articles");
    let (first, second) = (article("Rye bread", "flour"), article("Sourdough", "starter"));
    articles.set_many([("a-1", &first), ("a-2", &second), ("a-1", &article("Baguette", "crust"))]).unwrap();
    assert_eq!(articles.get("a-2").unwrap(), Some(second));
    assert_eq!(articles.get("a-1").unwrap(), Some(article("Baguette", "crust")), "a later place wins");
    assert!(search(&articles, "rye").hits.is_empty());
    let notes = fixture.store.index::<serde_json::Value>("notes").field("title", 1.0).open().unwrap();
    let (good, bad) = (serde_json::json!({ "title": "kept" }), serde_json::json!({ "title": 7 }));
    let refused = notes.set_many([("n-1", &good), ("n-2", &bad)]).unwrap_err();
    let said = "index notes: document 1, id \"n-2\": field title holds a number, not text: invalid";
    assert_eq!(refused.to_string(), said);
    assert_eq!(notes.get("n-1").unwrap(), None, "none of the list is set");
    assert!(checked(|| notes.search("kept").run()).unwrap().hits.is_empty());
}

#[test]
fn a_set_many_past_its_bound_is_a_limit_and_sets_nothing() {
    let fixture = fixture();
    let notes = fixture.store.index::<String>("notes").open().unwrap();
    let short = "word".to_owned();
    let many: Vec<(String, &String)> = (0..1001).map(|n| (format!("n-{n}"), &short)).collect();
    assert_eq!(notes.set_many(many).unwrap_err().kind(), ErrorKind::Limit);
    let large = "x".repeat(1_000_000);
    let heavy: Vec<(String, &String)> = (0..17).map(|n| (format!("n-{n}"), &large)).collect();
    assert_eq!(notes.set_many(heavy).unwrap_err().kind(), ErrorKind::Limit);
    assert_eq!(notes.get("n-0").unwrap(), None);
}

#[test]
fn pages_of_a_search_join_into_its_whole_ranking() {
    let fixture = fixture();
    let articles = fixture.articles::<Article>("articles");
    for n in 0..40 {
        articles.set(n, &article(&"bread ".repeat(n % 7 + 1), &format!("loaf {n}"))).unwrap();
    }
    let whole: Vec<String> =
        articles.search("bread loaf").limit(40).run().unwrap().hits.into_iter().map(|hit| hit.id).collect();
    let mut paged = Vec::new();
    for page in 0..4 {
        let found = articles.search("bread loaf").limit(10).offset(page * 10).run().unwrap();
        paged.extend(found.hits.into_iter().map(|hit| hit.id));
    }
    assert_eq!(whole.len(), 40);
    assert_eq!(paged, whole);
}

#[test]
fn a_misspelt_word_is_read_as_the_nearest_and_corrected_says_the_text_read() {
    let fixture = fixture();
    let articles = fixture.articles::<Article>("articles");
    articles.set("a", &article("Replication", "hello world")).unwrap();
    articles.set("b", &article("Репликация", "журнал")).unwrap();
    let found = search(&articles, "Replicaiton, Hello!");
    assert_eq!(ids(&found), ["a"]);
    assert_eq!(found.corrected.as_deref(), Some("replication, hello!"));
    assert_eq!(search(&articles, "реплиакция").corrected.as_deref(), Some("репликация"));
    let short = search(&articles, "helo");
    assert!(short.hits.is_empty() && short.corrected.is_none(), "under five letters a word is read as written");
    let strict =
        fixture.store.index::<Article>("articles").field("title", 2.0).field("body", 1.0).typo_tolerance(false);
    let strict = strict.open().unwrap();
    assert!(search(&strict, "replicaiton").hits.is_empty());
}

#[test]
fn a_word_that_matches_is_never_changed() {
    let fixture = fixture();
    let articles = fixture.articles::<Article>("articles");
    articles.set("a", &article("hellp", "")).unwrap();
    articles.set("b", &article("hello", "")).unwrap();
    let found = search(&articles, "hellp");
    assert_eq!((ids(&found), found.corrected.as_deref()), (vec!["a"], None));
}

#[test]
fn a_word_new_since_the_dictionary_was_built_is_read_once_maintenance_builds_it_again() {
    let fixture = fixture();
    let articles = fixture.articles::<Article>("articles");
    articles.set("a", &article("replication", "")).unwrap();
    assert_eq!(search(&articles, "replicaiton").corrected.as_deref(), Some("replication"));
    articles.set("b", &article("sourdough", "")).unwrap();
    assert!(search(&articles, "sourduogh").hits.is_empty(), "the dictionary kept is behind the words");
    maintain(&fixture.store).unwrap();
    assert_eq!(ids(&search(&articles, "sourduogh")), ["b"]);
    articles.delete("a").unwrap();
    let gone = search(&articles, "replicaiton");
    assert!(gone.hits.is_empty() && gone.corrected.is_none(), "a word gone since is never read");
}

/// Every row that holds a posting, as one string.
fn postings_in(dir: &Path) -> String {
    let connection = rusqlite::Connection::open(dir.join("search.db")).unwrap();
    let mut rows = String::new();
    for sql in [
        "select key, first, payload from _tinystore_search_blocks order by key, first",
        "select * from _tinystore_search_lists order by key",
    ] {
        let mut select = connection.prepare(sql).unwrap();
        let count = select.column_count();
        let mut found = select.query([]).unwrap();
        while let Some(row) = found.next().unwrap() {
            for at in 0..count {
                rows.push_str(&format!("{:?} ", row.get_ref(at).unwrap()));
            }
            rows.push('\n');
        }
    }
    rows
}

#[test]
fn a_changed_weight_rebuilds_nothing_and_the_next_search_reads_it() {
    let fixture = fixture();
    let articles = fixture.articles::<Article>("articles");
    articles.set("title", &article("bread", "salt")).unwrap();
    articles.set("body", &article("salt", "bread")).unwrap();
    for n in 0..30 {
        articles.set(format!("filler-{n}"), &article("water", &format!("salt {n}"))).unwrap();
    }
    maintain(&fixture.store).unwrap();
    assert_eq!(ids(&search(&articles, "bread"))[0], "title");
    let before = postings_in(fixture.dir.path());
    let reweighed = fixture.store.index::<Article>("articles").field("title", 1.0).field("body", 9.0).open().unwrap();
    assert_eq!(postings_in(fixture.dir.path()), before, "no block or list written again");
    assert_eq!(ids(&search(&reweighed, "bread"))[0], "body");
    assert_eq!(ids(&search(&articles, "bread"))[0], "body", "every handle of the index reads it");
}

#[test]
fn a_field_removed_leaves_searches_at_once_and_one_added_waits_for_an_empty_index() {
    let fixture = fixture();
    let articles = fixture.articles::<Article>("articles");
    articles.set("a", &article("bread", "salt")).unwrap();
    let titles = fixture.store.index::<Article>("articles").field("title", 2.0).open().unwrap();
    assert!(search(&titles, "salt").hits.is_empty());
    assert_eq!(ids(&search(&titles, "bread")), ["a"]);
    let added = fixture.store.index::<Article>("articles").field("title", 2.0).field("body", 1.0).open();
    assert_eq!(added.unwrap_err().kind(), ErrorKind::Invalid);
    let empty = fixture.store.index::<Article>("empty").field("title", 1.0).open().unwrap();
    drop(empty);
    fixture.store.index::<Article>("empty").field("title", 1.0).field("body", 1.0).open().unwrap();
}

#[test]
fn an_index_answers_after_a_restart_as_it_did_before() {
    let fixture = fixture();
    let articles = fixture.articles::<Article>("articles");
    for n in 0..50 {
        articles.set(n, &article(&format!("loaf {n}"), "bread and salt")).unwrap();
    }
    maintain(&fixture.store).unwrap();
    articles.set("late", &article("loaf", "in the head")).unwrap();
    let before = search(&articles, "loaf bread");
    drop(articles);
    let Fixture { dir, store } = fixture;
    store.close().unwrap();
    let store = open(dir.path());
    let articles = store.index::<Article>("articles").field("title", 2.0).field("body", 1.0).open().unwrap();
    assert_eq!(checked(|| articles.search("loaf bread").run()).unwrap(), before);
}

#[test]
fn errors_name_the_index_the_id_and_what_failed() {
    let fixture = fixture();
    let articles = fixture.articles::<Article>("articles");
    let failure = |error: crate::Error| (error.kind(), error.to_string());
    let bad = fixture.store.index::<Article>("Articles").open().unwrap_err();
    assert_eq!(failure(bad).0, ErrorKind::Invalid);
    let heavy = fixture.store.index::<Article>("weights").field("title", 0.0).open().unwrap_err();
    assert_eq!(failure(heavy).0, ErrorKind::Invalid);
    let empty = articles.set("", &article("a", "b")).unwrap_err();
    assert_eq!(failure(empty), (ErrorKind::Invalid, "index articles: id \"\": an empty id: invalid".to_owned()));
    let large = articles.set("big", &article("big", &"x".repeat(1 << 20))).unwrap_err();
    assert_eq!(failure(large).0, ErrorKind::Limit);
    let years = fixture.store.index::<serde_json::Value>("years").field("year", 1.0).open().unwrap();
    let year = years.set("a-17", &serde_json::json!({ "year": 2024 })).unwrap_err();
    let said = "index years: id \"a-17\": field year holds a number, not text: invalid";
    assert_eq!(failure(year), (ErrorKind::Invalid, said.to_owned()));
    assert_eq!(failure(articles.search(&"word ".repeat(300)).run().unwrap_err()).0, ErrorKind::Limit);
    assert_eq!(failure(articles.search("bread").limit(0).run().unwrap_err()).0, ErrorKind::Invalid);
    assert_eq!(failure(articles.search("bread").offset(995).limit(10).run().unwrap_err()).0, ErrorKind::Limit);
}

const WORDS: &[&str] = &[
    "bread",
    "sourdough",
    "flour",
    "water",
    "salt",
    "oven",
    "crust",
    "yeast",
    "starter",
    "dough",
    "rye",
    "wheat",
    "honey",
    "seed",
    "loaf",
    "bake",
    "baker",
    "knead",
    "crumb",
    "steam",
    "хлеб",
    "закваска",
    "мука",
    "вода",
    "соль",
    "печь",
    "корка",
    "тесто",
    "кошки",
    "кошка",
    "собаки",
    "собака",
    "東京",
    "世界和平",
];

/// Up to `most` words: a common one of [`WORDS`] mostly, and one in four of
/// hundreds of rare ones, whose short lists bound their blocks high.
fn words(random: &mut Random, most: u64) -> String {
    let count = random.below(most + 1);
    let word = |random: &mut Random| match random.below(4) {
        0 => format!("rare{}", random.skewed(400)),
        _ => WORDS[random.skewed(WORDS.len() as u64) as usize].to_owned(),
    };
    let picked: Vec<String> = (0..count).map(|_| word(random)).collect();
    picked.join(" ")
}

fn random_article(random: &mut Random) -> Article {
    Article { title: words(random, 5), body: words(random, 60) }
}

/// The documents an index should hold, each with the number its set gave
/// it and its words as the index's fields read them.
#[derive(Default)]
struct Model {
    docs: BTreeMap<String, Doc>,
    next: u32,
    /// What begins every id the model's writer sets.
    prefix: String,
}

/// A document of the model: its number, its value and its words.
struct Doc {
    number: u32,
    value: Article,
    terms: HashMap<(Part, String), u32>,
    analyzed: Analyzed,
}

impl Model {
    fn set(&mut self, settings: &Settings, id: String, value: Article) {
        self.next += 1;
        let analyzed = document::analyze(&serde_json::to_value(&value).unwrap(), &settings.fields).unwrap();
        let terms = analyzed.terms.iter().map(|(part, term, tf)| ((*part, term.clone()), *tf)).collect();
        self.docs.insert(id, Doc { number: self.next, value, terms, analyzed });
    }

    /// The best `k` by scoring every document the model holds, as the
    /// engine's hits say them.
    fn best(&self, settings: &Settings, text: &str, k: usize) -> Vec<(String, f64)> {
        let documents = self.docs.len() as u64;
        let mut holding: HashMap<&(Part, String), u64> = HashMap::new();
        let mut tokens: HashMap<Part, u64> = HashMap::new();
        for doc in self.docs.values() {
            doc.terms.keys().for_each(|key| *holding.entry(key).or_default() += 1);
            doc.analyzed.lengths.iter().for_each(|(part, len)| *tokens.entry(*part).or_default() += u64::from(*len));
        }
        let query = forms(&analyzer::words(text).into_iter().take(32).collect::<Vec<_>>(), settings);
        let mut scored: Vec<(u64, u32, &String)> = Vec::new();
        for (id, doc) in &self.docs {
            let mut total: Option<u64> = None;
            for key in &query {
                let Some(tf) = doc.terms.get(key) else { continue };
                let part = key.0;
                let weight = settings.weight(part.field()).unwrap();
                let scale = part.weight(weight) * idf(documents, holding[key]);
                let average = average(tokens.get(&part).copied().unwrap_or(0), documents);
                let added = points(scale, *tf, doc.analyzed.length(part), average);
                total = Some(total.unwrap_or(0).saturating_add(added));
            }
            if let Some(total) = total {
                scored.push((total, doc.number, id));
            }
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().take(k).map(|(total, _, id)| (id.clone(), score(total))).collect()
    }
}

/// Checks a search against the model, by every document it holds.
fn agrees(index: &Index<Article>, model: &Model, random: &mut Random, seed: u64) {
    let settings = Settings::clone(&index.open.settings_read());
    let text = words(random, 4);
    let (offset, limit) = (random.below(15) as usize, 1 + random.below(25) as usize);
    let found = checked(|| index.search(&text).offset(offset).limit(limit).run()).unwrap();
    let mut expected = model.best(&settings, &text, offset + limit);
    expected.drain(..offset.min(expected.len()));
    let got: Vec<(String, f64)> = found.hits.iter().map(|hit| (hit.id.clone(), hit.score)).collect();
    assert_eq!(got, expected, "seed {seed}: {text:?}");
    for hit in &found.hits {
        assert_eq!(hit.value, model.docs[&hit.id].value, "seed {seed}: a hit's value is the one set last");
    }
}

#[test]
fn a_search_answers_what_scoring_every_document_answers_under_sets_deletes_packing_and_purging() {
    for seed in 0..4 {
        let fixture = fixture();
        let index: Index<Article> =
            fixture.store.index("mix").field("title", 2.0).field("body", 1.0).typo_tolerance(false).open().unwrap();
        let mut random = Random::new(seed);
        let mut model = Model::default();
        let settings = Settings::clone(&index.open.settings_read());
        for _ in 0..2500 {
            match random.below(100) {
                0..70 => {
                    let (id, value) = (format!("d{}", random.below(500)), random_article(&mut random));
                    index.set(&id, &value).unwrap();
                    model.set(&settings, id, value);
                }
                70..82 => {
                    let id = format!("d{}", random.below(500));
                    assert_eq!(index.delete(&id).unwrap(), model.docs.remove(&id).is_some());
                }
                82..84 => drop(maintain(&fixture.store).unwrap()),
                84..86 => {
                    let ids: Vec<String> =
                        (0..1 + random.below(12)).map(|_| format!("d{}", random.below(500))).collect();
                    let values: Vec<Article> = ids.iter().map(|_| random_article(&mut random)).collect();
                    index.set_many(ids.iter().zip(&values)).unwrap();
                    ids.into_iter().zip(values).for_each(|(id, value)| model.set(&settings, id, value));
                }
                86..88 => drop(index.search.pack_step(&index.open, 1 + random.below(3000) as usize).unwrap()),
                88..89 => index.search.purge_in_steps(&index.open, 1 + random.below(40) as usize, || {}).unwrap(),
                _ => agrees(&index, &model, &mut random, seed),
            }
        }
        for _ in 0..20 {
            agrees(&index, &model, &mut random, seed);
        }
    }
}

#[test]
fn packing_and_purging_change_no_answer() {
    let fixture = fixture();
    let articles = fixture.articles::<Article>("articles");
    let mut random = Random::new(7);
    for n in 0..600 {
        articles.set(n % 300, &random_article(&mut random)).unwrap();
    }
    let texts: Vec<String> = (0..30).map(|_| words(&mut random, 4)).collect();
    let answers = |articles: &Index<Article>| -> Vec<Found<Article>> {
        texts.iter().map(|text| checked(|| articles.search(text).limit(20).run()).unwrap()).collect()
    };
    let before = answers(&articles);
    loop {
        let step = articles.search.pack_step(&articles.open, 700).unwrap();
        assert_eq!(answers(&articles), before, "a step of packing");
        if step.done {
            break;
        }
    }
    articles
        .search
        .purge_in_steps(&articles.open, 25, || assert_eq!(answers(&articles), before, "a step of purging"))
        .unwrap();
}

#[test]
fn searches_and_writes_from_many_threads_answer_as_one_at_a_time_would() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let fixture = fixture();
    let index: Index<Article> =
        fixture.store.index("threads").field("title", 2.0).field("body", 1.0).typo_tolerance(false).open().unwrap();
    let (done, searches) = (AtomicBool::new(false), AtomicUsize::new(0));
    let models: Vec<Model> = std::thread::scope(|scope| {
        for reader in 0..3 {
            let (index, done, searches) = (&index, &done, &searches);
            scope.spawn(move || {
                let mut random = Random::new(100 + reader);
                while !done.load(Ordering::Acquire) {
                    checked(|| index.search(&words(&mut random, 3)).limit(15).run()).unwrap();
                    searches.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
        let maintaining = scope.spawn(|| {
            while !done.load(Ordering::Acquire) {
                maintain(&fixture.store).unwrap();
            }
        });
        let writers: Vec<_> = (0..4)
            .map(|writer| {
                let index = &index;
                scope.spawn(move || {
                    let mut random = Random::new(writer);
                    let mut model = Model { prefix: format!("w{writer}-"), ..Model::default() };
                    for _ in 0..400 {
                        let id = format!("{}{}", model.prefix, random.below(60));
                        if random.below(5) == 0 {
                            index.delete(&id).unwrap();
                            model.docs.remove(&id);
                        } else {
                            let value = random_article(&mut random);
                            index.set(&id, &value).unwrap();
                            model.set(&index.open.settings_read(), id, value);
                        }
                    }
                    model
                })
            })
            .collect();
        let models = writers.into_iter().map(|writer| writer.join().unwrap()).collect();
        done.store(true, Ordering::Release);
        maintaining.join().unwrap();
        models
    });
    assert!(searches.load(Ordering::Relaxed) > 0);
    for model in models {
        for n in 0..60 {
            let id = format!("{}{n}", model.prefix);
            assert_eq!(index.get(&id).unwrap(), model.docs.get(&id).map(|doc| doc.value.clone()), "{id}");
        }
        for (id, Doc { value, .. }) in &model.docs {
            let Some(word) = value.title.split(' ').chain(value.body.split(' ')).find(|word| !word.is_empty()) else {
                continue;
            };
            let found = checked(|| index.search(word).limit(1000).run()).unwrap();
            let hit = found.hits.iter().find(|hit| &hit.id == id);
            assert_eq!(hit.map(|hit| &hit.value), Some(value), "{id} is found by its words as last set");
        }
    }
}

/// The files of search.db as a crash would leave them now.
fn crash_image(dir: &Path) -> tempfile::TempDir {
    let image = tempfile::tempdir().unwrap();
    for name in ["search.db", "search.db-wal"] {
        if dir.join(name).exists() {
            std::fs::copy(dir.join(name), image.path().join(name)).unwrap();
        }
    }
    image
}

#[test]
fn a_crash_between_steps_of_packing_or_purging_leaves_the_last_commit() {
    let fixture = fixture();
    let articles = fixture.articles::<Article>("articles");
    let mut random = Random::new(11);
    for n in 0..300 {
        articles.set(n % 200, &random_article(&mut random)).unwrap();
    }
    let texts: Vec<String> = (0..10).map(|_| words(&mut random, 3)).collect();
    let answers = |articles: &Index<Article>| -> Vec<Found<Article>> {
        texts.iter().map(|text| checked(|| articles.search(text).run()).unwrap()).collect()
    };
    let before = answers(&articles);
    let mut images = vec![crash_image(fixture.dir.path())];
    while !articles.search.pack_step(&articles.open, 500).unwrap().done {
        images.push(crash_image(fixture.dir.path()));
    }
    articles.search.purge_in_steps(&articles.open, 30, || images.push(crash_image(fixture.dir.path()))).unwrap();
    assert!(images.len() > 4, "packing and purging took several steps");
    for image in images {
        let store = open(image.path());
        let reopened = store.index::<Article>("articles").field("title", 2.0).field("body", 1.0).open().unwrap();
        assert_eq!(answers(&reopened), before);
        maintain(&store).unwrap();
        assert_eq!(answers(&reopened), before, "and once its packing and purging go on");
    }
}

fn sizes(dir: &Path) -> (u64, u64) {
    let size = |name: &str| std::fs::metadata(dir.join(name)).map_or(0, |metadata| metadata.len());
    (size("search.db"), size("search.db-wal"))
}

#[test]
fn through_a_long_mix_the_file_stays_near_a_fresh_build_of_the_same_documents() {
    let fixture = fixture();
    let articles = fixture.articles::<Article>("articles");
    let mut random = Random::new(13);
    let mut model = Model::default();
    let settings = Settings::clone(&articles.open.settings_read());
    for round in 0..8000 {
        let id = format!("d{}", random.below(400));
        if random.below(6) == 0 {
            articles.delete(&id).unwrap();
            model.docs.remove(&id);
        } else {
            let value = random_article(&mut random);
            articles.set(&id, &value).unwrap();
            model.set(&settings, id, value);
        }
        if round % 1000 == 999 {
            maintain(&fixture.store).unwrap();
        }
    }
    maintain(&fixture.store).unwrap();
    let fresh = crate::search::fixture::fixture();
    let rebuilt = fresh.articles::<Article>("articles");
    for (id, doc) in &model.docs {
        rebuilt.set(id, &doc.value).unwrap();
    }
    maintain(&fresh.store).unwrap();
    let ((mixed, mixed_log), (built, _)) = (sizes(fixture.dir.path()), sizes(fresh.dir.path()));
    eprintln!("{mixed} bytes after the mix, {built} built afresh, a log of {mixed_log}");
    assert!(mixed <= built * 2, "{mixed} bytes after the mix, {built} built afresh");
    assert!(mixed_log <= 8 << 20, "the write-ahead log holds {mixed_log} bytes");
}
