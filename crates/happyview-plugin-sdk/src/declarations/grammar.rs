//! The type strings a surface writes: `string`, `integer?`, `Record[]`,
//! `'asc' | 'desc'`.
//!
//! ```text
//! type     := union
//! union    := postfix ( '|' postfix )*
//! postfix  := primary ( '[]' | '?' )*
//! primary  := builtin | literal | ref | '(' type ')'
//! ```

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use serde_json::Value;

use super::TypeExpr;

/// Why a type string did not parse. `column` counts characters from 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeError {
    pub message: String,
    pub column: Option<usize>,
}

impl fmt::Display for TypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.column {
            Some(column) => write!(f, "{} at column {column}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

pub fn parse_type(text: &str) -> Result<TypeExpr, TypeError> {
    let mut parser = Parser {
        chars: text.chars().collect(),
        at: 0,
    };
    parser.skip_space();
    if parser.at_end() {
        return Err(TypeError {
            message: "a type cannot be empty".into(),
            column: None,
        });
    }
    let ty = parser.union()?;
    parser.skip_space();
    if !parser.at_end() {
        return Err(parser.error(format!("unexpected '{}'", parser.chars[parser.at])));
    }
    Ok(ty)
}

/// The type in the grammar's own form. What the grammar cannot spell (an
/// object's properties, a string's format) prints as its nearest word.
pub fn print_type(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::String { .. } => "string".into(),
        TypeExpr::Integer => "integer".into(),
        TypeExpr::Number => "number".into(),
        TypeExpr::Boolean => "boolean".into(),
        TypeExpr::Bytes => "bytes".into(),
        TypeExpr::Null => "null".into(),
        TypeExpr::Unknown => "unknown".into(),
        TypeExpr::Any => "any".into(),
        TypeExpr::Blob => "blob".into(),
        TypeExpr::CidLink => "cid-link".into(),
        TypeExpr::Literal { value } => print_literal(value),
        TypeExpr::Array { items } if **items == TypeExpr::Unknown => "array".into(),
        TypeExpr::Array { items } => format!("{}[]", print_postfix(items)),
        TypeExpr::Map { .. } | TypeExpr::Object(_) => "object".into(),
        TypeExpr::Union { members, .. } => {
            let others: Vec<&TypeExpr> = members.iter().filter(|m| **m != TypeExpr::Null).collect();
            let nullable = others.len() < members.len();
            match (others.as_slice(), nullable) {
                ([only], true) => format!("{}?", print_postfix(only)),
                _ => {
                    let mut parts: Vec<String> = others.iter().map(|m| print_type(m)).collect();
                    if nullable {
                        parts.push("null".into());
                    }
                    parts.join(" | ")
                }
            }
        }
        TypeExpr::Ref { name } | TypeExpr::Var { name } => name.clone(),
    }
}

/// A member of a postfix expression: a union that prints with `|` needs
/// parentheses there.
fn print_postfix(ty: &TypeExpr) -> String {
    let printed = print_type(ty);
    if matches!(ty, TypeExpr::Union { .. }) && printed.contains(" | ") {
        format!("({printed})")
    } else {
        printed
    }
}

fn print_literal(value: &Value) -> String {
    match value {
        Value::String(s) => {
            let escaped = s.replace('\\', "\\\\").replace('\'', "\\'");
            format!("'{escaped}'")
        }
        other => other.to_string(),
    }
}

struct Parser {
    chars: Vec<char>,
    at: usize,
}

impl Parser {
    fn at_end(&self) -> bool {
        self.at >= self.chars.len()
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn skip_space(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.at += 1;
        }
    }

    fn error(&self, message: impl Into<String>) -> TypeError {
        TypeError {
            message: message.into(),
            column: Some(self.at + 1),
        }
    }

    fn union(&mut self) -> Result<TypeExpr, TypeError> {
        let first = self.postfix()?;
        let mut members = Vec::new();
        push_member(&mut members, first);
        loop {
            self.skip_space();
            if self.peek() != Some('|') {
                break;
            }
            self.at += 1;
            self.skip_space();
            let next = self.postfix()?;
            push_member(&mut members, next);
        }
        Ok(if members.len() == 1 {
            members.pop().unwrap_or(TypeExpr::Unknown)
        } else {
            TypeExpr::Union {
                members,
                open: false,
            }
        })
    }

