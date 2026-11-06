//! The upstream STAR interop fixtures: every valid archive reads to its
//! expected contents and writes back to the same bytes, and every invalid one
//! is refused.

use happyview_star::{Limits, Pair, StarReader, write_star};
use serde_json::Value;

fn fixtures(json: &str) -> Vec<Value> {
    serde_json::from_str(json).unwrap()
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn read_all(bytes: &[u8]) -> Result<(StarReader<&[u8]>, Vec<Pair>), happyview_star::StarError> {
    let mut reader = StarReader::open(bytes, Limits::default())?;
    let mut pairs = Vec::new();
    while let Some(pair) = reader.next_pair()? {
        pairs.push(pair);
    }
    Ok((reader, pairs))
}

#[test]
fn valid_archives_read_and_round_trip() {
    let cases = fixtures(include_str!("interop/valid-star-fixtures.json"));
    assert!(!cases.is_empty());
    for case in cases {
        let comment = case["comment"].as_str().unwrap();
        let bytes = hex(case["star_hex"].as_str().unwrap());
        let (reader, pairs) =
            read_all(&bytes).unwrap_or_else(|e| panic!("{comment}: refused: {e}"));

        let expected = &case["decoded"];
        assert_eq!(
            reader.profile(),
            expected["meta"]["$type"].as_str().unwrap(),
            "{comment}"
        );
        let expected_pairs: Vec<Pair> = expected["pairs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| Pair {
                key: hex(p["key_hex"].as_str().unwrap()),
                value: hex(p["value_hex"].as_str().unwrap()),
            })
            .collect();
        assert_eq!(pairs, expected_pairs, "{comment}");

        let rewritten = write_star(
            reader.meta(),
            pairs.iter().map(|p| (p.key.as_slice(), p.value.as_slice())),
        )
        .unwrap();
        assert_eq!(
            rewritten, bytes,
            "{comment}: did not write back the same bytes"
        );
    }
}

#[test]
fn invalid_archives_are_refused() {
    let cases = fixtures(include_str!("interop/invalid-star-fixtures.json"));
    assert!(!cases.is_empty());
    for case in cases {
        let comment = case["comment"].as_str().unwrap();
        let bytes = hex(case["star_hex"].as_str().unwrap());
        assert!(read_all(&bytes).is_err(), "{comment}: was accepted");
    }
}
