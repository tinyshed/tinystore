//! An index's settings: its fields, each with its weight and a number of
//! its own for good, its filters, numbered the same way, whether a misspelt
//! word is read as the nearest, its vectors' dimensions, and how a search
//! by words and a vector at once combines them, and for a table's index the
//! table it follows. They are kept as JSON in the index's row, and an open
//! that asks for others changes them.
//!
//! ```text
//! {"fields":[{"path":"title","weight":2.0,"number":0}],"next_field":1,"typo_tolerance":true,"analysis":1,
//!  "filters":[{"path":"category","number":0}],"next_filter":1,
//!  "vectors":{"dimensions":1024,"recall":null},"hybrid":{"vector_weight":0.5,"rerank":false},
//!  "table":{"name":"articles","key":"id","vector":"embedding"}}
//! ```

use serde_json::{Value, json};

use super::analyzer;
use super::vectors::MOST_DIMENSIONS;
use crate::{Error, Result};

/// A field: where its text lies in a document, what a word found there
/// weighs, and its number, which names its parts and is never given again.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Field {
    pub(crate) path: String,
    pub(crate) weight: f64,
    pub(crate) number: u32,
}

/// A filter: where its values lie in a document, and its number, which
/// names its part and is never given again.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Filter {
    pub(crate) path: String,
    pub(crate) number: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Settings {
    pub(crate) fields: Vec<Field>,
    pub(crate) next_field: u32,
    pub(crate) filters: Vec<Filter>,
    pub(crate) next_filter: u32,
    pub(crate) typo_tolerance: bool,
    pub(crate) analysis: u32,
    pub(crate) vectors: Option<Vectors>,
    pub(crate) hybrid: Hybrid,
    pub(crate) table: Option<Table>,
}

/// The table of a database that a table's index follows: its rows are the
/// index's documents by `key`, a column of unique values, and their vectors
/// are the column `vector`'s, when it keeps them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Table {
    pub(crate) name: String,
    pub(crate) key: String,
    pub(crate) vector: Option<String>,
}

impl Table {
    fn to_json(&self) -> Value {
        json!({ "name": self.name, "key": self.key, "vector": self.vector })
    }

    /// A table as its settings keep it, none for an index of documents;
    /// none outer when it does not read.
    fn from_json(table: &Value) -> Option<Option<Table>> {
        if table.is_null() {
            return Some(None);
        }
        let name = table["name"].as_str()?.to_owned();
        let key = table["key"].as_str()?.to_owned();
        Some(Some(Table { name, key, vector: table["vector"].as_str().map(str::to_owned) }))
    }
}

/// The vectors an index keeps: of how many dimensions each, and the share
/// of the exact ten a search by one finds from clusters, when it is not
/// exact.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Vectors {
    pub(crate) dimensions: usize,
    pub(crate) recall: Option<f64>,
}

/// How a search by words and a vector at once combines them, unless it says:
/// their scores summed at `vector_weight`, or the words' hits ordered by the
/// vector.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Hybrid {
    pub(crate) vector_weight: f64,
    pub(crate) rerank: bool,
}

/// What an open asks for: the fields by their paths and weights, none for a
/// document that is its own text.
#[derive(Clone, Debug)]
pub(crate) struct Wanted {
    pub(crate) fields: Vec<(String, f64)>,
    pub(crate) filters: Vec<String>,
    pub(crate) typo_tolerance: bool,
    pub(crate) dimensions: Option<usize>,
    pub(crate) recall: Option<f64>,
    pub(crate) vector_weight: Option<f64>,
    pub(crate) rerank: bool,
    /// Whether a change that needs the index read again waits for `reindex`.
    pub(crate) manual: bool,
    pub(crate) table: Option<Table>,
}

impl Default for Wanted {
    /// An index of a document that is its own text, typos forgiven.
    fn default() -> Self {
        Wanted {
            fields: Vec::new(),
            filters: Vec::new(),
            typo_tolerance: true,
            dimensions: None,
            recall: None,
            vector_weight: None,
            rerank: false,
            manual: false,
            table: None,
        }
    }
}

