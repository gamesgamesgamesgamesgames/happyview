use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The type of a lexicon's `main` definition.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LexiconType {
    Record,
    Query,
    Procedure,
    /// A space type declaration: defines the shape and allowed collections for a space type.
    Space,
    /// Lexicons with no `main` def or a non-endpoint main type (token, object, string, etc.).
    Definitions,
}

/// The action a procedure lexicon performs on its target collection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcedureAction {
    Create,
    Update,
    Delete,
    /// Backwards-compatible default: sniff for `uri` in input to decide create vs put.
    Upsert,
}

impl ProcedureAction {
    /// Parse an optional action string into a `ProcedureAction`.
    /// Returns `Upsert` for `None`, or an error for unrecognized values.
    pub fn from_optional_str(s: Option<&str>) -> Result<Self, String> {
        match s {
            None => Ok(Self::Upsert),
            Some("create") => Ok(Self::Create),
            Some("update") => Ok(Self::Update),
            Some("delete") => Ok(Self::Delete),
            Some("upsert") => Ok(Self::Upsert),
            Some(other) => Err(format!(
                "invalid action '{other}': must be create, update, delete, or upsert"
            )),
        }
    }

    /// Convert to an optional string for database storage.
    /// `Upsert` maps to `None` (the default).
    pub fn to_optional_str(&self) -> Option<&'static str> {
        match self {
            Self::Create => Some("create"),
            Self::Update => Some("update"),
            Self::Delete => Some("delete"),
            Self::Upsert => None,
        }
    }
}

/// Metadata extracted from a raw lexicon JSON document.
#[derive(Debug, Clone)]
pub struct ParsedLexicon {
    /// The NSID, e.g. "games.gamesgamesgamesgames.game".
    pub id: String,
    /// What kind of endpoint this lexicon defines.
    pub lexicon_type: LexiconType,
    /// For records: the `key` field (e.g. "tid", "any", "literal:self").
    pub record_key: Option<String>,
    /// For queries: the parameters schema from `defs.main.parameters`.
    pub parameters: Option<Value>,
    /// For procedures: the input schema from `defs.main.input`.
    pub input: Option<Value>,
    /// For queries and procedures: the output schema from `defs.main.output`.
    pub output: Option<Value>,
    /// For records: the record schema from `defs.main.record`.
    pub record_schema: Option<Value>,
    /// The entire raw lexicon JSON.
    pub raw: Value,
    /// Database revision number.
    pub revision: i32,
    /// For queries/procedures: the backing record collection NSID.
    pub target_collection: Option<String>,
    /// For procedures: the action this procedure performs (create, update, delete, upsert).
    pub action: ProcedureAction,
    /// Optional per-NSID token cost for rate limiting.
    pub token_cost: Option<u32>,
    /// Optional space type NSID indicating this lexicon is designed for use within spaces of that type.
    pub space_type: Option<String>,
    /// For space declarations: the human-readable name (1-64 chars).
    pub space_name: Option<String>,
    /// For space declarations: the allowed collection NSIDs.
    pub space_collections: Option<Vec<String>>,
}

impl ParsedLexicon {
    /// Parse a lexicon JSON document into a `ParsedLexicon`.
    pub fn parse(
        raw: Value,
        revision: i32,
        target_collection: Option<String>,
        action: ProcedureAction,
        token_cost: Option<u32>,
    ) -> Result<Self, String> {
        let id = raw
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or("lexicon JSON missing 'id' field")?
            .to_string();

        let main_def = raw.get("defs").and_then(|d| d.get("main"));

        let main_type_str = main_def
            .and_then(|m| m.get("type"))
            .and_then(|t| t.as_str());

        let lexicon_type = match main_type_str {
            Some("record") => LexiconType::Record,
            Some("query") => LexiconType::Query,
            Some("procedure") => LexiconType::Procedure,
            Some("space") => LexiconType::Space,
            _ => LexiconType::Definitions,
        };

        let record_key = main_def
            .and_then(|m| m.get("key"))
            .and_then(|k| k.as_str())
            .map(|s| s.to_string());

        let parameters = main_def.and_then(|m| m.get("parameters")).cloned();
        let input = main_def.and_then(|m| m.get("input")).cloned();
        let output = main_def.and_then(|m| m.get("output")).cloned();
        let record_schema = main_def.and_then(|m| m.get("record")).cloned();

        let space_type = raw
            .get("spaceType")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let (space_name, space_collections) = if lexicon_type == LexiconType::Space {
            let main = main_def.ok_or("space lexicon missing 'defs.main'")?;

            main.get("key")
                .and_then(|v| v.as_str())
                .ok_or("space lexicon 'defs.main.key' must be a string")?;

            let name = main
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or("space lexicon 'defs.main.name' must be a string")?;
            let name_len = name.chars().count();
            if name_len == 0 || name_len > 64 {
                return Err("space lexicon 'defs.main.name' must be 1-64 characters".into());
            }

            let collections_val = main
                .get("collections")
                .and_then(|v| v.as_array())
                .ok_or("space lexicon 'defs.main.collections' must be an array")?;
            let collections: Vec<String> = collections_val
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    v.as_str()
                        .map(|s| s.to_string())
                        .ok_or_else(|| format!("space lexicon 'collections[{i}]' must be a string"))
                })
                .collect::<Result<_, _>>()?;

