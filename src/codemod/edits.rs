//! Byte-range edits spliced into the original source.
//!
//! Splicing beats reprinting the tree: every byte nobody touched — comments,
//! blank lines, hand-alignment, the author's quote style — survives because it
//! is never re-rendered.

/// One piece of a replacement.
#[derive(Debug, Clone)]
pub enum Piece {
    Text(String),
    /// A span of the original source, re-rendered with whatever edits fall
    /// inside it. A rewrite that carries a subexpression forward carries that
    /// subexpression's own rewrites with it.
    Source(usize, usize),
}

#[derive(Debug, Clone)]
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub pieces: Vec<Piece>,
}

impl Edit {
    pub fn replace(start: usize, end: usize, text: impl Into<String>) -> Self {
        Edit {
            start,
            end,
            pieces: vec![Piece::Text(text.into())],
        }
    }

    pub fn insert(at: usize, text: impl Into<String>) -> Self {
        Edit {
            start: at,
            end: at,
            pieces: vec![Piece::Text(text.into())],
        }
    }
}

/// Apply `edits` to `source`. An edit nested inside another is skipped at the
/// outer level; it reappears only if the outer edit names its span with a
/// [`Piece::Source`], which is how a discarded construct takes its contents
/// with it.
pub fn apply(source: &str, edits: &[Edit]) -> String {
    let mut order: Vec<usize> = (0..edits.len()).collect();
    // An insertion at a replacement's own start goes first, so a comment line
    // written above a statement survives that statement being rewritten.
    order.sort_by_key(|&i| (edits[i].start, edits[i].start != edits[i].end, i));
    render_range(source, edits, &order, 0, source.len(), None)
}

fn render_range(
    source: &str,
    edits: &[Edit],
    order: &[usize],
    from: usize,
    to: usize,
    skip: Option<usize>,
) -> String {
    let mut out = String::new();
    let mut cursor = from;
    for &i in order {
        if Some(i) == skip {
            continue;
        }
        let edit = &edits[i];
        if edit.start < from || edit.end > to || edit.start < cursor {
            continue;
        }
        out.push_str(&source[cursor..edit.start]);
        for piece in &edit.pieces {
            match piece {
                Piece::Text(text) => out.push_str(text),
                Piece::Source(a, b) => {
                    out.push_str(&render_range(source, edits, order, *a, *b, Some(i)))
                }
            }
        }
        cursor = edit.end;
    }
    out.push_str(&source[cursor..to]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untouched_source_comes_back_byte_identical() {
        let source = "-- hi\nlocal x = 1 --[[ keep ]]\n";
        assert_eq!(apply(source, &[]), source);
    }

    #[test]
    fn replacements_apply_left_to_right() {
        let source = "a b c";
        let edits = vec![Edit::replace(0, 1, "x"), Edit::replace(4, 5, "z")];
        assert_eq!(apply(source, &edits), "x b z");
    }

    #[test]
    fn a_source_piece_carries_its_own_edits() {
        let source = "db.query(params.cursor)";
        let edits = vec![
            Edit {
                start: 0,
                end: source.len(),
                pieces: vec![
                    Piece::Text("db.records():cursor(".into()),
                    Piece::Source(9, 22),
                    Piece::Text("):run()".into()),
                ],
            },
            Edit::replace(9, 15, "input"),
        ];
        assert_eq!(
            apply(source, &edits),
            "db.records():cursor(input.cursor):run()"
        );
    }

    #[test]
    fn a_nested_edit_no_piece_names_is_dropped_with_its_construct() {
        let source = "db.query(now())";
        let edits = vec![
            Edit::replace(0, source.len(), "db.records():run()"),
            Edit::replace(9, 12, "time.now"),
        ];
        assert_eq!(apply(source, &edits), "db.records():run()");
    }

    #[test]
    fn an_insert_at_a_replacements_start_survives_it() {
        let source = "db.query({})";
        let edits = vec![
            Edit::replace(0, source.len(), "db.records():run()"),
            Edit::insert(0, "-- note\n"),
        ];
        assert_eq!(apply(source, &edits), "-- note\ndb.records():run()");
    }

    #[test]
    fn inserts_keep_their_relative_order_at_one_position() {
        let source = "x";
        let edits = vec![Edit::insert(0, "a\n"), Edit::insert(0, "b\n")];
        assert_eq!(apply(source, &edits), "a\nb\nx");
    }
}
