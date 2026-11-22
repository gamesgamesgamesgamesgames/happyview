//! A space repo as a STAR archive: Proposal 0016's `atproto-space-repo`
//! profile, which `com.atproto.space.getRepo` serves.
//!
//! The metadata is the commit's fields under `$type: "atproto-space-repo"`.
//! Each pair is a record: the key is its path, `{collection}/{rkey}`, and the
//! value its canonical DRISL encoding. Pairs are in strict byte order of their
//! keys, which is not the order `ORDER BY collection, rkey` gives when one
//! collection's name is a prefix of another's (`.` sorts before `/`), so the
//! records are sorted here.
//!
//! The commit's MAC is not part of the archive; it travels in the response's
//! HTTP signature.

use std::collections::BTreeMap;

use ipld_core::ipld::Ipld;

use crate::error::AppError;
use crate::spaces::space_mac::Commit;
use crate::spaces::types::SpaceRecord;

/// The media type `getRepo` answers with.
pub const MEDIA_TYPE: &str = "application/vnd.dasl.star";

/// The profile named by the archive's `$type`.
pub const PROFILE: &str = "atproto-space-repo";

/// The archive for one repo at `commit`, holding `records`.
pub fn serialize_repo(commit: &Commit, records: &[SpaceRecord]) -> Result<Vec<u8>, AppError> {
    let mut pairs = records
        .iter()
        .map(|r| {
            let value = crate::cid_verify::record_to_dag_cbor(&r.record).ok_or_else(|| {
                AppError::Internal(format!("record {} cannot be encoded as DRISL", r.uri))
            })?;
            Ok((format!("{}/{}", r.collection, r.rkey).into_bytes(), value))
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    if let Some(dup) = pairs.windows(2).find(|w| w[0].0 == w[1].0) {
        return Err(AppError::Internal(format!(
            "repo holds two records at {}",
            String::from_utf8_lossy(&dup[0].0)
        )));
    }

    let meta = BTreeMap::from([
        ("$type".to_string(), Ipld::String(PROFILE.into())),
        ("ver".to_string(), Ipld::Integer(commit.ver.into())),
        ("repo".to_string(), Ipld::String(commit.repo.clone())),
        ("space".to_string(), Ipld::String(commit.space.clone())),
        ("rev".to_string(), Ipld::String(commit.rev.clone())),
        ("data".to_string(), Ipld::Bytes(commit.data.to_vec())),
    ]);
    happyview_star::write_star(
        &meta,
        pairs.iter().map(|(k, v)| (k.as_slice(), v.as_slice())),
    )
    .map_err(|e| AppError::Internal(format!("failed to write the repo archive: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spaces::lthash::{LtHashState, record_element};
    use happyview_star::{Limits, StarReader};

    fn record(collection: &str, rkey: &str, value: serde_json::Value) -> SpaceRecord {
        let cid = crate::spaces::service::content_cid(&value).unwrap();
        SpaceRecord {
            uri: format!(
                "at://did:example:space/space/app.example.group/test/did:example:alice/{collection}/{rkey}"
            ),
            space_id: "s".into(),
            author_did: "did:example:alice".into(),
            collection: collection.into(),
            rkey: rkey.into(),
            record: value,
            cid,
            indexed_at: "2026-11-22T00:00:00Z".into(),
        }
    }

    fn records() -> Vec<SpaceRecord> {
        // In the order the database returns them: by collection, then rkey.
        // `app.example.post` sorts before `app.example.post.reply` as a
        // column, but after it as a path (`.` < `/`).
        vec![
            record(
                "app.example.like",
                "3kbcq3p7ad403",
                serde_json::json!({"$type": "app.example.like", "subject": "at://x"}),
            ),
            record(
                "app.example.post",
                "3kbcq3p7ad401",
                serde_json::json!({"$type": "app.example.post", "text": "hello"}),
            ),
            record(
                "app.example.post.reply",
                "3kbcq3p7ad402",
                serde_json::json!({"$type": "app.example.post.reply", "text": "hi"}),
            ),
        ]
    }

    fn commit_for(records: &[SpaceRecord]) -> Commit {
        let mut state = LtHashState::new();
        for r in records {
            state.add(&record_element(&r.collection, &r.rkey, &r.cid));
        }
        Commit {
            ver: 1,
            repo: "did:example:alice".into(),
            space: "at://did:example:space/space/app.example.group/test".into(),
            rev: "3kbcq3p7ad42b".into(),
            data: state.hash(),
        }
    }

    #[test]
    fn the_archive_holds_the_commit_then_every_record_in_path_order() {
        let records = records();
        let commit = commit_for(&records);
        let star = serialize_repo(&commit, &records).unwrap();

        let mut reader = StarReader::open(star.as_slice(), Limits::default()).unwrap();
        assert_eq!(reader.profile(), PROFILE);
        let meta = reader.meta().clone();
        assert_eq!(meta.get("ver"), Some(&Ipld::Integer(1)));
        assert_eq!(meta.get("repo"), Some(&Ipld::String(commit.repo.clone())));
        assert_eq!(meta.get("space"), Some(&Ipld::String(commit.space.clone())));
        assert_eq!(meta.get("rev"), Some(&Ipld::String(commit.rev.clone())));
        assert_eq!(meta.get("data"), Some(&Ipld::Bytes(commit.data.to_vec())));
        assert_eq!(
            meta.len(),
            6,
            "the metadata is $type plus the commit's fields"
        );

        let mut keys = Vec::new();
        let mut state = LtHashState::new();
        while let Some(pair) = reader.next_pair().unwrap() {
            let path = String::from_utf8(pair.key).unwrap();
            let (collection, rkey) = path.rsplit_once('/').unwrap();
            let cid = crate::cid_verify::dag_cbor_cid(&pair.value)
                .unwrap()
                .to_string();
            state.add(&record_element(collection, rkey, &cid));
            keys.push(path);
        }
        assert_eq!(
            keys,
            [
                "app.example.like/3kbcq3p7ad403",
                "app.example.post.reply/3kbcq3p7ad402",
                "app.example.post/3kbcq3p7ad401",
            ],
            "pairs are in byte order of their paths"
        );
        assert_eq!(
            state.hash(),
            commit.data,
            "folding the archive's records reproduces the commit"
        );
    }

    #[test]
    fn an_empty_repo_is_its_commit_alone() {
        let commit = commit_for(&[]);
        let star = serialize_repo(&commit, &[]).unwrap();
        let mut reader = StarReader::open(star.as_slice(), Limits::default()).unwrap();
        assert!(reader.next_pair().unwrap().is_none());
    }
}