/// What a change of settings makes of an index.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Changed {
    /// The settings that change at once.
    pub(crate) settings: Settings,
    pub(crate) said: Vec<String>,
    pub(crate) rebuild: Option<Asked>,
}

/// A rebuild a change asks for: why, as the log says it, and whether the
/// index needs it to serve what was asked, or only to drop what it keeps
/// no longer, which an index rebuilt by hand leaves for its next rebuild.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Asked {
    pub(crate) why: String,
    pub(crate) needed: bool,
}

/// The rebuild a change gathers, reason by reason.
#[derive(Default)]
struct Rebuild {
    needed: Vec<String>,
    cleaning: Vec<String>,
}

impl Rebuild {
    fn needs(&mut self, why: String) {
        self.needed.push(why);
    }

    fn cleans(&mut self, why: String, documents: u64) {
        if documents > 0 {
            self.cleaning.push(why);
        }
    }

    fn asked(self) -> Option<Asked> {
        let needed = !self.needed.is_empty();
        let why: Vec<String> = self.needed.into_iter().chain(self.cleaning).collect();
        (!why.is_empty()).then(|| Asked { why: why.join(", "), needed })
    }
}

/// The weight of a vector a search by both gives unless its index or it
/// says: search-hybrid found no weight that won both its collections.
pub(crate) const VECTOR_WEIGHT: f64 = 0.5;

/// The heaviest weight a field may have: a thousand to one is far past what
/// orders hits, and a score of a search of 32 words stays within its points.
const MOST_WEIGHT: f64 = 1000.0;

impl Wanted {
    /// The fields asked for, the document itself when none was.
    fn fields(&self) -> Vec<(String, f64)> {
        if self.fields.is_empty() { vec![(String::new(), 1.0)] } else { self.fields.clone() }
    }

    pub(crate) fn check(&self) -> Result<()> {
        for (at, (path, weight)) in self.fields.iter().enumerate() {
            check_path(path)?;
            if !(weight.is_finite() && *weight > 0.0 && *weight <= MOST_WEIGHT) {
                return Err(Error::invalid(format!(
                    "field {path} weighs {weight}: a weight is above 0, at most 1,000"
                )));
            }
            if self.fields[..at].iter().any(|(earlier, _)| earlier == path) {
                return Err(Error::invalid(format!("field {path} is given twice")));
            }
        }
        for (at, path) in self.filters.iter().enumerate() {
            check_path(path)?;
            if self.filters[..at].contains(path) {
                return Err(Error::invalid(format!("filter {path} is given twice")));
            }
        }
        self.check_table()?;
        self.check_vectors()
    }

    /// Refuses a table's index that reads no column as its text, a path
    /// into a column's value, and vectors from no column.
    fn check_table(&self) -> Result<()> {
        let Some(table) = &self.table else { return Ok(()) };
        if self.fields.is_empty() {
            return Err(Error::invalid("a table's index reads at least one column as text: field(column, weight)"));
        }
        let paths = self.fields.iter().map(|(path, _)| path).chain(&self.filters);
        if let Some(path) = paths.into_iter().find(|path| path.contains('.')) {
            return Err(Error::invalid(format!("a table's index reads columns, and {path:?} names none")));
        }
        if self.dimensions.is_some() != table.vector.is_some() {
            return Err(Error::invalid("a table's index reads its vectors from a column: vectors(column, dimensions)"));
        }
        Ok(())
    }

    fn check_vectors(&self) -> Result<()> {
        match self.dimensions {
            Some(0) => return Err(Error::invalid("vectors of 0 dimensions")),
            Some(dimensions) if dimensions > MOST_DIMENSIONS => {
                return Err(Error::limit(format!("vectors of {dimensions} dimensions, past {MOST_DIMENSIONS}")));
            }
            None if self.vector_weight.is_some() || self.rerank || self.recall.is_some() => {
                return Err(Error::invalid("a vector weight, a rerank or a recall for an index without vectors"));
            }
            _ => {}
        }
        if let Some(recall) = self.recall.filter(|recall| !(recall > &0.0 && recall < &1.0)) {
            return Err(Error::invalid(format!(
                "a recall of {recall}: it is above 0 and below 1, the exact search being the default"
            )));
        }
        check_hybrid(self.vector_weight, self.rerank)
    }
}

