use regex::Regex;
use std::sync::LazyLock;

/// Matches an xrpc.query or xrpc.procedure call and captures the method name.
static XRPC_CALL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"xrpc\.(?:query|procedure)\(\s*["']([a-zA-Z][a-zA-Z0-9]*(?:\.[a-zA-Z][a-zA-Z0-9]*)*)["']"#).unwrap()
});

/// Matches a Lua line comment at the start of the non-whitespace content on a line.
/// Used to detect lines that are fully commented out before any code.
static LUA_COMMENT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*--").unwrap());

/// Strips Lua block comments (`--[[ ... ]]`) from source, including multi-line ones.
static BLOCK_COMMENT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"--\[\[[\s\S]*?\]\]").unwrap());

pub fn extract_outbound_xrpcs(source: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();

    let stripped = BLOCK_COMMENT_RE.replace_all(source, "");

    for line in stripped.lines() {
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

    #[test]
    fn handles_multiline_scripts() {
        let source = r#"
function handle(input, params)
    local game = xrpc.query("games.birb.chess.getGame",
        { uri = params.uri })
    local result = xrpc.procedure("games.birb.chess.makeMove",
        { game = game.uri,
          move = params.move })
    return result
end
        "#;
        let result = extract_outbound_xrpcs(source);
        assert_eq!(
            result,
            vec!["games.birb.chess.getGame", "games.birb.chess.makeMove",]
        );
    }

    #[test]
    fn handles_mixed_comments_and_code() {
        let source = r#"
function handle(input, params)
    -- This is a comment about the next call
    local game = xrpc.query("games.birb.chess.getGame", { uri = params.uri })
    -- local old = xrpc.query("games.birb.chess.oldEndpoint", {})
    -- xrpc.procedure("games.birb.chess.deprecatedMove", {})
    xrpc.procedure("games.birb.chess.makeMove", { game = game.uri })
    return game
end
        "#;
        let result = extract_outbound_xrpcs(source);
        assert_eq!(
            result,
            vec!["games.birb.chess.getGame", "games.birb.chess.makeMove",]
        );
    }

    #[test]
    fn ignores_block_comment_single_line() {
        let source = r#"
            --[[ local result = xrpc.query("games.birb.chess.getGame", { uri = params.uri }) ]]
            return {}
        "#;
        let result = extract_outbound_xrpcs(source);
        assert!(result.is_empty());
    }

    #[test]
    fn ignores_block_comment_multiline() {
        let source = r#"
            --[[
            local result = xrpc.query("games.birb.chess.getGame", { uri = params.uri })
            xrpc.procedure("games.birb.chess.makeMove", {})
            ]]
            local active = xrpc.query("games.birb.chess.listGames", {})
        "#;
        let result = extract_outbound_xrpcs(source);
        assert_eq!(result, vec!["games.birb.chess.listGames"]);
    }

    #[test]
    fn dynamic_method_names_not_detected() {
        let source = r#"
            local method = "games.birb.chess.getGame"
            local result = xrpc.query(method, {})
        "#;
        let result = extract_outbound_xrpcs(source);
        assert!(
            result.is_empty(),
            "dynamically constructed method names should not be detected"
        );
    }

    #[test]
    fn extracts_from_complex_lua() {
        let source = r#"
function handle(input, params)
    local results = {}

    if params.include_profile then
        local profile = xrpc.query("app.bsky.actor.getProfile", { actor = params.did })
        table.insert(results, profile)
    end

    for i = 1, params.count do
        local feed = xrpc.query("app.bsky.feed.getAuthorFeed", { actor = params.did, limit = 10 })
        for _, post in ipairs(feed.feed) do
            table.insert(results, post)
        end
    end

    if params.should_notify then
        xrpc.procedure("games.birb.chess.sendNotification", { target = params.did })
    end

    return { items = results }
end
        "#;
        let result = extract_outbound_xrpcs(source);
        assert_eq!(
            result,
            vec![
                "app.bsky.actor.getProfile",
                "app.bsky.feed.getAuthorFeed",
                "games.birb.chess.sendNotification",
            ]
        );
    }
}