            (Some(name.to_string()), Some(collections))
        } else {
            (None, None)
        };

        Ok(Self {
            id,
            lexicon_type,
            record_key,
            parameters,
            input,
            output,
            record_schema,
            raw,
            revision,
            target_collection,
            action,
            token_cost,
            space_type,
            space_name,
            space_collections,
        })
    }
}

/// The structural checks every lexicon passes before it is stored or served:
/// lexicon version 1 and a string `id` that is a valid NSID. Returns the id.
/// The admin upload and the project build both call this, so a file cannot
/// carry a lexicon the upload would refuse.
pub fn check_document(raw: &Value) -> Result<String, String> {
    let version = raw
        .get("lexicon")
        .and_then(|v| v.as_i64())
        .ok_or("lexicon JSON must have a numeric 'lexicon' field")?;
    if version != 1 {
        return Err(format!("unsupported lexicon version: {version}"));
    }
    let id = raw
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or("lexicon JSON must have a string 'id' field")?;
    happyview_nsid::validate_nsid(id).map_err(|e| e.to_string())?;
    Ok(id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -----------------------------------------------------------------------
    // ParsedLexicon::parse
    // -----------------------------------------------------------------------

    fn record_lexicon_json() -> Value {
        json!({
            "lexicon": 1,
            "id": "games.gamesgamesgamesgames.game",
            "defs": {
                "main": {
                    "type": "record",
                    "key": "tid",
                    "record": {
                        "type": "object",
                        "properties": {
                            "title": { "type": "string" }
                        }
                    }
                }
            }
        })
    }

    #[test]
    fn check_document_returns_the_id_of_a_valid_lexicon() {
        assert_eq!(
            check_document(&record_lexicon_json()).unwrap(),
            "games.gamesgamesgamesgames.game"
        );
    }

    #[test]
    fn check_document_says_what_is_wrong() {
        let cases = [
            (json!({"id": "com.example.x"}), "numeric 'lexicon'"),
            (
                json!({"lexicon": 2, "id": "com.example.x"}),
                "unsupported lexicon version: 2",
            ),
            (json!({"lexicon": 1}), "string 'id'"),
            (json!({"lexicon": 1, "id": "not an nsid"}), "invalid NSID"),
        ];
        for (raw, expected) in cases {
            let err = check_document(&raw).unwrap_err();
            assert!(err.contains(expected), "{raw}: {err}");
        }
    }

    fn query_lexicon_json() -> Value {
        json!({
            "lexicon": 1,
            "id": "games.gamesgamesgamesgames.listGames",
            "defs": {
                "main": {
                    "type": "query",
                    "parameters": {
                        "type": "params",
                        "properties": {
                            "limit": { "type": "integer" }
                        }
                    },
                    "output": {
                        "encoding": "application/json"
                    }
                }
            }
        })
    }

    fn procedure_lexicon_json() -> Value {
        json!({
            "lexicon": 1,
            "id": "games.gamesgamesgamesgames.createGame",
            "defs": {
                "main": {
                    "type": "procedure",
                    "input": {
                        "encoding": "application/json"
                    },
                    "output": {
                        "encoding": "application/json"
                    }
                }
            }
        })
    }

    fn definitions_lexicon_json() -> Value {
        json!({
            "lexicon": 1,
            "id": "games.gamesgamesgamesgames.defs",
            "defs": {
                "genre": {
                    "type": "string",
                    "knownValues": ["action", "rpg"]
                }
            }
        })
    }

    #[test]
    fn parse_record_lexicon() {
        let parsed = ParsedLexicon::parse(
            record_lexicon_json(),
            1,
            None,
            ProcedureAction::Upsert,
            None,
        )
        .unwrap();
        assert_eq!(parsed.id, "games.gamesgamesgamesgames.game");
        assert_eq!(parsed.lexicon_type, LexiconType::Record);
        assert_eq!(parsed.record_key, Some("tid".into()));
        assert!(parsed.record_schema.is_some());
        assert!(parsed.parameters.is_none());
        assert!(parsed.input.is_none());
    }

    #[test]
    fn parse_query_lexicon() {
        let parsed = ParsedLexicon::parse(
            query_lexicon_json(),
            2,
            Some("games.gamesgamesgamesgames.game".into()),
            ProcedureAction::Upsert,
            None,
        )
        .unwrap();
        assert_eq!(parsed.lexicon_type, LexiconType::Query);
        assert!(parsed.parameters.is_some());
        assert!(parsed.output.is_some());
        assert_eq!(
            parsed.target_collection,
            Some("games.gamesgamesgamesgames.game".into())
        );
        assert_eq!(parsed.revision, 2);
    }

    #[test]
    fn parse_procedure_lexicon() {
        let parsed = ParsedLexicon::parse(
            procedure_lexicon_json(),
            1,
            None,
            ProcedureAction::Upsert,
            None,
        )
        .unwrap();
        assert_eq!(parsed.lexicon_type, LexiconType::Procedure);
        assert!(parsed.input.is_some());
        assert!(parsed.output.is_some());
    }

    #[test]
    fn parse_procedure_with_action() {
        let parsed = ParsedLexicon::parse(
            procedure_lexicon_json(),
            1,
            None,
            ProcedureAction::Delete,
            None,
        )
        .unwrap();
        assert_eq!(parsed.action, ProcedureAction::Delete);
    }

    #[test]
    fn parse_definitions_lexicon() {
        let parsed = ParsedLexicon::parse(
            definitions_lexicon_json(),
            1,
            None,
            ProcedureAction::Upsert,
            None,
        )
        .unwrap();
        assert_eq!(parsed.lexicon_type, LexiconType::Definitions);
    }

    #[test]
    fn parse_missing_id_returns_error() {
        let raw = json!({"lexicon": 1, "defs": {}});
        let result = ParsedLexicon::parse(raw, 1, None, ProcedureAction::Upsert, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("id"));
    }

    #[test]
    fn parse_preserves_raw_json() {
        let raw = record_lexicon_json();
        let parsed =
            ParsedLexicon::parse(raw.clone(), 1, None, ProcedureAction::Upsert, None).unwrap();
        assert_eq!(parsed.raw, raw);
    }

    #[test]
    fn parse_target_collection_passthrough() {
        let parsed = ParsedLexicon::parse(
            query_lexicon_json(),
            1,
            Some("custom.collection".into()),
            ProcedureAction::Upsert,
            None,
        )
        .unwrap();
        assert_eq!(parsed.target_collection, Some("custom.collection".into()));
    }

    // -----------------------------------------------------------------------
    // ProcedureAction
    // -----------------------------------------------------------------------

    #[test]
    fn procedure_action_from_none_is_upsert() {
        assert_eq!(
            ProcedureAction::from_optional_str(None).unwrap(),
            ProcedureAction::Upsert
        );
    }

    #[test]
    fn procedure_action_from_known_values() {
        assert_eq!(
            ProcedureAction::from_optional_str(Some("create")).unwrap(),
            ProcedureAction::Create
        );
        assert_eq!(
            ProcedureAction::from_optional_str(Some("update")).unwrap(),
            ProcedureAction::Update
        );
        assert_eq!(
            ProcedureAction::from_optional_str(Some("delete")).unwrap(),
            ProcedureAction::Delete
        );
        assert_eq!(
            ProcedureAction::from_optional_str(Some("upsert")).unwrap(),
            ProcedureAction::Upsert
        );
    }

    #[test]
    fn procedure_action_from_invalid_returns_error() {
        let result = ProcedureAction::from_optional_str(Some("invalid"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid"));
    }

    #[test]
    fn procedure_action_to_optional_str_roundtrip() {
        assert_eq!(ProcedureAction::Create.to_optional_str(), Some("create"));
        assert_eq!(ProcedureAction::Update.to_optional_str(), Some("update"));
        assert_eq!(ProcedureAction::Delete.to_optional_str(), Some("delete"));
        assert_eq!(ProcedureAction::Upsert.to_optional_str(), None);
    }

    fn space_declaration_lexicon_json() -> Value {
        json!({
            "lexicon": 1,
            "id": "com.example.forum",
            "defs": {
                "main": {
                    "type": "space",
                    "key": "slug",
                    "name": "Forum",
                    "collections": [
                        "com.example.forum.post",
                        "com.example.forum.comment"
                    ]
                }
            }
        })
    }

    #[test]
    fn parse_space_declaration_lexicon() {
        let parsed = ParsedLexicon::parse(
            space_declaration_lexicon_json(),
            1,
            None,
            ProcedureAction::Upsert,
            None,
        )
        .unwrap();
        assert_eq!(parsed.lexicon_type, LexiconType::Space);
        assert_eq!(parsed.space_name.as_deref(), Some("Forum"));
        assert_eq!(
            parsed.space_collections,
            Some(vec![
                "com.example.forum.post".to_string(),
                "com.example.forum.comment".to_string()
            ])
        );
    }

    #[test]
    fn parse_space_declaration_missing_key_returns_error() {
        let raw = json!({
            "lexicon": 1,
            "id": "com.example.forum",
            "defs": {
                "main": {
                    "type": "space",
                    "name": "Forum",
                    "collections": []
                }
            }
        });
        let result = ParsedLexicon::parse(raw, 1, None, ProcedureAction::Upsert, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("key"));
    }

    #[test]
    fn parse_space_declaration_missing_name_returns_error() {
        let raw = json!({
            "lexicon": 1,
            "id": "com.example.forum",
            "defs": {
                "main": {
                    "type": "space",
                    "key": "slug",
                    "collections": []
                }
            }
        });
        let result = ParsedLexicon::parse(raw, 1, None, ProcedureAction::Upsert, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("name"));
    }

    #[test]
    fn parse_space_declaration_name_too_long_returns_error() {
        let raw = json!({
            "lexicon": 1,
            "id": "com.example.forum",
            "defs": {
                "main": {
                    "type": "space",
                    "key": "slug",
                    "name": "a".repeat(65),
                    "collections": []
                }
            }
        });
        let result = ParsedLexicon::parse(raw, 1, None, ProcedureAction::Upsert, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("1-64"));
    }

    #[test]
    fn parse_space_declaration_missing_collections_returns_error() {
        let raw = json!({
            "lexicon": 1,
            "id": "com.example.forum",
            "defs": {
                "main": {
                    "type": "space",
                    "key": "slug",
                    "name": "Forum"
                }
            }
        });
        let result = ParsedLexicon::parse(raw, 1, None, ProcedureAction::Upsert, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("collections"));
    }

    #[test]
    fn parse_space_type_from_lexicon() {
        let raw = json!({
            "lexicon": 1,
            "id": "com.example.forum.post",
            "spaceType": "com.example.forum",
            "defs": {
                "main": {
                    "type": "record",
                    "key": "tid",
                    "record": {
                        "type": "object",
                        "properties": {
                            "text": { "type": "string" }
                        }
                    }
                }
            }
        });
        let parsed = ParsedLexicon::parse(raw, 1, None, ProcedureAction::Upsert, None).unwrap();
        assert_eq!(parsed.space_type.as_deref(), Some("com.example.forum"));
    }

    #[test]
    fn parse_space_type_none_by_default() {
        let parsed = ParsedLexicon::parse(
            record_lexicon_json(),
            1,
            None,
            ProcedureAction::Upsert,
            None,
        )
        .unwrap();
        assert!(parsed.space_type.is_none());
    }
}