/// Refuses a path that names no field, or fields joined by dots.
pub(crate) fn check_path(path: &str) -> Result<()> {
    if path.is_empty() || path.split('.').any(str::is_empty) {
        return Err(Error::invalid(format!("a path names a field, or fields joined by dots: {path:?}")));
    }
    Ok(())
}

/// Refuses a vector weight outside 0–1, and one beside a rerank, which
/// orders by the vector alone.
pub(crate) fn check_hybrid(vector_weight: Option<f64>, rerank: bool) -> Result<()> {
    match vector_weight {
        Some(weight) if !(0.0..=1.0).contains(&weight) => {
            Err(Error::invalid(format!("a vector weight of {weight}: it is from 0, the words alone, to 1")))
        }
        Some(_) if rerank => Err(Error::invalid("a vector weight beside a rerank, which orders by the vector alone")),
        _ => Ok(()),
    }
}

impl Settings {
    pub(crate) fn new(wanted: &Wanted) -> Settings {
        let fields: Vec<Field> =
            (0..).zip(wanted.fields()).map(|(number, (path, weight))| Field { path, weight, number }).collect();
        let next_field = fields.len() as u32;
        let filters: Vec<Filter> =
            (0..).zip(&wanted.filters).map(|(number, path)| Filter { path: path.clone(), number }).collect();
        Settings {
            fields,
            next_field,
            next_filter: filters.len() as u32,
            filters,
            typo_tolerance: wanted.typo_tolerance,
            analysis: analyzer::VERSION,
            vectors: wanted.dimensions.map(|dimensions| Vectors { dimensions, recall: wanted.recall }),
            hybrid: Hybrid::of(wanted),
            table: wanted.table.clone(),
        }
    }

    /// What `wanted` changes of these: the settings that change at once, each
    /// change as the log says it, and what needs the index read again beside
    /// it. A weight, a field or a filter removed, typo tolerance, vectors
    /// added, recall and how a search by both combines them change at once,
    /// a field or a filter removed leaving its keys for a rebuild to drop; a
    /// field or a filter added to an index that holds documents needs them
    /// read again, as do words read by another release. Other vectors than
    /// those kept are refused: the store has nothing to make them from, but
    /// a table's index reads them again from their column. A table's index
    /// follows the table and key it was made for.
    pub(crate) fn change(&self, wanted: &Wanted, documents: u64) -> Result<Changed> {
        self.check_same_table(wanted)?;
        let mut changed = Settings { fields: Vec::new(), ..self.clone() };
        let mut said = Vec::new();
        let mut rebuild = Rebuild::default();
        changed.change_filters(wanted, documents, (&mut said, &mut rebuild));
        changed.change_vectors(wanted, documents, (&mut said, &mut rebuild))?;
        for (path, weight) in wanted.fields() {
            let Some(field) = self.fields.iter().find(|field| field.path == path) else {
                if documents > 0 {
                    rebuild.needs(format!("{} added", shown(&path)));
                    continue;
                }
                said.push(format!("{} added", shown(&path)));
                changed.fields.push(Field { path, weight, number: changed.next_field });
                changed.next_field += 1;
                continue;
            };
            if field.weight != weight {
                said.push(format!("{} weighs {weight}, was {}; at once", shown(&path), field.weight));
            }
            changed.fields.push(Field { weight, ..field.clone() });
        }
        for field in &self.fields {
            if !changed.fields.iter().any(|kept| kept.number == field.number) {
                said.push(format!("{} removed; at once from searches", shown(&field.path)));
                rebuild.cleans(format!("{} removed", shown(&field.path)), documents);
            }
        }
        if wanted.typo_tolerance != self.typo_tolerance {
            let now = if wanted.typo_tolerance { "on" } else { "off" };
            said.push(format!("typo tolerance {now}; at once"));
            changed.typo_tolerance = wanted.typo_tolerance;
        }
        if self.analysis != analyzer::VERSION {
            rebuild.needs("words read by another release".to_owned());
        }
        Ok(Changed { settings: changed, said, rebuild: rebuild.asked() })
    }

