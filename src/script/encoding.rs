//! What a method's lexicon says its input and output are encoded as.
//!
//! `com.atproto.sync.getBlob` returns bytes because its lexicon declares
//! `"output": { "encoding": "*/*" }`, and `com.atproto.repo.uploadBlob` takes
//! them because it declares the same on its input. The declaration is the
//! contract both sides read, so it is what decides here too — never the shape
//! of whatever a script happened to return, or of whatever a caller happened
//! to send.

use serde_json::Value;

use crate::lexicon::ParsedLexicon;

/// A declared encoding, on either side of a method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Encoding {
    /// `application/json`, or no declaration at all.
    Json,
    /// A media type the lexicon pins, so neither side has to name one and
    /// neither can contradict it.
    Fixed(String),
    /// `*/*` — the concrete type is named by whoever supplies the bytes.
    Any,
}

impl Encoding {
    /// Whether this side of the method carries bytes rather than JSON.
    pub fn is_bytes(&self) -> bool {
        !matches!(self, Encoding::Json)
    }
}

fn read(declaration: Option<&Value>) -> Encoding {
    let declared = declaration
        .and_then(|side| side.get("encoding"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");

    match declared {
        "" | "application/json" => Encoding::Json,
        "*/*" => Encoding::Any,
        concrete => Encoding::Fixed(concrete.to_string()),
    }
}

/// What `defs.main.output.encoding` says.
pub fn of_output(lexicon: &ParsedLexicon) -> Encoding {
    read(lexicon.output.as_ref())
}

/// What `defs.main.input.encoding` says.
pub fn of_input(lexicon: &ParsedLexicon) -> Encoding {
    read(lexicon.input.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexicon::ProcedureAction;
    use serde_json::json;

    /// A lexicon declaring whichever encodings the caller names.
    fn lexicon(input: Option<&str>, output: Option<&str>) -> ParsedLexicon {
        let side = |e: Option<&str>| match e {
            Some(e) => json!({ "encoding": e }),
            None => json!({}),
        };
        ParsedLexicon::parse(
            json!({
                "lexicon": 1,
                "id": "at.example.thing",
                "defs": { "main": {
                    "type": "procedure",
                    "input": side(input),
                    "output": side(output),
                } }
            }),
            1,
            None,
            ProcedureAction::Create,
            None,
        )
        .expect("the fixture lexicon should parse")
    }

    #[test]
    fn a_declaration_is_read_as_json_bytes_or_delegated() {
        for (declared, expected) in [
            (None, Encoding::Json),
            (Some("application/json"), Encoding::Json),
            // Whitespace should not make a declaration into a media type of
            // its own.
            (Some("  application/json  "), Encoding::Json),
            (Some("*/*"), Encoding::Any),
            (
                Some("application/wasm"),
                Encoding::Fixed("application/wasm".into()),
            ),
        ] {
            let lex = lexicon(declared, declared);
            assert_eq!(of_input(&lex), expected, "input {declared:?}");
            assert_eq!(of_output(&lex), expected, "output {declared:?}");
        }
    }

    /// The two sides are read independently, which is the shape `uploadBlob`
    /// has: bytes in, JSON out.
    #[test]
    fn the_two_sides_are_independent() {
        let lex = lexicon(Some("*/*"), Some("application/json"));
        assert_eq!(of_input(&lex), Encoding::Any);
        assert_eq!(of_output(&lex), Encoding::Json);
        assert!(of_input(&lex).is_bytes());
        assert!(!of_output(&lex).is_bytes());
    }
}
