/**
 * What the instance's attached account signs in with: `identity:*` for its
 * DID document, and the schema scope so the instance can publish and
 * unpublish its lexicons there. Mirrors `SCHEMA_WRITE_SCOPE` in
 * src/lexicon_publishing/status.rs, which a Rust test pins.
 */
export const ATTACHED_ACCOUNT_SCOPE =
  "atproto identity:* repo:com.atproto.lexicon.schema?action=create&action=update&action=delete";
