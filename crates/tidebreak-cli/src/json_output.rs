//! The JSON documents the CLI prints under `--output-format json` and `--json`.
//!
//! A command prints one object on one line, and every object carries
//! `"schema_version": 1`. The number names the shape. Within one schema
//! version, keys are only ever added, so a script reads the keys it needs and
//! ignores the rest; a change that removes, renames, or retypes a key raises
//! the number.
//!
//! The NDJSON streams (`-p --output-format json`, `code run`, `code watch`)
//! are not documents. They carry the internal event wire line by line and do
//! not pass through here.

use tidebreak_core::{AgentError, Result};

/// The shape version every JSON document the CLI prints carries.
pub(crate) const SCHEMA_VERSION: u32 = 1;

/// The key that carries [`SCHEMA_VERSION`].
pub(crate) const SCHEMA_VERSION_KEY: &str = "schema_version";

/// A value stamped as a CLI document.
///
/// An object gains the key. Anything else is wrapped as `value`, so every
/// document is an object a reader can look the version up on.
pub(crate) fn document<T: serde::Serialize + ?Sized>(value: &T) -> Result<serde_json::Value> {
    let value = serde_json::to_value(value)
        .map_err(|error| AgentError::msg(format!("could not encode json: {error}")))?;
    let mut object = match value {
        serde_json::Value::Object(object) => object,
        other => {
            let mut object = serde_json::Map::new();
            object.insert("value".to_owned(), other);
            object
        }
    };
    object.insert(SCHEMA_VERSION_KEY.to_owned(), SCHEMA_VERSION.into());
    Ok(serde_json::Value::Object(object))
}

/// Print one document on one line of stdout.
pub(crate) fn print_document<T: serde::Serialize + ?Sized>(value: &T) -> Result<()> {
    println!("{}", document(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_object_gains_the_schema_version_and_keeps_its_keys() {
        let stamped = document(&serde_json::json!({ "chats": [], "id": "a" })).unwrap();
        assert_eq!(
            stamped,
            serde_json::json!({ "chats": [], "id": "a", "schema_version": 1 })
        );
    }

    /// A route that ever answered a bare list would otherwise print a
    /// document with nowhere to put the version.
    #[test]
    fn anything_but_an_object_is_wrapped() {
        assert_eq!(
            document(&serde_json::json!([1, 2])).unwrap(),
            serde_json::json!({ "value": [1, 2], "schema_version": 1 })
        );
    }
}
