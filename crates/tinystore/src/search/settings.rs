//! An index's settings: its fields, each with its weight and a number of
//! its own for good, and whether a misspelt word is read as the nearest.
//! They are kept as JSON in the index's row, and an open that asks for
//! others changes them.
//!
//! ```text
//! {"fields":[{"path":"title","weight":2.0,"number":0}],"next_field":1,"typo_tolerance":true,"analysis":1}
//! ```

use serde_json::{Value, json};

use super::analyzer;
use crate::{Error, Result};

/// A field: where its text lies in a document, what a word found there
/// weighs, and its number, which names its parts and is never given again.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Field {
    pub(crate) path: String,
    pub(crate) weight: f64,
    pub(crate) number: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Settings {
    pub(crate) fields: Vec<Field>,
    pub(crate) next_field: u32,
    pub(crate) typo_tolerance: bool,
    pub(crate) analysis: u32,
}

/// What an open asks for: the fields by their paths and weights, none for a
/// document that is its own text.
#[derive(Clone, Debug)]
pub(crate) struct Wanted {
    pub(crate) fields: Vec<(String, f64)>,
    pub(crate) typo_tolerance: bool,
}

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
            if path.is_empty() || path.split('.').any(str::is_empty) {
                return Err(Error::invalid(format!(
                    "a field's path names a field, or fields joined by dots: {path:?}"
                )));
            }
            if !(weight.is_finite() && *weight > 0.0 && *weight <= MOST_WEIGHT) {
                return Err(Error::invalid(format!(
                    "field {path} weighs {weight}: a weight is above 0, at most 1,000"
                )));
            }
            if self.fields[..at].iter().any(|(earlier, _)| earlier == path) {
                return Err(Error::invalid(format!("field {path} is given twice")));
            }
        }
        Ok(())
    }
}

impl Settings {
    pub(crate) fn new(wanted: &Wanted) -> Settings {
        let fields: Vec<Field> =
            (0..).zip(wanted.fields()).map(|(number, (path, weight))| Field { path, weight, number }).collect();
        let next_field = fields.len() as u32;
        Settings { fields, next_field, typo_tolerance: wanted.typo_tolerance, analysis: analyzer::VERSION }
    }

    /// The settings `wanted` makes of these, and each change as the log says
    /// it. A weight, a field removed and typo tolerance change at once; a
    /// field added to an index that holds documents would need them read
    /// again, which no release does yet, so it is refused.
    pub(crate) fn change(&self, wanted: &Wanted, documents: u64) -> Result<(Settings, Vec<String>)> {
        let mut changed = Settings { fields: Vec::new(), ..self.clone() };
        let mut said = Vec::new();
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
        let settings = json!({
            "fields": fields,
            "next_field": self.next_field,
            "typo_tolerance": self.typo_tolerance,
            "analysis": self.analysis,
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
        Ok(Settings {
            fields,
            next_field: number(&settings["next_field"])?,
            typo_tolerance: settings["typo_tolerance"].as_bool().ok_or_else(broken)?,
            analysis: number(&settings["analysis"])?,
        })
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
            typo_tolerance: true,
        }
    }

    #[test]
    fn settings_read_back_as_written() {
        let settings = Settings::new(&wanted(&[("title", 2.0), ("body", 1.0)]));
        assert_eq!(Settings::from_json(&settings.to_json()).unwrap(), settings);
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