    /// Whether these settings read documents as `other` does: the same fields
    /// and filters by their paths, vectors of the same dimensions, the same
    /// analysis and table, whatever their numbers, weights and the rest that
    /// changes at once.
    pub(crate) fn reads_as(&self, other: &Settings) -> bool {
        let fields = |settings: &Settings| settings.fields.iter().map(|field| field.path.clone()).collect::<Vec<_>>();
        let filters = |settings: &Settings| settings.filters.iter().map(|f| f.path.clone()).collect::<Vec<_>>();
        let dimensions = |settings: &Settings| settings.vectors.map(|vectors| vectors.dimensions);
        fields(self) == fields(other)
            && filters(self) == filters(other)
            && dimensions(self) == dimensions(other)
            && self.analysis == other.analysis
            && self.table == other.table
    }

    /// Refuses another table or key than the index follows: its documents
    /// would be another's.
    fn check_same_table(&self, wanted: &Wanted) -> Result<()> {
        let followed = |table: &Option<Table>| table.as_ref().map(|table| (table.name.clone(), table.key.clone()));
        let (kept, asked) = (followed(&self.table), followed(&wanted.table));
        if kept == asked {
            return Ok(());
        }
        let shown = |table: Option<(String, String)>| {
            table.map_or_else(|| "documents of its own".to_owned(), |(name, key)| format!("table {name} by {key}"))
        };
        Err(Error::invalid(format!(
            "the index follows {}, not {}: drop it to make one that does",
            shown(kept),
            shown(asked)
        )))
    }

    /// Keeps the filters `wanted` names, each with its number; one removed
    /// leaves searches at once, and one added to an index that holds
    /// documents needs them read again, as a field does.
    fn change_filters(&mut self, wanted: &Wanted, documents: u64, (said, rebuild): (&mut Vec<String>, &mut Rebuild)) {
        let mut filters = Vec::with_capacity(wanted.filters.len());
        for path in &wanted.filters {
            if let Some(kept) = self.filters.iter().find(|filter| &filter.path == path) {
                filters.push(kept.clone());
                continue;
            }
            if documents > 0 {
                rebuild.needs(format!("filter {path} added"));
                continue;
            }
            said.push(format!("filter {path} added"));
            filters.push(Filter { path: path.clone(), number: self.next_filter });
            self.next_filter += 1;
        }
        for filter in &self.filters {
            if !filters.iter().any(|kept| kept.number == filter.number) {
                said.push(format!("filter {} removed; at once from searches", filter.path));
                rebuild.cleans(format!("filter {} removed", filter.path), documents);
            }
        }
        self.filters = filters;
    }

    /// The filter of `path`, while the index has it.
    pub(crate) fn filter(&self, path: &str) -> Option<&Filter> {
        self.filters.iter().find(|filter| filter.path == path)
    }

    fn change_vectors(
        &mut self,
        wanted: &Wanted,
        documents: u64,
        (said, rebuild): (&mut Vec<String>, &mut Rebuild),
    ) -> Result<()> {
        match (self.vectors, wanted.dimensions) {
            _ if self.table.is_some() => self.change_column(wanted, documents, (said, rebuild))?,
            (Some(kept), Some(dimensions)) if kept.dimensions != dimensions => {
                return Err(Error::invalid(format!(
                    "vectors of {dimensions} dimensions where the index keeps {}: they are the program's model's, \
                     and the store has nothing to make new ones from",
                    kept.dimensions
                )));
            }
            (Some(kept), None) => {
                return Err(Error::invalid(format!(
                    "opened without vectors, an index that keeps vectors of {} dimensions would lose them",
                    kept.dimensions
                )));
            }
            (None, Some(dimensions)) => {
                let held =
                    if documents > 0 { format!("; the {documents} documents held have none") } else { "".into() };
                said.push(format!("vectors of {dimensions} dimensions added{held}"));
                self.vectors = Some(Vectors { dimensions, recall: None });
            }
            _ => {}
        }
        self.change_recall(wanted, said);
        self.change_hybrid(wanted, said);
        Ok(())
    }

