//! An index's settings: its fields, each with its weight and a number of
//! its own for good, its filters, numbered the same way, whether a misspelt
//! word is read as the nearest, its vectors' dimensions, and how a search
//! by words and a vector at once combines them. They are kept as JSON in
//! the index's row, and an open that asks for others changes them.
//!
//! ```text
//! {"fields":[{"path":"title","weight":2.0,"number":0}],"next_field":1,"typo_tolerance":true,"analysis":1,
//!  "filters":[{"path":"category","number":0}],"next_filter":1,
//!  "vectors":{"dimensions":1024},"hybrid":{"vector_weight":0.5,"rerank":false}}
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
}

/// The vectors an index keeps: of how many dimensions each.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Vectors {
    pub(crate) dimensions: usize,
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
    pub(crate) vector_weight: Option<f64>,
    pub(crate) rerank: bool,
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
        self.check_vectors()
    }

    fn check_vectors(&self) -> Result<()> {
        match self.dimensions {
            Some(0) => return Err(Error::invalid("vectors of 0 dimensions")),
            Some(dimensions) if dimensions > MOST_DIMENSIONS => {
                return Err(Error::limit(format!("vectors of {dimensions} dimensions, past {MOST_DIMENSIONS}")));
            }
            None if self.vector_weight.is_some() || self.rerank => {
                return Err(Error::invalid("a vector weight or a rerank for an index without vectors"));
            }
            _ => {}
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
            vectors: wanted.dimensions.map(|dimensions| Vectors { dimensions }),
            hybrid: Hybrid::of(wanted),
        }
    }

    /// The settings `wanted` makes of these, and each change as the log says
    /// it. A weight, a field removed, typo tolerance, vectors added and how
    /// a search by both combines them change at once; a field added to an
    /// index that holds documents would need them read again, which no
    /// release does yet, so it is refused, as are other vectors than those
    /// kept, which the store has nothing to make from.
    pub(crate) fn change(&self, wanted: &Wanted, documents: u64) -> Result<(Settings, Vec<String>)> {
        let mut changed = Settings { fields: Vec::new(), ..self.clone() };
        let mut said = Vec::new();
        changed.change_filters(wanted, documents, &mut said)?;
        changed.change_vectors(wanted, documents, &mut said)?;
        for (path, weight) in wanted.fields() {
            let Some(field) = self.fields.iter().find(|field| field.path == path) else {
                if documents > 0 {
                    return Err(Error::invalid(format!(
                        "field {} added to an index of {documents} documents: adding a field to an index that \
                         holds documents is not built yet",
                        shown(&path)
                    )));
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
            }
        }
        if wanted.typo_tolerance != self.typo_tolerance {
            let now = if wanted.typo_tolerance { "on" } else { "off" };
            said.push(format!("typo tolerance {now}; at once"));
            changed.typo_tolerance = wanted.typo_tolerance;
        }
        Ok((changed, said))
    }

    /// Keeps the filters `wanted` names, each with its number; one removed
    /// leaves searches at once, and one added waits for an index that holds
    /// no documents, as a field does.
    fn change_filters(&mut self, wanted: &Wanted, documents: u64, said: &mut Vec<String>) -> Result<()> {
        let mut filters = Vec::with_capacity(wanted.filters.len());
        for path in &wanted.filters {
            if let Some(kept) = self.filters.iter().find(|filter| &filter.path == path) {
                filters.push(kept.clone());
                continue;
            }
            if documents > 0 {
                return Err(Error::invalid(format!(
                    "filter {path} added to an index of {documents} documents: adding a filter to an index that \
                     holds documents is not built yet"
                )));
            }
            said.push(format!("filter {path} added"));
            filters.push(Filter { path: path.clone(), number: self.next_filter });
            self.next_filter += 1;
        }
        for filter in &self.filters {
            if !filters.iter().any(|kept| kept.number == filter.number) {
                said.push(format!("filter {} removed; at once from searches", filter.path));
            }
        }
        self.filters = filters;
        Ok(())
    }

    /// The filter of `path`, while the index has it.
    pub(crate) fn filter(&self, path: &str) -> Option<&Filter> {
        self.filters.iter().find(|filter| filter.path == path)
    }

    fn change_vectors(&mut self, wanted: &Wanted, documents: u64, said: &mut Vec<String>) -> Result<()> {
        match (self.vectors, wanted.dimensions) {
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
                self.vectors = Some(Vectors { dimensions });
            }
            _ => {}
        }
        let hybrid = Hybrid::of(wanted);
        if hybrid.vector_weight != self.hybrid.vector_weight {
            said.push(format!("a vector weighs {}, was {}; at once", hybrid.vector_weight, self.hybrid.vector_weight));
        }
        if hybrid.rerank != self.hybrid.rerank {
            said.push(format!("rerank {}; at once", if hybrid.rerank { "on" } else { "off" }));
        }
        self.hybrid = hybrid;
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
        let vectors = self.vectors.map(|vectors| json!({ "dimensions": vectors.dimensions }));
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
            vectors => Some(Vectors { dimensions: number(&vectors["dimensions"])? as usize }),
        };
        let hybrid = &settings["hybrid"];
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
            vector_weight: None,
            rerank: false,
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
        let (changed, said) = settings.change(&with(&["owner"]), 5).unwrap();
        assert_eq!(said, ["filter category removed; at once from searches"]);
        assert_eq!(changed.filters, [Filter { path: "owner".to_owned(), number: 1 }]);
        assert!(changed.change(&with(&["owner", "category"]), 5).is_err());
        let (added, _) = changed.change(&with(&["owner", "category"]), 0).unwrap();
        assert_eq!(added.filter("category").unwrap().number, 2, "a filter added again takes a new number");
        assert!(with(&["a", "a"]).check().is_err());
        assert!(with(&["a..b"]).check().is_err());
    }

    #[test]
    fn vectors_added_change_at_once_and_other_dimensions_or_none_are_refused() {
        let settings = Settings::new(&wanted(&[("title", 1.0)]));
        let (changed, said) = settings.change(&with_vectors(3, None, false), 2).unwrap();
        assert_eq!(said, ["vectors of 3 dimensions added; the 2 documents held have none"]);
        assert_eq!(changed.vectors, Some(Vectors { dimensions: 3 }));
        assert_eq!(changed.change(&with_vectors(4, None, false), 2).unwrap_err().kind(), crate::ErrorKind::Invalid);
        assert_eq!(changed.change(&wanted(&[("title", 1.0)]), 2).unwrap_err().kind(), crate::ErrorKind::Invalid);
        let (_, said) = changed.change(&with_vectors(3, Some(0.7), false), 2).unwrap();
        assert_eq!(said, ["a vector weighs 0.7, was 0.5; at once"]);
        let (_, said) = changed.change(&with_vectors(3, None, true), 2).unwrap();
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
        let (changed, said) = settings.change(&wanted(&[("title", 5.0)]), 10).unwrap();
        assert_eq!(said, ["title weighs 5, was 2; at once", "body removed; at once from searches"]);
        assert_eq!(changed.fields, [Field { path: "title".to_owned(), weight: 5.0, number: 0 }]);
    }

    #[test]
    fn a_field_added_takes_a_new_number_and_waits_for_an_empty_index() {
        let settings = Settings::new(&wanted(&[("title", 1.0)]));
        let (changed, _) = settings.change(&wanted(&[("body", 1.0)]), 0).unwrap();
        assert_eq!(changed.fields, [Field { path: "body".to_owned(), weight: 1.0, number: 1 }]);
        let refused = settings.change(&wanted(&[("title", 1.0), ("body", 1.0)]), 3).unwrap_err();
        assert_eq!(refused.kind(), crate::ErrorKind::Invalid);
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
