use regex::Regex;
use std::sync::LazyLock;

/// Matches an xrpc.query or xrpc.procedure call and captures the method name.
static XRPC_CALL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"xrpc\.(?:query|procedure)\(\s*["']([a-zA-Z][a-zA-Z0-9]*(?:\.[a-zA-Z][a-zA-Z0-9]*)*)["']"#).unwrap()
});

/// Matches a Lua line comment at the start of the non-whitespace content on a line.
/// Used to detect lines that are fully commented out before any code.
static LUA_COMMENT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*--").unwrap());

pub fn extract_outbound_xrpcs(source: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();

    for line in source.lines() {
        // Skip lines whose non-whitespace content starts with a Lua comment (`--`).
        if LUA_COMMENT_RE.is_match(line) {
            continue;
        }

        for cap in XRPC_CALL_RE.captures_iter(line) {
            if let Some(method) = cap.get(1) {
                let method = method.as_str().to_string();
                if seen.insert(method.clone()) {
                    result.push(method);
                }
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_script_returns_empty() {
        let result = extract_outbound_xrpcs("");
        assert!(result.is_empty());
    }

    #[test]
    fn no_xrpc_calls_returns_empty() {
        let source = r#"
            local record = params.record
            return { records = { record } }
        "#;
        let result = extract_outbound_xrpcs(source);
        assert!(result.is_empty());
    }

    #[test]
    fn detects_xrpc_query_call() {
        let source = r#"
            local result = xrpc.query("games.birb.chess.getGame", { uri = params.uri })
            return result
        "#;
        let result = extract_outbound_xrpcs(source);
        assert_eq!(result, vec!["games.birb.chess.getGame"]);
    }

    #[test]
    fn detects_xrpc_procedure_call() {
        let source = r#"
            xrpc.procedure("games.birb.chess.makeMove", { game = params.game, move = params.move })
        "#;
        let result = extract_outbound_xrpcs(source);
        assert_eq!(result, vec!["games.birb.chess.makeMove"]);
    }

    #[test]
    fn detects_multiple_calls() {
        let source = r#"
            local game = xrpc.query("games.birb.chess.getGame", { uri = params.uri })
            xrpc.procedure("games.birb.chess.makeMove", { game = game.uri, move = params.move })
            local games = xrpc.query("games.birb.chess.listGames", {})
        "#;
        let result = extract_outbound_xrpcs(source);
        assert_eq!(
            result,
            vec![
                "games.birb.chess.getGame",
                "games.birb.chess.makeMove",
                "games.birb.chess.listGames",
            ]
        );
    }

    #[test]
    fn deduplicates_repeated_calls() {
        let source = r#"
            local a = xrpc.query("games.birb.chess.getGame", { uri = "a" })
            local b = xrpc.query("games.birb.chess.getGame", { uri = "b" })
        "#;
        let result = extract_outbound_xrpcs(source);
        assert_eq!(result, vec!["games.birb.chess.getGame"]);
    }

    #[test]
    fn ignores_commented_out_calls() {
        let source = r#"
            -- local result = xrpc.query("games.birb.chess.getGame", { uri = params.uri })
            return {}
        "#;
        let result = extract_outbound_xrpcs(source);
        assert!(result.is_empty());
    }

    #[test]
    fn handles_single_quotes_and_double_quotes() {
        let source = r#"
            local a = xrpc.query('games.birb.chess.getGame', {})
            local b = xrpc.query("games.birb.chess.listGames", {})
        "#;
        let result = extract_outbound_xrpcs(source);
        assert_eq!(
            result,
            vec!["games.birb.chess.getGame", "games.birb.chess.listGames",]
        );
    }
}