    fn change_recall(&mut self, wanted: &Wanted, said: &mut Vec<String>) {
        if let Some(vectors) = self.vectors.as_mut().filter(|vectors| vectors.recall != wanted.recall) {
            said.push(match wanted.recall {
                Some(recall) => format!("recall {recall}: clusters built in the background, searches exact until then"),
                None => "recall removed: searches exact at once, clusters dropped in the background".to_owned(),
            });
            vectors.recall = wanted.recall;
        }
    }

    fn change_hybrid(&mut self, wanted: &Wanted, said: &mut Vec<String>) {
        let hybrid = Hybrid::of(wanted);
        if hybrid.vector_weight != self.hybrid.vector_weight {
            said.push(format!("a vector weighs {}, was {}; at once", hybrid.vector_weight, self.hybrid.vector_weight));
        }
        if hybrid.rerank != self.hybrid.rerank {
            said.push(format!("rerank {}; at once", if hybrid.rerank { "on" } else { "off" }));
        }
        self.hybrid = hybrid;
    }

    /// A table's index's vectors as `wanted` reads them: at once while it
    /// holds no document, and by reading the column again once it does.
    fn change_column(
        &mut self,
        wanted: &Wanted,
        documents: u64,
        (said, rebuild): (&mut Vec<String>, &mut Rebuild),
    ) -> Result<()> {
        let column = |table: &Option<Table>| table.as_ref().and_then(|table| table.vector.clone());
        let (kept, asked) = (column(&self.table), column(&wanted.table));
        let kept_dimensions = self.vectors.map(|vectors| vectors.dimensions);
        if (kept.as_ref(), kept_dimensions) == (asked.as_ref(), wanted.dimensions) {
            return Ok(());
        }
        let Some(column) = asked else {
            return Err(Error::invalid(format!(
                "opened without vectors, an index that reads them from column {} would lose them",
                kept.unwrap_or_default()
            )));
        };
        let dimensions = wanted.dimensions.unwrap_or_default();
        let change = format!("vectors of {dimensions} dimensions from column {column}");
        if documents > 0 {
            rebuild.needs(change);
            return Ok(());
        }
        said.push(change);
        self.vectors = Some(Vectors { dimensions, recall: wanted.recall });
        if let Some(table) = &mut self.table {
            table.vector = Some(column);
        }
        Ok(())
    }

    /// The weight of the field numbered `number`, while the index has it.
    pub(crate) fn weight(&self, number: u32) -> Option<f64> {
        self.fields.iter().find(|field| field.number == number).map(|field| field.weight)
    }

    pub(crate) fn to_json(&self) -> String {
        let fields: Vec<Value> = self
            .fields
            .iter()
            .map(|field| json!({ "path": field.path, "weight": field.weight, "number": field.number }))
            .collect();
        let vectors = self.vectors.map(|vectors| json!({ "dimensions": vectors.dimensions, "recall": vectors.recall }));
        let filters: Vec<Value> =
            self.filters.iter().map(|filter| json!({ "path": filter.path, "number": filter.number })).collect();
        let settings = json!({
            "fields": fields,
            "next_field": self.next_field,
            "filters": filters,
            "next_filter": self.next_filter,
            "typo_tolerance": self.typo_tolerance,
            "analysis": self.analysis,
            "vectors": vectors,
            "hybrid": { "vector_weight": self.hybrid.vector_weight, "rerank": self.hybrid.rerank },
            "table": self.table.as_ref().map(Table::to_json),
        });
        settings.to_string()
    }