    fn postfix(&mut self) -> Result<TypeExpr, TypeError> {
        let mut ty = self.primary()?;
        loop {
            match self.peek() {
                Some('[') => {
                    self.at += 1;
                    if self.peek() != Some(']') {
                        return Err(self.error("expected ']' after '['"));
                    }
                    self.at += 1;
                    ty = TypeExpr::Array {
                        items: Box::new(ty),
                    };
                }
                Some('?') => {
                    self.at += 1;
                    ty = ty.nullable();
                }
                _ => return Ok(ty),
            }
        }
    }

    fn primary(&mut self) -> Result<TypeExpr, TypeError> {
        match self.peek() {
            None => Err(self.error("expected a type")),
            Some('(') => {
                self.at += 1;
                self.skip_space();
                let inner = self.union()?;
                self.skip_space();
                if self.peek() != Some(')') {
                    return Err(self.error("expected ')'"));
                }
                self.at += 1;
                Ok(inner)
            }
            Some(quote @ ('\'' | '"')) => self.string_literal(quote),
            Some(c) if c == '-' || c.is_ascii_digit() => self.number_literal(),
            Some(c) if c.is_ascii_alphabetic() || c == '_' => Ok(self.word()),
            Some(c) => Err(self.error(format!("unexpected '{c}'"))),
        }
    }

    fn string_literal(&mut self, quote: char) -> Result<TypeExpr, TypeError> {
        let start = self.at;
        self.at += 1;
        let mut value = String::new();
        loop {
            match self.peek() {
                None => {
                    return Err(TypeError {
                        message: "unclosed string literal".into(),
                        column: Some(start + 1),
                    })
                }
                Some('\\') => {
                    self.at += 1;
                    match self.peek() {
                        Some(c) => value.push(c),
                        None => continue,
                    }
                    self.at += 1;
                }
                Some(c) if c == quote => {
                    self.at += 1;
                    return Ok(TypeExpr::Literal {
                        value: Value::String(value),
                    });
                }
                Some(c) => {
                    value.push(c);
                    self.at += 1;
                }
            }
        }
    }

    fn number_literal(&mut self) -> Result<TypeExpr, TypeError> {
        let start = self.at;
        if self.peek() == Some('-') {
            self.at += 1;
        }
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.at += 1;
        }
        let text: String = self.chars[start..self.at].iter().collect();
        text.parse::<i64>()
            .map(|n| TypeExpr::Literal {
                value: Value::from(n),
            })
            .map_err(|_| TypeError {
                message: format!("'{text}' is not an integer"),
                column: Some(start + 1),
            })
    }

    fn word(&mut self) -> TypeExpr {
        let start = self.at;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '#'))
        {
            self.at += 1;
        }
        let word: String = self.chars[start..self.at].iter().collect();
        match word.as_str() {
            "string" => TypeExpr::string(),
            "integer" => TypeExpr::Integer,
            "number" => TypeExpr::Number,
            "boolean" => TypeExpr::Boolean,
            "bytes" => TypeExpr::Bytes,
            "null" => TypeExpr::Null,
            "unknown" => TypeExpr::Unknown,
            "any" => TypeExpr::Any,
            "blob" => TypeExpr::Blob,
            "cid-link" => TypeExpr::CidLink,
            "object" => TypeExpr::Map {
                values: Box::new(TypeExpr::Unknown),
            },
            "array" => TypeExpr::Array {
                items: Box::new(TypeExpr::Unknown),
            },
            "true" => TypeExpr::Literal {
                value: Value::Bool(true),
            },
            "false" => TypeExpr::Literal {
                value: Value::Bool(false),
            },
            _ => TypeExpr::Ref { name: word },
        }
    }
}

