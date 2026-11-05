//! What the instance's lexicons look like, for the two lexicon detail
//! settings: their shape as counts, and the documents themselves.

use std::collections::BTreeMap;

use serde_json::{Value, json};

/// The schema types atproto defines. Any other `type` value is counted as
/// `other`, so the summary only ever carries words from this list.
const LEXICON_TYPES: &[&str] = &[
    "array",
    "blob",
    "boolean",
    "bytes",
    "cid-link",
    "integer",
    "null",
    "object",
    "params",
    "permission",
    "permission-set",
    "procedure",
    "query",
    "record",
    "ref",
    "string",
    "subscription",
    "token",
    "union",
    "unknown",
];

/// A report holds lexicon documents up to this many bytes between them.
/// The collector refuses a body over 1 MiB, and a refused report is a lost
/// day, so the documents leave room for everything else.
pub const DOCUMENTS_BYTE_BUDGET: usize = 512 * 1024;

fn type_word(value: &Value) -> Option<&'static str> {
    let t = value.get("type")?.as_str()?;
    Some(
        LEXICON_TYPES
            .iter()
            .find(|known| **known == t)
            .copied()
            .unwrap_or("other"),
    )
}

/// A lexicon's shape as counts: its definitions by type, the main
/// definition's type, how often each schema type occurs, how many fields its
/// objects declare between them, and how deeply its schemas nest. Nothing
/// that names anything: no NSID, no definition or field name, no
/// description, no `ref` target and no enum or known value.
pub fn structure(doc: &Value) -> Value {
    let mut def_types = BTreeMap::<&str, u64>::new();
    let mut walk = Walk::default();
    let defs = doc.get("defs").and_then(Value::as_object);
    for def in defs.into_iter().flat_map(|defs| defs.values()) {
        if let Some(t) = type_word(def) {
            *def_types.entry(t).or_default() += 1;
        }
        walk.visit(def, 0);
    }
    json!({
        "defs": defs.map_or(0, |defs| defs.len()),
        "def_types": def_types,
        "main_type": doc.pointer("/defs/main").and_then(type_word),
        "types": walk.types,
        "fields": walk.fields,
        "max_depth": walk.max_depth,
    })
}

#[derive(Default)]
struct Walk {
    types: BTreeMap<&'static str, u64>,
    fields: u64,
    max_depth: u32,
}

impl Walk {
    fn visit(&mut self, value: &Value, depth: u32) {
        match value {
            Value::Object(map) => {
                let depth = match type_word(value) {
                    Some(t) => {
                        *self.types.entry(t).or_default() += 1;
                        self.max_depth = self.max_depth.max(depth + 1);
                        depth + 1
                    }
                    None => depth,
                };
                if let Some(properties) = map.get("properties").and_then(Value::as_object) {
                    self.fields += properties.len() as u64;
                }
                for child in map.values() {
                    self.visit(child, depth);
                }
            }
            Value::Array(items) => {
                for item in items {
                    self.visit(item, depth);
                }
            }
            _ => {}
        }
    }
}

/// The shapes of `docs`, sorted by their content rather than kept in the
/// catalog's order. The catalog lists by NSID, so its order would let the
/// shapes be matched to the names in a report that also carries those.
pub fn structures<'a>(docs: impl IntoIterator<Item = &'a Value>) -> Vec<Value> {
    let mut shapes: Vec<(String, Value)> = docs
        .into_iter()
        .map(|doc| {
            let shape = structure(doc);
            (shape.to_string(), shape)
        })
        .collect();
    shapes.sort_by(|a, b| a.0.cmp(&b.0));
    shapes.into_iter().map(|(_, shape)| shape).collect()
}

/// `docs` in order until the next would pass `budget` bytes, and how many
/// were left out.
pub fn documents<'a>(
    docs: impl IntoIterator<Item = &'a Value>,
    budget: usize,
) -> (Vec<Value>, u32) {
    let mut kept = Vec::new();
    let mut used = 0;
    let mut omitted = 0_u32;
    for doc in docs {
        let size = serde_json::to_vec(doc).map_or(usize::MAX, |bytes| bytes.len());
        if omitted == 0 && used + size <= budget {
            used += size;
            kept.push(doc.clone());
        } else {
            omitted += 1;
        }
    }
    (kept, omitted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post() -> Value {
        json!({
            "lexicon": 1,
            "id": "com.secretapp.post",
            "description": "Posts in Secret App",
            "defs": {
                "main": {
                    "type": "record",
                    "key": "tid",
                    "record": {
                        "type": "object",
                        "required": ["text"],
                        "properties": {
                            "text": { "type": "string", "maxLength": 300, "description": "The body" },
                            "mood": { "type": "string", "knownValues": ["com.secretapp.defs#happy"] },
                            "reply": { "type": "ref", "ref": "com.secretapp.defs#replyRef" },
                            "tags": { "type": "array", "items": { "type": "string" } }
                        }
                    }
                },
                "draft": { "type": "token", "description": "A draft" }
            }
        })
    }

    #[test]
    fn a_structure_counts_the_shape() {
        assert_eq!(
            structure(&post()),
            json!({
                "defs": 2,
                "def_types": { "record": 1, "token": 1 },
                "main_type": "record",
                "types": { "array": 1, "object": 1, "record": 1, "ref": 1, "string": 3, "token": 1 },
                "fields": 4,
                "max_depth": 4,
            })
        );
    }

    #[test]
    fn a_structure_names_nothing() {
        let shape = structure(&post()).to_string();
        for word in [
            "secretapp",
            "post",
            "Posts",
            "text",
            "mood",
            "reply",
            "tags",
            "draft",
            "happy",
            "replyRef",
            "body",
            "main\"",
            "tid",
        ] {
            assert!(
                !shape.contains(word),
                "the structure carried {word:?}: {shape}"
            );
        }
    }

    #[test]
    fn a_type_outside_the_vocabulary_is_counted_as_other() {
        let shape = structure(&json!({
            "defs": { "main": { "type": "com.secretapp.custom" } }
        }));
        assert_eq!(shape["def_types"], json!({ "other": 1 }));
        assert_eq!(shape["main_type"], "other");
        assert!(!shape.to_string().contains("secretapp"));
    }

    #[test]
    fn structures_do_not_keep_the_catalog_order() {
        let small = json!({ "defs": { "main": { "type": "token" } } });
        let big = post();
        assert_eq!(structures([&small, &big]), structures([&big, &small]),);
    }

    #[test]
    fn documents_stop_at_the_budget_and_count_the_rest() {
        let a = json!({ "id": "a" });
        let b = json!({ "id": "b", "pad": "x".repeat(100) });
        let c = json!({ "id": "c" });
        let budget = serde_json::to_vec(&a).unwrap().len() + 10;

        let (kept, omitted) = documents([&a, &b, &c], budget);

        assert_eq!(kept, vec![a.clone()]);
        assert_eq!(
            omitted, 2,
            "a document after the first that doesn't fit is left out too"
        );
        assert_eq!(documents([&a, &c], usize::MAX), (vec![a, c], 0));
    }
}