    pub(crate) fn from_json(text: &str) -> Result<Settings> {
        let broken = || Error::corrupt("an index's settings that do not read");
        let settings: Value = serde_json::from_str(text).map_err(|error| broken().with_source(error))?;
        let number = |value: &Value| value.as_u64().and_then(|number| u32::try_from(number).ok()).ok_or_else(broken);
        let mut fields = Vec::new();
        for field in settings["fields"].as_array().ok_or_else(broken)? {
            let path = field["path"].as_str().ok_or_else(broken)?.to_owned();
            let weight = field["weight"].as_f64().ok_or_else(broken)?;
            fields.push(Field { path, weight, number: number(&field["number"])? });
        }
        let mut filters = Vec::new();
        for filter in settings["filters"].as_array().ok_or_else(broken)? {
            let path = filter["path"].as_str().ok_or_else(broken)?.to_owned();
            filters.push(Filter { path, number: number(&filter["number"])? });
        }
        let vectors = match &settings["vectors"] {
            Value::Null => None,
            vectors => Some(Vectors {
                dimensions: number(&vectors["dimensions"])? as usize,
                recall: vectors["recall"].as_f64(),
            }),
        };
        let hybrid = &settings["hybrid"];
        let table = Table::from_json(&settings["table"]).ok_or_else(broken)?;
        Ok(Settings {
            fields,
            next_field: number(&settings["next_field"])?,
            filters,
            next_filter: number(&settings["next_filter"])?,
            typo_tolerance: settings["typo_tolerance"].as_bool().ok_or_else(broken)?,
            analysis: number(&settings["analysis"])?,
            vectors,
            hybrid: Hybrid {
                vector_weight: hybrid["vector_weight"].as_f64().ok_or_else(broken)?,
                rerank: hybrid["rerank"].as_bool().ok_or_else(broken)?,
            },
            table,
        })
    }
}

impl Hybrid {
    fn of(wanted: &Wanted) -> Hybrid {
        Hybrid { vector_weight: wanted.vector_weight.unwrap_or(VECTOR_WEIGHT), rerank: wanted.rerank }
    }
}