/// Unions flatten: `(a | b) | c` and `a? | c` are one list, with `null` at
/// most once.
fn push_member(members: &mut Vec<TypeExpr>, ty: TypeExpr) {
    match ty {
        TypeExpr::Union { members: inner, .. } => {
            for member in inner {
                push_member(members, member);
            }
        }
        other => {
            if !members.contains(&other) {
                members.push(other);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> TypeExpr {
        parse_type(s).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    #[test]
    fn builtins_parse() {
        for (s, t) in [
            ("string", TypeExpr::string()),
            ("integer", TypeExpr::Integer),
            ("unknown", TypeExpr::Unknown),
            ("cid-link", TypeExpr::CidLink),
            ("blob", TypeExpr::Blob),
        ] {
            assert_eq!(parse(s), t, "{s}");
        }
    }

    #[test]
    fn object_and_array_alone_are_open() {
        assert_eq!(
            parse("object"),
            TypeExpr::Map {
                values: Box::new(TypeExpr::Unknown)
            }
        );
        assert_eq!(
            parse("array"),
            TypeExpr::Array {
                items: Box::new(TypeExpr::Unknown)
            }
        );
    }

    #[test]
    fn postfix_binds_tighter_than_union() {
        let TypeExpr::Union { members, .. } = parse("string[]? | 'a'") else {
            panic!("not a union");
        };
        assert_eq!(
            members,
            [
                TypeExpr::Array {
                    items: Box::new(TypeExpr::string())
                },
                TypeExpr::Null,
                TypeExpr::Literal {
                    value: Value::from("a")
                },
            ]
        );
        assert!(matches!(
            parse("(string | integer)[]"),
            TypeExpr::Array { .. }
        ));
    }

    #[test]
    fn optional_is_a_union_with_null() {
        assert_eq!(
            parse("string?"),
            TypeExpr::Union {
                members: Vec::from([TypeExpr::string(), TypeExpr::Null]),
                open: false
            }
        );
        assert_eq!(parse("string??"), parse("string?"));
        assert_eq!(parse("string? | null"), parse("string?"));
    }

    #[test]
    fn literals() {
        assert_eq!(
            parse("'asc'"),
            TypeExpr::Literal {
                value: Value::from("asc")
            }
        );
        assert_eq!(parse("\"desc\""), parse("'desc'"));
        assert_eq!(
            parse("-42"),
            TypeExpr::Literal {
                value: Value::from(-42)
            }
        );
        assert_eq!(
            parse("true"),
            TypeExpr::Literal {
                value: Value::Bool(true)
            }
        );
        assert_eq!(
            parse(r"'it\'s'"),
            TypeExpr::Literal {
                value: Value::from("it's")
            }
        );
    }

    #[test]
    fn refs_are_dotted_names() {
        assert_eq!(
            parse("db.Record"),
            TypeExpr::Ref {
                name: "db.Record".into()
            }
        );
    }

    #[test]
    fn whitespace_is_insignificant() {
        assert_eq!(parse("  string |integer  "), parse("string | integer"));
    }

    #[test]
    fn printing_round_trips() {
        for s in [
            "string",
            "integer?",
            "Record[]",
            "'asc' | 'desc'",
            "(string | integer)[]",
            "string[]?",
            "string | integer | null",
            "object",
            "array",
            "'it\\'s'",
            "-3",
        ] {
            assert_eq!(print_type(&parse(s)), s);
        }
    }

    #[test]
    fn errors_say_where() {
        let e = parse_type("string | ").unwrap_err();
        assert_eq!(e.column, Some(10), "{e}");
        let e = parse_type("'open").unwrap_err();
        assert_eq!(e.to_string(), "unclosed string literal at column 1");
        assert_eq!(
            parse_type("").unwrap_err().to_string(),
            "a type cannot be empty"
        );
        assert_eq!(
            parse_type("string)").unwrap_err().to_string(),
            "unexpected ')' at column 7"
        );
        assert_eq!(
            parse_type("string[").unwrap_err().to_string(),
            "expected ']' after '[' at column 8"
        );
        assert_eq!(
            parse_type("(string").unwrap_err().to_string(),
            "expected ')' at column 8"
        );
        assert_eq!(
            parse_type("string & integer").unwrap_err().to_string(),
            "unexpected '&' at column 8"
        );
    }
}
