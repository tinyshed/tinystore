use std::collections::{BTreeMap, BTreeSet, HashMap};
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

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Article {
    title: String,
    body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    price: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
}

fn article(title: &str, body: &str) -> Article {
    Article { title: title.to_owned(), body: body.to_owned(), ..Article::default() }
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

// contract: SRCH-04
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

// contract: SRCH-05
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

// contract: SRCH-05
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

// contract: SRCH-09
#[test]
fn two_corrected_pairs_of_one_unspaced_run_read_as_one_text() {
    let fixture = fixture();
    let index = |name: &str| fixture.store.index::<String>(name).open().unwrap();
    let found = |notes: &Index<String>, text: &str| checked(|| notes.search(text).run()).unwrap();
    // A pair of Thai graphemes, marks and all, is long enough to be corrected.
    let notes = index("thai");
    notes.set("a", &"คิ้กิ".to_owned()).unwrap();
    notes.set("b", &"กิขิ้".to_owned()).unwrap();
    let thai = found(&notes, "คิ้กิ้ขิ้");
    assert_eq!(thai.corrected.as_deref(), Some("คิ้กิขิ้"), "both pairs read without the middle tone mark");
    assert_eq!(ids(&thai), ["a", "b"]);
    // Read as a character of three bytes, one of four leaves the text after it elsewhere.
    let (first, second, wide, narrow) =
        ("\u{e01}\u{e34}\u{e49}\u{e47}", "\u{e02}\u{e34}\u{e49}\u{e47}", "\u{20000}", "中");
    let notes = index("han");
    notes.set("c", &format!("{first}{narrow}")).unwrap();
    notes.set("d", &format!("{narrow}{second}")).unwrap();
    let han = found(&notes, &format!("{first}{wide}{second}"));
    assert_eq!(han.corrected, Some(format!("{first}{narrow}{second}")));
    assert_eq!(ids(&han), ["c", "d"]);
}

// contract: SRCH-09
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

// contract: SRCH-09
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

// contract: SRCH-06
#[test]
fn a_field_removed_leaves_searches_at_once_and_one_added_is_read_beside_the_index_until_it_switches_in() {
    let fixture = fixture();
    let articles = fixture.articles::<Article>("articles");
    articles.set("a", &article("bread", "salt")).unwrap();
    let titles = fixture.store.index::<Article>("articles").field("title", 2.0).open().unwrap();
    assert!(search(&titles, "salt").hits.is_empty());
    assert_eq!(ids(&search(&titles, "bread")), ["a"]);
    let added = fixture.store.index::<Article>("articles").field("title", 2.0).field("body", 1.0).open().unwrap();
    assert_eq!(added.status().unwrap(), crate::search::Status::Rebuilding { done: 0.0 });
    assert!(search(&added, "salt").hits.is_empty(), "the index answers as before until then");
    maintain(&fixture.store).unwrap();
    added.ready().unwrap();
    assert_eq!(added.status().unwrap(), crate::search::Status::Ready);
    assert_eq!(ids(&search(&added, "salt")), ["a"]);
    assert_eq!(ids(&search(&titles, "salt")), ["a"], "every handle of the index reads it");
    let empty = fixture.store.index::<Article>("empty").field("title", 1.0).open().unwrap();
    drop(empty);
    let empty = fixture.store.index::<Article>("empty").field("title", 1.0).field("body", 1.0).open().unwrap();
    assert_eq!(empty.status().unwrap(), crate::search::Status::Ready, "an empty index takes a field at once");
}

// contract: SRCH-10
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

// contract: ERR-02
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
    Article { title: words(random, 5), body: words(random, 60), ..Article::default() }
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

/// A document of the model: its number, its value, its words and its vector.
struct Doc {
    number: u32,
    value: Article,
    terms: HashMap<(Part, String), u32>,
    analyzed: Analyzed,
    vector: Option<Vec<f32>>,
}

impl Model {
    /// The same documents as `settings` read them.
    fn read_by(&self, settings: &Settings) -> Model {
        let mut model = Model { next: self.next, prefix: self.prefix.clone(), ..Model::default() };
        for (id, doc) in &self.docs {
            let analyzed =
                document::analyze(&serde_json::to_value(&doc.value).unwrap(), &settings.fields, &settings.filters)
                    .unwrap();
            let terms = analyzed.terms.iter().map(|(part, term, tf)| ((*part, term.clone()), *tf)).collect();
            let value = doc.value.clone();
            model
                .docs
                .insert(id.clone(), Doc { number: doc.number, value, terms, analyzed, vector: doc.vector.clone() });
        }
        model
    }

    fn set(&mut self, settings: &Settings, id: String, value: Article) {
        self.set_with(settings, id, value, None);
    }

    fn set_with(&mut self, settings: &Settings, id: String, value: Article, vector: Option<Vec<f32>>) {
        self.next += 1;
        let analyzed =
            document::analyze(&serde_json::to_value(&value).unwrap(), &settings.fields, &settings.filters).unwrap();
        let terms = analyzed.terms.iter().map(|(part, term, tf)| ((*part, term.clone()), *tf)).collect();
        self.docs.insert(id, Doc { number: self.next, value, terms, analyzed, vector });
    }

    /// The best `k` by scoring every document the model holds that the
    /// search keeps, as the engine's hits say them; every document counts in
    /// the scores.
    fn best(&self, settings: &Settings, ask: &Ask<'_>, k: usize) -> Vec<(String, f64)> {
        let documents = self.docs.len() as u64;
        let mut holding: HashMap<&(Part, String), u64> = HashMap::new();
        let mut tokens: HashMap<Part, u64> = HashMap::new();
        for doc in self.docs.values() {
            doc.terms.keys().for_each(|key| *holding.entry(key).or_default() += 1);
            doc.analyzed.lengths.iter().for_each(|(part, len)| *tokens.entry(*part).or_default() += u64::from(*len));
        }
        let query = forms(&analyzer::words(ask.text).into_iter().take(32).collect::<Vec<_>>(), settings);
        let mut scored: Vec<(u64, u32, &String)> = Vec::new();
        for (id, doc) in self.docs.iter().filter(|(_, doc)| (ask.keep)(&doc.value)) {
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

    /// The cosine of a document's vector with `question`, none when it has none.
    fn cosine(&self, id: &str, question: &[f32]) -> Option<f64> {
        let length = |vector: &[f32]| vector.iter().map(|x| f64::from(*x) * f64::from(*x)).sum::<f64>().sqrt();
        let vector = self.docs[id].vector.as_ref()?;
        let dot: f64 = question.iter().zip(vector).map(|(q, v)| f64::from(*q) * f64::from(*v)).sum();
        Some(dot / (length(question) * length(vector)))
    }

    /// The best `k` by scoring every vector the model holds that the
    /// search keeps.
    fn nearest(&self, ask: &Ask<'_>, k: usize) -> Vec<(String, f64)> {
        let kept = self.docs.iter().filter(|(_, doc)| (ask.keep)(&doc.value));
        let scored = kept.filter_map(|(id, _)| Some((id.clone(), self.cosine(id, ask.question)?)));
        self.ordered(scored.collect(), k)
    }

    /// Hits by their scores, the document set first of equal ones.
    fn ordered(&self, mut hits: Vec<(String, f64)>, k: usize) -> Vec<(String, f64)> {
        let number = |id: &str| self.docs[id].number;
        hits.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap().then(number(&a.0).cmp(&number(&b.0))));
        hits.truncate(k);
        hits
    }

    /// The best `k` of a search by `text` and `question` at once, from the
    /// best hundred of each half, or `k` if more.
    fn both(&self, settings: &Settings, ask: &Ask<'_>, how: How, k: usize) -> Vec<(String, f64)> {
        let (text, question) = (ask.text, ask.question);
        let alone = analyzer::words(text).is_empty() && !matches!(how, How::Rerank);
        if alone || matches!(how, How::Sum(1.0)) {
            return self.nearest(ask, k);
        }
        if matches!(how, How::Sum(0.0)) {
            return self.best(settings, ask, k);
        }
        let words = self.best(settings, ask, k.max(100));
        let How::Sum(weight) = how else {
            let (with, without): (Vec<_>, Vec<_>) =
                words.into_iter().partition(|(id, _)| self.docs[id].vector.is_some());
            let cosines = with.into_iter().map(|(id, _)| (id.clone(), self.cosine(&id, question).unwrap())).collect();
            let mut reranked = self.ordered(cosines, usize::MAX);
            reranked.extend(without.into_iter().map(|(id, _)| (id, -1.0)));
            reranked.truncate(k);
            return reranked;
        };
        let (words, vectors) = (scaled(&words), scaled(&self.nearest(ask, k.max(100))));
        let ids: BTreeSet<&String> = words.keys().chain(vectors.keys()).collect();
        let half = |half: &HashMap<String, f64>, id: &String| half.get(id).copied().unwrap_or(0.0);
        let sums =
            ids.into_iter().map(|id| (id.clone(), (1.0 - weight) * half(&words, id) + weight * half(&vectors, id)));
        self.ordered(sums.collect(), k)
    }
}

/// What a check asks the model: a search's text and vector, and which
/// documents its filter keeps.
struct Ask<'a> {
    text: &'a str,
    question: &'a [f32],
    keep: &'a dyn Fn(&Article) -> bool,
}

/// How a search by both combines its halves.
#[derive(Clone, Copy, Debug)]
enum How {
    Sum(f64),
    Rerank,
}

/// A half's scores from 0, its lowest, to 1, its highest; 1 each when all are equal.
fn scaled(half: &[(String, f64)]) -> HashMap<String, f64> {
    let lowest = half.iter().map(|(_, score)| *score).fold(f64::INFINITY, f64::min);
    let highest = half.iter().map(|(_, score)| *score).fold(f64::NEG_INFINITY, f64::max);
    let scale = |score: f64| if highest > lowest { (score - lowest) / (highest - lowest) } else { 1.0 };
    half.iter().map(|(id, score)| (id.clone(), scale(*score))).collect()
}

/// Checks a search against the model, by every document it holds.
fn agrees(index: &Index<Article>, model: &Model, random: &mut Random, seed: u64) {
    let settings = Settings::clone(&index.open.settings());
    let text = words(random, 4);
    let (offset, limit) = (random.below(15) as usize, 1 + random.below(25) as usize);
    let found = checked(|| index.search(&text).offset(offset).limit(limit).run()).unwrap();
    let mut expected = model.best(&settings, &Ask { text: &text, question: &[], keep: &|_| true }, offset + limit);
    expected.drain(..offset.min(expected.len()));
    let got: Vec<(String, f64)> = found.hits.iter().map(|hit| (hit.id.clone(), hit.score)).collect();
    assert_eq!(got, expected, "seed {seed}: {text:?}");
    for hit in &found.hits {
        assert_eq!(hit.value, model.docs[&hit.id].value, "seed {seed}: a hit's value is the one set last");
    }
}

// contract: SRCH-01
#[test]
fn a_search_answers_what_scoring_every_document_answers_under_sets_deletes_packing_and_purging() {
    for seed in 0..4 {
        let fixture = fixture();
        let index: Index<Article> =
            fixture.store.index("mix").field("title", 2.0).field("body", 1.0).typo_tolerance(false).open().unwrap();
        let mut random = Random::new(seed);
        let mut model = Model::default();
        let settings = Settings::clone(&index.open.settings());
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

// contract: DUR-09
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
                            model.set(&index.open.settings(), id, value);
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

// contract: DUR-09
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
    let settings = Settings::clone(&articles.open.settings());
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

/// The dimensions of the vectors the tests give.
const DIMENSIONS: usize = 6;

/// A vector of whole numbers and of fractions, which gives equal cosines as
/// often as nearly equal ones.
fn embedding(random: &mut Random) -> Vec<f32> {
    let mut values: Vec<f32> = (0..DIMENSIONS)
        .map(|_| match random.below(3) {
            0 => random.below(5) as f32 - 2.0,
            _ => (random.below(2001) as f32 - 1000.0) / 997.0,
        })
        .collect();
    if values.iter().all(|value| *value == 0.0) {
        values[0] = 1.0;
    }
    values
}

fn products(fixture: &Fixture) -> Index<Article> {
    let builder = fixture.store.index("products").field("title", 2.0).field("body", 1.0);
    builder.typo_tolerance(false).vectors(DIMENSIONS).open().unwrap()
}

#[test]
fn a_search_by_a_vector_answers_the_nearest_by_their_cosine() {
    let fixture = fixture();
    let shoes = fixture.store.index::<Article>("shoes").field("title", 1.0).vectors(3).open().unwrap();
    shoes.id("a").vector(&[1.0, 0.0, 0.0]).set(&article("red shoes", "")).unwrap();
    shoes.id("b").vector(&[0.0, 1.0, 0.0]).set(&article("blue shoes", "")).unwrap();
    shoes.id("c").vector(&[2.0, 0.5, 0.0]).set(&article("red hat", "")).unwrap();
    shoes.id("d").without_vector().set(&article("red", "")).unwrap();
    let nearest = checked(|| shoes.search_vector(&[1.0, 0.0, 0.0]).run()).unwrap();
    assert_eq!(ids(&nearest), ["a", "c", "b"], "a document without a vector is found by its words alone");
    assert_eq!(nearest.hits.iter().map(|hit| hit.score).collect::<Vec<_>>(), [1.0, 2.0 / 4.25f64.sqrt(), 0.0]);
    let reranked = checked(|| shoes.search("red").vector(&[0.0, 1.0, 0.0]).rerank().run()).unwrap();
    assert_eq!(ids(&reranked), ["c", "a", "d"], "the words' hits by their vectors, none without last");
    assert_eq!(reranked.hits[2].score, -1.0);
    let words = checked(|| shoes.search("red").vector(&[0.0, 1.0, 0.0]).vector_weight(0.0).run()).unwrap();
    assert_eq!(words, search(&shoes, "red"), "a weight of 0 is the search by the words alone");
    let vector = checked(|| shoes.search("red").vector(&[1.0, 0.0, 0.0]).vector_weight(1.0).run()).unwrap();
    assert_eq!(vector, nearest, "and of 1, by the vector alone");
    let (first, second, third) = (article("a", ""), article("b", ""), article("c", ""));
    let (va, vb) = (vec![1.0, 2.0, 3.0], vec![3.0, 2.0, 1.0]);
    shoes.set_many([("e", &first, &va), ("f", &second, &vb)]).unwrap();
    shoes.set_many([("g", &third, Some(&va[..])), ("h", &third, None)]).unwrap();
    let listed = checked(|| shoes.search_vector(&[3.0, 2.0, 1.0]).limit(20).run()).unwrap();
    assert_eq!(ids(&listed), ["f", "c", "a", "e", "g", "b"], "of equal cosines, e set first; h has no vector");
}

#[test]
fn an_index_with_vectors_asks_each_set_for_one_and_refuses_a_vector_it_cannot_compare() {
    let fixture = fixture();
    let products = products(&fixture);
    let kind = |result: crate::Result<()>| result.unwrap_err().kind();
    let said = products.set("p", &article("shoes", "")).unwrap_err().to_string();
    assert!(said.starts_with("index products: id \"p\": the index keeps vectors"), "{said}");
    assert_eq!(kind(products.id("p").vector(&[1.0; 5]).set(&article("shoes", ""))), ErrorKind::Invalid);
    assert_eq!(kind(products.id("p").vector(&[f32::NAN; DIMENSIONS]).set(&article("", ""))), ErrorKind::Invalid);
    assert_eq!(kind(products.id("p").vector(&[0.0; DIMENSIONS]).set(&article("", ""))), ErrorKind::Invalid);
    let many = products.set_many([("p", &article("shoes", ""))]).unwrap_err().to_string();
    assert!(many.starts_with("index products: document 0, id \"p\": the index keeps vectors"), "{many}");
    assert_eq!(products.get("p").unwrap(), None);
    let words = fixture.articles::<Article>("articles");
    assert_eq!(kind(words.id("a").vector(&[1.0]).set(&article("", ""))), ErrorKind::Invalid);
    words.id("a").without_vector().set(&article("bread", "")).unwrap();
    let search = |call: SearchCall<'_, Article>| call.run().map(drop).unwrap_err().kind();
    let q = [1.0; DIMENSIONS];
    assert_eq!(search(words.search("bread").vector(&q)), ErrorKind::Invalid);
    assert_eq!(search(products.search("bread").vector(&[1.0; 5])), ErrorKind::Invalid);
    assert_eq!(search(products.search("bread").vector(&[0.0; DIMENSIONS])), ErrorKind::Invalid);
    assert_eq!(search(products.search("bread").vector(&q).vector_weight(1.5)), ErrorKind::Invalid);
    assert_eq!(search(products.search("bread").vector(&q).vector_weight(0.5).rerank()), ErrorKind::Invalid);
    assert_eq!(search(products.search("bread").vector_weight(0.5)), ErrorKind::Invalid);
    assert_eq!(search(products.search("bread").rerank()), ErrorKind::Invalid);
    assert_eq!(search(products.search("").vector(&q).rerank()), ErrorKind::Invalid);
}

#[test]
fn vectors_added_leave_the_documents_held_without_and_other_dimensions_are_refused() {
    let fixture = fixture();
    let articles = fixture.articles::<Article>("articles");
    articles.set("old", &article("bread", "")).unwrap();
    let opened = |dimensions: Option<usize>| {
        let builder = fixture.store.index::<Article>("articles").field("title", 2.0).field("body", 1.0);
        match dimensions {
            Some(dimensions) => builder.vectors(dimensions).open(),
            None => builder.open(),
        }
    };
    let with = opened(Some(2)).unwrap();
    with.id("new").vector(&[1.0, 0.0]).set(&article("bread", "")).unwrap();
    assert_eq!(ids(&checked(|| with.search("").vector(&[1.0, 1.0]).run()).unwrap()), ["new"]);
    assert_eq!(ids(&search(&with, "bread")), ["old", "new"]);
    assert_eq!(opened(Some(3)).unwrap_err().kind(), ErrorKind::Invalid);
    assert_eq!(opened(None).unwrap_err().kind(), ErrorKind::Invalid, "nor would an open without them drop them");
}

#[test]
fn a_changed_vector_weight_rebuilds_nothing_and_the_next_search_reads_it() {
    let fixture = fixture();
    let products = products(&fixture);
    let mut random = Random::new(3);
    for n in 0..40 {
        products.id(n).vector(&embedding(&mut random)).set(&random_article(&mut random)).unwrap();
    }
    maintain(&fixture.store).unwrap();
    let (text, q) = (words(&mut random, 3) + " bread", embedding(&mut random));
    let weighed = checked(|| products.search(&text).vector(&q).vector_weight(0.8).run()).unwrap();
    let before = postings_in(fixture.dir.path());
    let builder = fixture.store.index::<Article>("products").field("title", 2.0).field("body", 1.0);
    let reopened = builder.typo_tolerance(false).vectors(DIMENSIONS).vector_weight(0.8).open().unwrap();
    assert_eq!(postings_in(fixture.dir.path()), before, "no block or list written again");
    assert_eq!(checked(|| reopened.search(&text).vector(&q).run()).unwrap(), weighed);
    assert_eq!(checked(|| products.search(&text).vector(&q).run()).unwrap(), weighed, "every handle reads it");
}

/// Checks a search by a vector, or by words and a vector, against the model.
fn agrees_by_vector(index: &Index<Article>, model: &Model, random: &mut Random, seed: u64) {
    let settings = Settings::clone(&index.open.settings());
    let (offset, limit) = (random.below(15) as usize, 1 + random.below(25) as usize);
    let (text, q) = (if random.below(4) == 0 { String::new() } else { words(random, 4) }, embedding(random));
    let call = index.search(&text).vector(&q).offset(offset).limit(limit);
    let (call, how) = match random.below(4) {
        0 if !text.trim().is_empty() => (call.rerank(), How::Rerank),
        1 => {
            let weight = [0.0, 0.3, 1.0][random.below(3) as usize];
            (call.vector_weight(weight), How::Sum(weight))
        }
        _ => (call, How::Sum(0.5)),
    };
    let found = checked(|| call.run()).unwrap();
    let ask = Ask { text: &text, question: &q, keep: &|_| true };
    let mut expected = model.both(&settings, &ask, how, offset + limit);
    expected.drain(..offset.min(expected.len()));
    let got: Vec<(String, f64)> = found.hits.iter().map(|hit| (hit.id.clone(), hit.score)).collect();
    assert_eq!(got, expected, "seed {seed}: {text:?} {how:?}");
}

/// Sets a document of the model's with its vector, or without one.
fn set_with(index: &Index<Article>, id: &str, value: &Article, vector: Option<&[f32]>) {
    let call = index.id(id);
    let call = match vector {
        Some(vector) => call.vector(vector),
        None => call.without_vector(),
    };
    call.set(value).unwrap();
}

// contract: SRCH-02
#[test]
fn searches_by_a_vector_and_by_both_answer_what_scoring_every_document_answers() {
    for seed in 0..4 {
        let fixture = fixture();
        let index = products(&fixture);
        let mut random = Random::new(seed);
        let mut model = Model::default();
        let settings = Settings::clone(&index.open.settings());
        for _ in 0..1500 {
            match random.below(100) {
                0..60 => {
                    let (id, value) = (format!("d{}", random.below(300)), random_article(&mut random));
                    let vector = (random.below(10) != 0).then(|| embedding(&mut random));
                    set_with(&index, &id, &value, vector.as_deref());
                    model.set_with(&settings, id, value, vector);
                }
                60..72 => {
                    let id = format!("d{}", random.below(300));
                    assert_eq!(index.delete(&id).unwrap(), model.docs.remove(&id).is_some());
                }
                72..74 => drop(maintain(&fixture.store).unwrap()),
                74..76 => {
                    let ids: Vec<String> =
                        (0..1 + random.below(8)).map(|_| format!("d{}", random.below(300))).collect();
                    let values: Vec<(Article, Vec<f32>)> =
                        ids.iter().map(|_| (random_article(&mut random), embedding(&mut random))).collect();
                    index.set_many(ids.iter().zip(&values).map(|(id, (value, vector))| (id, value, vector))).unwrap();
                    for (id, (value, vector)) in ids.into_iter().zip(values) {
                        model.set_with(&settings, id, value, Some(vector));
                    }
                }
                76..78 => drop(index.search.pack_step(&index.open, 1 + random.below(3000) as usize).unwrap()),
                78..79 => index.search.purge_in_steps(&index.open, 1 + random.below(40) as usize, || {}).unwrap(),
                79..85 => agrees(&index, &model, &mut random, seed),
                _ => agrees_by_vector(&index, &model, &mut random, seed),
            }
        }
        let Fixture { dir, store } = fixture;
        drop(index);
        store.close().unwrap();
        let reopened = Fixture { store: open(dir.path()), dir };
        let index = products(&reopened);
        for _ in 0..20 {
            agrees_by_vector(&index, &model, &mut random, seed);
        }
    }
}

/// How long an exact search by a vector takes over 100,000 vectors of 1,024
/// dimensions, each number a sum of three uniform ones. Run by hand:
/// `cargo test -p tinystore --release --lib a_search_by_a_vector_over -- --ignored --nocapture`.
#[test]
#[ignore = "a timing, run by hand"]
fn a_search_by_a_vector_over_a_hundred_thousand_takes() {
    let fixture = fixture();
    let index = fixture.store.index::<String>("timing").vectors(1024).open().unwrap();
    let mut random = Random::new(1);
    let mut normal = |_| (0..3).map(|_| random.below(2001) as f32 / 1000.0 - 1.0).sum::<f32>();
    let empty = String::new();
    for batch in 0..100 {
        let vectors: Vec<Vec<f32>> = (0..1000).map(|_| (0..1024).map(&mut normal).collect()).collect();
        let ids: Vec<String> = (0..1000).map(|n| format!("{batch}-{n}")).collect();
        index.set_many(ids.iter().zip(&vectors).map(|(id, vector)| (id, &empty, vector))).unwrap();
    }
    let questions: Vec<Vec<f32>> = (0..21).map(|_| (0..1024).map(&mut normal).collect()).collect();
    index.search("").vector(&questions[0]).run().unwrap();
    let mut took: Vec<f64> = questions[1..]
        .iter()
        .map(|question| {
            let started = std::time::Instant::now();
            index.search("").vector(question).run().unwrap();
            started.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    took.sort_by(f64::total_cmp);
    eprintln!("a search by a vector over 100,000: {:.1} ms at the median, {:.1} at most", took[10], took[19]);
}

/// A condition as the tests ask it, which the engine is given and the model
/// passes documents by itself.
#[derive(Clone, Debug)]
enum Test {
    Eq(&'static str, serde_json::Value),
    In(&'static str, Vec<serde_json::Value>),
    /// From the first, inclusive, to the second, exclusive.
    Range(&'static str, Option<i64>, Option<i64>),
    Or(Vec<Test>),
    And(Vec<Test>),
}

const CATEGORIES: &[&str] = &["shoes", "boots", "hats", "bags", "socks", "coats"];

/// An article with a category mostly, a price mostly, and tags of which
/// some few documents hold each, so that a filter by one passes few.
fn random_product(random: &mut Random) -> Article {
    let category = (random.below(6) != 0).then(|| CATEGORIES[random.below(6) as usize].to_owned());
    let price = (random.below(8) != 0).then(|| random.below(2000) as i64 - 300);
    let tags = (0..random.below(4)).map(|_| format!("tag{}", random.skewed(60))).collect();
    Article { category, price, tags, ..random_article(random) }
}

/// The values a document holds at a filter's path.
fn values_of(article: &Article, path: &str) -> Vec<serde_json::Value> {
    match path {
        "category" => article.category.iter().map(|category| serde_json::json!(category)).collect(),
        "price" => article.price.iter().map(|price| serde_json::json!(price)).collect(),
        _ => article.tags.iter().map(|tag| serde_json::json!(tag)).collect(),
    }
}

impl Test {
    fn passes(&self, article: &Article) -> bool {
        match self {
            Test::Eq(path, serde_json::Value::Null) => values_of(article, path).is_empty(),
            Test::Eq(path, value) => values_of(article, path).contains(value),
            Test::In(path, values) => values.iter().any(|value| Test::Eq(path, value.clone()).passes(article)),
            Test::Range(path, low, high) => values_of(article, path)
                .iter()
                .filter_map(serde_json::Value::as_i64)
                .any(|number| low.is_none_or(|low| number >= low) && high.is_none_or(|high| number < high)),
            Test::Or(parts) => parts.iter().any(|part| part.passes(article)),
            Test::And(parts) => parts.iter().all(|part| part.passes(article)),
        }
    }

    fn condition(&self) -> crate::search::Condition {
        use crate::search::{and, eq, is_in, or, range};
        match self {
            Test::Eq(path, serde_json::Value::Null) => crate::search::missing(path),
            Test::Eq(path, value) => eq(path, value),
            Test::In(path, values) => is_in(path, values),
            Test::Range(path, Some(low), Some(high)) => range(path, *low..*high),
            Test::Range(path, Some(low), None) => range(path, *low..),
            Test::Range(path, None, Some(high)) => range(path, ..*high),
            Test::Range(path, None, None) => range::<i64>(path, ..),
            Test::Or(parts) => or(parts.iter().map(Test::condition)),
            Test::And(parts) => and(parts.iter().map(Test::condition)),
        }
    }

    fn random(random: &mut Random, depth: u32) -> Test {
        let tag = |random: &mut Random| serde_json::json!(format!("tag{}", random.skewed(60)));
        let price = |random: &mut Random| (random.below(5) != 0).then(|| random.below(2200) as i64 - 400);
        match random.below(if depth == 0 { 6 } else { 4 }) {
            0 => {
                let category = CATEGORIES[random.below(6) as usize];
                Test::Eq("category", if random.below(8) == 0 { serde_json::Value::Null } else { category.into() })
            }
            1 => Test::In("tags", (0..random.below(4)).map(|_| tag(random)).collect()),
            2 => Test::Eq("tags", tag(random)),
            3 => Test::Range("price", price(random), price(random)),
            4 => Test::Or((0..2).map(|_| Test::random(random, depth + 1)).collect()),
            _ => Test::And((0..2).map(|_| Test::random(random, depth + 1)).collect()),
        }
    }
}

fn shop(fixture: &Fixture) -> Index<Article> {
    let builder = fixture.store.index("shop").field("title", 2.0).field("body", 1.0);
    let builder = builder.filters(["category", "price", "tags"]).vectors(DIMENSIONS);
    builder.typo_tolerance(false).open().unwrap()
}

/// Checks a filtered search, by words, by a vector or by both, against the
/// model, which passes each document by the conditions itself.
fn agrees_filtered(index: &Index<Article>, model: &Model, random: &mut Random, seed: u64) {
    let settings = Settings::clone(&index.open.settings());
    let tests: Vec<Test> = (0..1 + random.below(2)).map(|_| Test::random(random, 0)).collect();
    let keep = |article: &Article| tests.iter().all(|test| test.passes(article));
    let (offset, limit) = (random.below(10) as usize, 1 + random.below(20) as usize);
    let (text, q) = (words(random, 4), embedding(random));
    let ask = Ask { text: &text, question: &q, keep: &keep };
    let (call, mut expected) = match random.below(4) {
        0 => (index.search(&text), model.best(&settings, &ask, offset + limit)),
        1 => (index.search_vector(&q), model.nearest(&ask, offset + limit)),
        2 if !text.trim().is_empty() => {
            (index.search(&text).vector(&q).rerank(), model.both(&settings, &ask, How::Rerank, offset + limit))
        }
        _ => (index.search(&text).vector(&q), model.both(&settings, &ask, How::Sum(0.5), offset + limit)),
    };
    let call = tests.iter().fold(call.offset(offset).limit(limit), |call, test| call.filter(test.condition()));
    let found = checked(|| call.run()).unwrap();
    expected.drain(..offset.min(expected.len()));
    let got: Vec<(String, f64)> = found.hits.iter().map(|hit| (hit.id.clone(), hit.score)).collect();
    assert_eq!(got, expected, "seed {seed}: {text:?} {tests:?}");
}

// contract: SRCH-03
#[test]
fn a_filtered_search_answers_what_scoring_every_document_it_passes_answers() {
    for seed in 0..4 {
        let fixture = fixture();
        let index = shop(&fixture);
        let mut random = Random::new(seed);
        let mut model = Model::default();
        let settings = Settings::clone(&index.open.settings());
        for _ in 0..1500 {
            match random.below(100) {
                0..60 => {
                    let (id, value) = (format!("d{}", random.below(400)), random_product(&mut random));
                    let vector = (random.below(10) != 0).then(|| embedding(&mut random));
                    set_with(&index, &id, &value, vector.as_deref());
                    model.set_with(&settings, id, value, vector);
                }
                60..70 => {
                    let id = format!("d{}", random.below(400));
                    assert_eq!(index.delete(&id).unwrap(), model.docs.remove(&id).is_some());
                }
                70..72 => drop(maintain(&fixture.store).unwrap()),
                72..74 => drop(index.search.pack_step(&index.open, 1 + random.below(3000) as usize).unwrap()),
                74..75 => index.search.purge_in_steps(&index.open, 1 + random.below(40) as usize, || {}).unwrap(),
                _ => agrees_filtered(&index, &model, &mut random, seed),
            }
        }
    }
}

#[test]
fn a_filter_keeps_a_search_to_its_documents_and_names_what_it_cannot_read() {
    use crate::search::{and, eq, is_in, or, range};
    let fixture = fixture();
    let shop = shop(&fixture);
    let item = |category: Option<&str>, price: Option<i64>, tags: &[&str]| Article {
        category: category.map(str::to_owned),
        price,
        tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
        ..article("red shoes", "")
    };
    shop.id("a").without_vector().set(&item(Some("shoes"), Some(50), &["sale"])).unwrap();
    shop.id("b").without_vector().set(&item(Some("boots"), Some(150), &["sale", "new"])).unwrap();
    shop.id("c").without_vector().set(&item(None, Some(-5), &[])).unwrap();
    let found = |condition| ids(&checked(|| shop.search("red").filter(condition).limit(20).run()).unwrap()).join(",");
    assert_eq!(found(eq("category", "shoes")), "a");
    assert_eq!(found(is_in("category", &["shoes", "boots"])), "a,b");
    assert_eq!(found(crate::search::missing("category")), "c", "missing passes a document with no value");
    assert_eq!(found(eq("tags", "new")), "b", "a list passes on any of its values");
    assert_eq!(found(range("price", 0..150)), "a");
    assert_eq!(found(range("price", ..=150)), "a,b,c");
    assert_eq!(found(or([eq("category", "boots"), range("price", ..0)])), "b,c");
    assert_eq!(found(and([eq("tags", "sale"), range("price", 100..)])), "b");
    assert_eq!(found(eq("category", "hats")), "", "a value no document holds passes none");
    assert_eq!(found(eq("price", "50")), "", "the text 50 is not the number 50");
    let kind = |condition| shop.search("red").filter(condition).run().map(drop).unwrap_err().kind();
    assert_eq!(kind(eq("color", "red")), ErrorKind::Invalid, "a field the index was not opened with");
    let said = shop.search("red").filter(eq("prise", &5)).run().map(drop).unwrap_err().to_string();
    assert!(said.ends_with("did you mean price?: invalid"), "{said}");
    assert_eq!(kind(eq("price", &serde_json::json!({ "a": 1 }))), ErrorKind::Invalid, "an object is no value");
    assert_eq!(kind(or((0..9).map(|n| eq("price", &n)))), ErrorKind::Limit);
    let refused = shop.id("d").without_vector().set(&Article { tags: vec!["x".repeat(300)], ..article("", "") });
    assert_eq!(refused.unwrap_err().kind(), ErrorKind::Limit);
}

/// Where a filter's own documents are best scored alone: 100,000 articles
/// with vectors of 256 dimensions, each tagged `1/m` one time in `m`, a
/// search by three common words and one by a vector timed on each path.
/// Run by hand:
/// `cargo test -p tinystore --release --lib where_a_filter_is_scored_alone -- --ignored --nocapture`.
#[test]
#[ignore = "a timing, run by hand"]
fn where_a_filter_is_scored_alone_takes() {
    use crate::search::read::NARROW;
    let fixture = fixture();
    let builder = fixture.store.index::<Article>("timing").field("title", 2.0).field("body", 1.0);
    let index = builder.filters(["tags"]).vectors(256).typo_tolerance(false).open().unwrap();
    let shares = [2, 8, 32, 128, 512, 2048, 8192];
    let mut random = Random::new(1);
    let normal = |random: &mut Random| (0..3).map(|_| random.below(2001) as f32 / 1000.0 - 1.0).sum::<f32>();
    for batch in 0..100 {
        let mut documents = Vec::new();
        for n in 0..1000 {
            let tags = shares.iter().filter(|m| random.below(**m) == 0).map(|m| format!("1/{m}")).collect();
            let vector: Vec<f32> = (0..256).map(|_| normal(&mut random)).collect();
            documents.push((format!("{batch}-{n}"), Article { tags, ..random_article(&mut random) }, vector));
        }
        index.set_many(documents.iter().map(|(id, value, vector)| (id, value, vector))).unwrap();
    }
    maintain(&fixture.store).unwrap();
    let texts: Vec<String> = (0..20).map(|_| words(&mut random, 3) + " bread").collect();
    let questions: Vec<Vec<f32>> = (0..10).map(|_| (0..256).map(|_| normal(&mut random)).collect()).collect();
    let median = |mut took: Vec<f64>| {
        took.sort_by(f64::total_cmp);
        took[took.len() / 2]
    };
    let time = |run: &dyn Fn() -> crate::Result<()>| {
        let started = std::time::Instant::now();
        run().unwrap();
        started.elapsed().as_secs_f64() * 1000.0
    };
    eprintln!("share   words alone  words crossed  vector alone  vector crossed   (ms, median)");
    for share in shares {
        let tag = format!("1/{share}");
        let mut row = Vec::new();
        for narrow in [true, false] {
            NARROW.set(Some(narrow));
            let filter = || crate::search::eq("tags", &tag);
            let took = texts.iter().map(|text| time(&|| index.search(text).filter(filter()).run().map(drop)));
            row.push(median(took.collect()));
            let took = questions.iter().map(|q| time(&|| index.search_vector(q).filter(filter()).run().map(drop)));
            row.push(median(took.collect()));
        }
        NARROW.set(None);
        eprintln!("1/{share:<6} {:>10.2} {:>14.2} {:>13.2} {:>15.2}", row[0], row[2], row[1], row[3]);
    }
}

/// Sets or deletes a random document of the mix, in the index and the model.
fn mixed(index: &Index<Article>, model: &mut Model, random: &mut Random) {
    let id = format!("d{}", random.below(300));
    if random.below(5) == 0 {
        assert_eq!(index.delete(&id).unwrap(), model.docs.remove(&id).is_some());
    } else {
        let value = random_article(random);
        index.set(&id, &value).unwrap();
        model.set(&index.open.settings(), id, value);
    }
}

// contract: SRCH-06
#[test]
fn writes_while_an_index_is_read_again_reach_its_new_rows_and_it_answers_by_them_once_switched() {
    for seed in 0..3 {
        let fixture = fixture();
        fn titles(store: &crate::Store) -> IndexBuilder<'_, Article> {
            store.index::<Article>("mix").field("title", 2.0).typo_tolerance(false)
        }
        let index = titles(&fixture.store).open().unwrap();
        let (mut random, mut model) = (Random::new(seed), Model::default());
        for _ in 0..300 {
            mixed(&index, &mut model, &mut random);
        }
        maintain(&fixture.store).unwrap();
        let both = titles(&fixture.store).field("body", 1.0).open().unwrap();
        for _ in 0..400 {
            match random.below(10) {
                0 => drop(both.search.copy_step(&both.open).unwrap()),
                1 => agrees(&both, &model, &mut random, seed),
                _ => mixed(&both, &mut model, &mut random),
            }
        }
        assert!(matches!(both.status().unwrap(), crate::search::Status::Rebuilding { .. }));
        maintain(&fixture.store).unwrap();
        assert_eq!(both.status().unwrap(), crate::search::Status::Ready);
        let model = model.read_by(&both.open.settings());
        for _ in 0..40 {
            agrees(&both, &model, &mut random, seed);
        }
        for (id, doc) in &model.docs {
            assert_eq!(both.get(id).unwrap().as_ref(), Some(&doc.value), "seed {seed}: {id}");
        }
    }
}

#[test]
fn an_index_rebuilt_by_hand_refuses_a_change_that_needs_it_until_reindex_asks() {
    use crate::search::Rebuild;
    let fixture = fixture();
    fn manual(store: &crate::Store) -> IndexBuilder<'_, Article> {
        store.index::<Article>("articles").field("title", 2.0).rebuild(Rebuild::Manual)
    }
    let articles = manual(&fixture.store).open().unwrap();
    articles.set("a", &article("bread", "salt")).unwrap();
    let refused = manual(&fixture.store).field("body", 1.0).open().unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid);
    assert!(refused.to_string().contains("body added"), "{refused}");
    let reindexed = manual(&fixture.store).field("body", 1.0).reindex().unwrap();
    maintain(&fixture.store).unwrap();
    reindexed.ready().unwrap();
    assert_eq!(ids(&search(&reindexed, "salt")), ["a"]);
}

#[test]
fn a_rebuild_that_cannot_read_a_document_fails_and_the_index_answers_as_before() {
    let fixture = fixture();
    let notes = fixture.store.index::<serde_json::Value>("notes").field("title", 1.0).open().unwrap();
    notes.set("n", &serde_json::json!({ "title": "bread", "year": 2024 })).unwrap();
    let years =
        fixture.store.index::<serde_json::Value>("notes").field("title", 1.0).field("year", 1.0).open().unwrap();
    maintain(&fixture.store).unwrap();
    let crate::search::Status::Failed { error } = years.status().unwrap() else { panic!("{:?}", years.status()) };
    assert!(error.contains("field year holds a number"), "{error}");
    assert_eq!(years.ready().unwrap_err().kind(), ErrorKind::Invalid);
    assert_eq!(checked(|| years.search("bread").run()).unwrap().hits.len(), 1, "the index answers as before");
    years.set("m", &serde_json::json!({ "title": "salt" })).unwrap();
    let back = fixture.store.index::<serde_json::Value>("notes").field("title", 1.0).open().unwrap();
    assert_eq!(back.status().unwrap(), crate::search::Status::Failed { error }, "until another change asks");
    let again = fixture.store.index::<serde_json::Value>("notes").field("title", 1.0).reindex().unwrap();
    maintain(&fixture.store).unwrap();
    again.ready().unwrap();
}

#[test]
fn ready_on_a_store_without_background_work_runs_maintenance_itself() {
    let fixture = fixture();
    let titles = fixture.store.index::<Article>("articles").field("title", 2.0).open().unwrap();
    titles.set("a", &article("bread", "salt")).unwrap();
    let bodies = fixture.store.index::<Article>("articles").field("title", 2.0).field("body", 1.0).open().unwrap();
    bodies.ready().unwrap();
    assert_eq!(ids(&search(&bodies, "salt")), ["a"]);
}

/// How many numbers an index has given.
fn numbers_given<T>(index: &Index<T>) -> i64 {
    let id = index.open.id();
    index
        .search
        .file_read(|connection| {
            connection
                .query_row("select next_doc - 1 from _tinystore_search_indexes where id = ?1", [id], |row| row.get(0))
                .map_err(|error| crate::sqlite::sql_error("read numbers", error))
        })
        .unwrap()
}

#[test]
fn an_index_whose_numbers_pass_twice_its_documents_is_numbered_again_and_answers_alike() {
    let fixture = fixture();
    let articles = fixture.articles::<Article>("articles");
    let mut random = Random::new(21);
    for n in 0..600 {
        articles.set(n % 40, &random_article(&mut random)).unwrap();
    }
    let texts: Vec<String> = (0..20).map(|_| words(&mut random, 3)).collect();
    let answers = |articles: &Index<Article>| -> Vec<Found<Article>> {
        texts.iter().map(|text| checked(|| articles.search(text).limit(20).run()).unwrap()).collect()
    };
    let before = answers(&articles);
    assert_eq!(numbers_given(&articles), 600);
    maintain(&fixture.store).unwrap();
    maintain(&fixture.store).unwrap();
    assert_eq!(numbers_given(&articles), 40, "numbered again from 1");
    assert_eq!(answers(&articles), before);
}

// contract: SRCH-06
#[test]
fn an_index_reopened_in_the_middle_of_a_rebuild_goes_on_from_where_it_stood() {
    let fixture = fixture();
    let articles = fixture.store.index::<Article>("articles").field("title", 2.0).open().unwrap();
    let mut random = Random::new(22);
    for n in 0..900 {
        articles.set(n, &random_article(&mut random)).unwrap();
    }
    let both = fixture.store.index::<Article>("articles").field("title", 2.0).field("body", 1.0).open().unwrap();
    both.search.copy_step(&both.open).unwrap();
    let mut images = vec![crash_image(fixture.dir.path())];
    while both.search.copy_step(&both.open).unwrap() {
        images.push(crash_image(fixture.dir.path()));
    }
    let crate::search::Status::Rebuilding { done } = both.status().unwrap() else { panic!() };
    assert!(done > 0.5, "{done}");
    let text = words(&mut random, 3) + " bread";
    maintain(&fixture.store).unwrap();
    let answer = checked(|| both.search(&text).run()).unwrap();
    for image in images {
        let store = open(image.path());
        let reopened = store.index::<Article>("articles").field("title", 2.0).field("body", 1.0).open().unwrap();
        assert!(matches!(reopened.status().unwrap(), crate::search::Status::Rebuilding { .. }));
        maintain(&store).unwrap();
        reopened.ready().unwrap();
        assert_eq!(checked(|| reopened.search(&text).run()).unwrap(), answer);
    }
}

#[test]
fn a_dropped_index_refuses_its_handles_and_leaves_no_row() {
    let fixture = fixture();
    let articles = fixture.articles::<Article>("articles");
    let kept = fixture.articles::<Article>("kept");
    let mut random = Random::new(23);
    for n in 0..200 {
        articles.set(n, &random_article(&mut random)).unwrap();
        kept.set(n, &random_article(&mut random)).unwrap();
    }
    maintain(&fixture.store).unwrap();
    assert!(fixture.store.drop_index("articles").unwrap());
    assert!(!fixture.store.drop_index("articles").unwrap(), "dropping again finds none");
    assert_eq!(articles.set("x", &article("a", "b")).unwrap_err().kind(), ErrorKind::Invalid);
    assert_eq!(articles.search("bread").run().unwrap_err().kind(), ErrorKind::Invalid);
    maintain(&fixture.store).unwrap();
    let listed = crate::search::list(&fixture.store).unwrap();
    assert_eq!(listed.iter().map(|index| index.name.as_str()).collect::<Vec<_>>(), ["kept"]);
    assert_eq!(listed[0].documents, 200);
    assert!(listed[0].opened.is_some());
    let rows: i64 = kept
        .search
        .file_read(|connection| {
            connection
                .query_row(
                    "select count(*) from _tinystore_search_docs where index_id != ?1",
                    [kept.open.id()],
                    |row| row.get(0),
                )
                .map_err(|error| crate::sqlite::sql_error("count", error))
        })
        .unwrap();
    assert_eq!(rows, 0, "every row of the dropped index is gone");
    let again = fixture.articles::<Article>("articles");
    assert!(search(&again, "bread").hits.is_empty(), "the name opens a new index");
}