/// A field's path as an error or the log says it; the document itself when
/// it is its own text.
pub(crate) fn shown(path: &str) -> String {
    if path.is_empty() { "the document's text".to_owned() } else { path.to_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wanted(fields: &[(&str, f64)]) -> Wanted {
        Wanted {
            fields: fields.iter().map(|(path, weight)| ((*path).to_owned(), *weight)).collect(),
            filters: Vec::new(),
            typo_tolerance: true,
            dimensions: None,
            recall: None,
            vector_weight: None,
            rerank: false,
            manual: false,
            table: None,
        }
    }

    fn with_vectors(dimensions: usize, vector_weight: Option<f64>, rerank: bool) -> Wanted {
        Wanted { dimensions: Some(dimensions), vector_weight, rerank, ..wanted(&[("title", 1.0)]) }
    }

    #[test]
    fn settings_read_back_as_written() {
        let settings = Settings::new(&wanted(&[("title", 2.0), ("body", 1.0)]));
        assert_eq!(Settings::from_json(&settings.to_json()).unwrap(), settings);
        let settings = Settings::new(&with_vectors(1024, Some(0.7), false));
        assert_eq!(Settings::from_json(&settings.to_json()).unwrap(), settings);
    }

    #[test]
    fn a_filter_removed_leaves_at_once_and_one_added_waits_for_an_empty_index_as_a_field_does() {
        let with =
            |filters: &[&str]| Wanted { filters: filters.iter().map(|f| (*f).to_owned()).collect(), ..wanted(&[]) };
        let settings = Settings::new(&with(&["category", "owner"]));
        assert_eq!(Settings::from_json(&settings.to_json()).unwrap(), settings);
        let removed = settings.change(&with(&["owner"]), 5).unwrap();
        assert_eq!(removed.said, ["filter category removed; at once from searches"]);
        assert_eq!(removed.settings.filters, [Filter { path: "owner".to_owned(), number: 1 }]);
        let cleaning = Asked { why: "filter category removed".to_owned(), needed: false };
        assert_eq!(removed.rebuild, Some(cleaning), "its keys left for a rebuild to drop");
        let changed = removed.settings;
        let added = changed.change(&with(&["owner", "category"]), 5).unwrap();
        assert_eq!(added.rebuild, Some(Asked { why: "filter category added".to_owned(), needed: true }));
        assert_eq!(added.settings, changed, "nothing changes at once");
        let added = changed.change(&with(&["owner", "category"]), 0).unwrap().settings;
        assert_eq!(added.filter("category").unwrap().number, 2, "a filter added again takes a new number");
        assert!(with(&["a", "a"]).check().is_err());
        assert!(with(&["a..b"]).check().is_err());
    }

    #[test]
    fn vectors_added_change_at_once_and_other_dimensions_or_none_are_refused() {
        let settings = Settings::new(&wanted(&[("title", 1.0)]));
        let Changed { settings: changed, said, .. } = settings.change(&with_vectors(3, None, false), 2).unwrap();
        assert_eq!(said, ["vectors of 3 dimensions added; the 2 documents held have none"]);
        assert_eq!(changed.vectors, Some(Vectors { dimensions: 3, recall: None }));
        assert_eq!(changed.change(&with_vectors(4, None, false), 2).unwrap_err().kind(), crate::ErrorKind::Invalid);
        assert_eq!(changed.change(&wanted(&[("title", 1.0)]), 2).unwrap_err().kind(), crate::ErrorKind::Invalid);
        let said = changed.change(&with_vectors(3, Some(0.7), false), 2).unwrap().said;
        assert_eq!(said, ["a vector weighs 0.7, was 0.5; at once"]);
        let said = changed.change(&with_vectors(3, None, true), 2).unwrap().said;
        assert_eq!(said, ["rerank on; at once"]);
    }

    #[test]
    fn a_vector_weight_is_from_zero_to_one_and_never_beside_a_rerank() {
        assert!(with_vectors(8, Some(1.0), false).check().is_ok());
        assert!(with_vectors(8, Some(1.5), false).check().is_err());
        assert!(with_vectors(8, Some(f64::NAN), false).check().is_err());
        assert!(with_vectors(8, Some(0.5), true).check().is_err());
        assert!(with_vectors(0, None, false).check().is_err());
        assert_eq!(with_vectors(4097, None, false).check().unwrap_err().kind(), crate::ErrorKind::Limit);
        assert!(Wanted { rerank: true, ..wanted(&[("title", 1.0)]) }.check().is_err());
    }

    #[test]
    fn a_weight_and_a_field_removed_change_at_once_and_say_so() {
        let settings = Settings::new(&wanted(&[("title", 2.0), ("body", 1.0)]));
        let changed = settings.change(&wanted(&[("title", 5.0)]), 10).unwrap();
        assert_eq!(changed.said, ["title weighs 5, was 2; at once", "body removed; at once from searches"]);
        assert_eq!(changed.settings.fields, [Field { path: "title".to_owned(), weight: 5.0, number: 0 }]);
    }

    #[test]
    fn a_field_added_takes_a_new_number_at_once_to_an_empty_index_and_asks_a_rebuild_of_one_that_holds_documents() {
        let settings = Settings::new(&wanted(&[("title", 1.0)]));
        let changed = settings.change(&wanted(&[("body", 1.0)]), 0).unwrap().settings;
        assert_eq!(changed.fields, [Field { path: "body".to_owned(), weight: 1.0, number: 1 }]);
        let rebuilt = settings.change(&wanted(&[("title", 2.0), ("body", 1.0)]), 3).unwrap();
        assert_eq!(rebuilt.rebuild, Some(Asked { why: "body added".to_owned(), needed: true }));
        assert_eq!(rebuilt.said, ["title weighs 2, was 1; at once"], "what needs no rebuild changes at once");
        assert!(
            Settings::new(&wanted(&[("title", 9.0), ("body", 1.0)]))
                .reads_as(&Settings::new(&wanted(&[("title", 1.0), ("body", 3.0)])))
        );
    }

    #[test]
    fn a_weight_is_above_zero_and_a_path_names_a_field() {
        assert!(wanted(&[("title", 0.0)]).check().is_err());
        assert!(wanted(&[("title", f64::NAN)]).check().is_err());
        assert!(wanted(&[("author..name", 1.0)]).check().is_err());
        assert!(wanted(&[("title", 1.0), ("title", 2.0)]).check().is_err());
        assert!(wanted(&[("author.name", 1.0)]).check().is_ok());
    }
}
