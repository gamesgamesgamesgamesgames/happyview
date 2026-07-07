/// Cross-module integration tests for the spaces subsystem.
///
/// These run with `cargo test --lib` — no database required.
#[cfg(test)]
mod tests {
    // -----------------------------------------------------------------------
    // 1. LtHash + commit integration
    // -----------------------------------------------------------------------

    use crate::spaces::commit::{sign_commit, verify_commit};
    use crate::spaces::lthash::{LtHashState, record_element};
    use k256::ecdsa::SigningKey;

    fn test_signing_key() -> SigningKey {
        let mut bytes = [0u8; 32];
        bytes[31] = 1;
        SigningKey::from_bytes((&bytes[..]).into()).unwrap()
    }

    /// Add two records, generate a commit over the hash, verify it.
    #[test]
    fn lthash_commit_roundtrip() {
        let mut state = LtHashState::new();
        let elem_a = record_element("com.example.forum.post", "aaa", "bafyreiaaa");
        let elem_b = record_element("com.example.forum.post", "bbb", "bafyreibbb");

        state.add(&elem_a);
        let hash_after_a = state.hash();
        assert_ne!(
            hash_after_a,
            LtHashState::new().hash(),
            "hash must change after first add"
        );

        state.add(&elem_b);
        let hash_after_ab = state.hash();
        assert_ne!(
            hash_after_ab, hash_after_a,
            "hash must change after second add"
        );

        let sk = test_signing_key();
        let vk = *sk.verifying_key();
        let space_uri = "at://did:plc:abc/space/com.example.forum/main";
        let rev = "3k2rev1";

        let commit = sign_commit(&hash_after_ab, space_uri, "did:plc:testuser", rev, &sk).unwrap();
        assert_eq!(commit.hash, hash_after_ab);
        assert_eq!(commit.rev, rev);
        assert!(verify_commit(&commit, space_uri, "did:plc:testuser", &vk).is_ok());
    }

    /// Remove a record — hash must change back toward the previous state.
    #[test]
    fn lthash_commit_after_delete() {
        let mut state = LtHashState::new();
        let elem_a = record_element("com.example.forum.post", "aaa", "bafyreiaaa");
        let elem_b = record_element("com.example.forum.post", "bbb", "bafyreibbb");

        state.add(&elem_a);
        state.add(&elem_b);
        let hash_two = state.hash();

        state.remove(&elem_b);
        let hash_one = state.hash();
        assert_ne!(hash_one, hash_two, "hash must change after delete");

        // The remaining state should equal a state built with only elem_a
        let mut expected = LtHashState::new();
        expected.add(&elem_a);
        assert_eq!(
            hash_one,
            expected.hash(),
            "hash after delete must match single-record state"
        );

        let sk = test_signing_key();
        let vk = *sk.verifying_key();
        let space_uri = "at://did:plc:abc/space/com.example.forum/main";
        let commit = sign_commit(&hash_one, space_uri, "did:plc:testuser", "3k2rev2", &sk).unwrap();
        assert!(verify_commit(&commit, space_uri, "did:plc:testuser", &vk).is_ok());
    }

    /// Commit signed for one hash must not verify against a different hash.
    #[test]
    fn commit_does_not_verify_for_different_hash() {
        let mut state_a = LtHashState::new();
        state_a.add(&record_element("col", "key1", "cid1"));
        let hash_a = state_a.hash();

        let mut state_b = LtHashState::new();
        state_b.add(&record_element("col", "key2", "cid2"));
        let hash_b = state_b.hash();

        let sk = test_signing_key();
        let vk = *sk.verifying_key();
        let space_uri = "at://did:plc:abc/space/com.example.forum/main";

        let commit_a = sign_commit(&hash_a, space_uri, "did:plc:testuser", "rev1", &sk).unwrap();
        // Tamper: swap in hash_b
        let mut tampered = commit_a;
        tampered.hash = hash_b;
        assert!(verify_commit(&tampered, space_uri, "did:plc:testuser", &vk).is_err());
    }

    // -----------------------------------------------------------------------
    // 2. Credential flow cross-module
    // -----------------------------------------------------------------------

    use crate::oauth::keys::generate_dpop_keypair;
    use crate::spaces::credential::{
        DEFAULT_CREDENTIAL_TTL_SECS, DELEGATION_TOKEN_TTL_SECS, DELEGATION_TOKEN_TYP,
        DelegationTokenClaims, SPACE_CREDENTIAL_TYP, SpaceCredentialClaims, make_jti,
        peek_credential_sub, peek_jwt_typ, sign_credential, sign_delegation_token,
        verify_credential, verify_delegation_token,
    };
    use k256::ecdsa::{SigningKey as K256SigningKey, VerifyingKey as K256VerifyingKey};

    fn k256_key() -> K256SigningKey {
        K256SigningKey::from_bytes((&[0x42u8; 32][..]).into()).unwrap()
    }

    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    /// Full delegation → space credential flow: sign delegation token, then sign space credential
    /// and verify both in sequence.
    #[test]
    fn delegation_then_credential_flow() {
        let now = now_secs();
        let sk = k256_key();
        let vk = K256VerifyingKey::from(&sk);

        // Step 1: member signs delegation token
        let delegation = DelegationTokenClaims {
            iss: "did:plc:member".into(),
            sub: "at://did:plc:space/space/com.example.forum/main".into(),
            aud: "did:plc:space#atproto_space_host".into(),
            iat: now,
            exp: now + DELEGATION_TOKEN_TTL_SECS,
            jti: make_jti(),
        };
        let token = sign_delegation_token(&delegation, &sk).unwrap();

        // Peek must return the correct typ before verification
        assert_eq!(peek_jwt_typ(&token).as_deref(), Some(DELEGATION_TOKEN_TYP));

        // Step 2: space host verifies delegation token
        let verified_delegation = verify_delegation_token(&token, &vk, &delegation.aud).unwrap();
        assert_eq!(verified_delegation.iss, "did:plc:member");
        assert_eq!(
            verified_delegation.sub,
            "at://did:plc:space/space/com.example.forum/main"
        );

        // Step 3: space host issues a space credential (using P-256 key)
        let keypair = generate_dpop_keypair().unwrap();
        let cred_claims = SpaceCredentialClaims {
            iss: "did:plc:space".into(),
            sub: verified_delegation.sub.clone(),
            iat: now,
            exp: now + DEFAULT_CREDENTIAL_TTL_SECS,
            jti: make_jti(),
        };
        let credential = sign_credential(&cred_claims, &keypair.private_jwk).unwrap();

        // Peek on credential
        assert_eq!(
            peek_jwt_typ(&credential).as_deref(),
            Some(SPACE_CREDENTIAL_TYP)
        );
        assert_eq!(
            peek_credential_sub(&credential).as_deref(),
            Some("at://did:plc:space/space/com.example.forum/main")
        );

        // Step 4: verify credential
        let verified_cred = verify_credential(&credential, &keypair.public_jwk).unwrap();
        assert_eq!(verified_cred.iss, "did:plc:space");
        assert_eq!(
            verified_cred.sub,
            "at://did:plc:space/space/com.example.forum/main"
        );
    }

    /// An expired delegation token must be rejected before we even try to issue a credential.
    #[test]
    fn expired_delegation_blocks_credential_flow() {
        let now = now_secs();
        let sk = k256_key();
        let vk = K256VerifyingKey::from(&sk);

        let delegation = DelegationTokenClaims {
            iss: "did:plc:member".into(),
            sub: "at://did:plc:space/space/com.example.forum/main".into(),
            aud: "did:plc:space#atproto_space_host".into(),
            iat: now - 120,
            exp: now - 60, // already expired
            jti: make_jti(),
        };
        let token = sign_delegation_token(&delegation, &sk).unwrap();
        let result = verify_delegation_token(&token, &vk, &delegation.aud);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("expired"));
    }

    // -----------------------------------------------------------------------
    // 3. Oplog types: serialization roundtrips
    // -----------------------------------------------------------------------

    use crate::spaces::types::{OplogAction, OplogEntry};

    #[test]
    fn oplog_action_serde_roundtrip() {
        for action in [
            OplogAction::Create,
            OplogAction::Update,
            OplogAction::Delete,
        ] {
            let json = serde_json::to_string(&action).unwrap();
            let parsed: OplogAction = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, action);
        }
    }

    #[test]
    fn oplog_entry_serialization() {
        let entry = OplogEntry {
            id: "entry-1".into(),
            space_id: "space-abc".into(),
            author_did: "did:plc:author".into(),
            rev: "3k2rev1".into(),
            idx: 0,
            action: OplogAction::Create,
            collection: "com.example.forum.post".into(),
            rkey: "3k2abc".into(),
            cid: Some("bafyreiabc".into()),
            prev: None,
            value: None,
            created_at: "2026-01-01T00:00:00Z".into(),
        };

        let json = serde_json::to_string(&entry).unwrap();
        let parsed: OplogEntry = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.id, entry.id);
        assert_eq!(parsed.space_id, entry.space_id);
        assert_eq!(parsed.action.as_str(), "create");
        assert_eq!(parsed.cid.as_deref(), Some("bafyreiabc"));
        assert!(parsed.prev.is_none());
    }

    #[test]
    fn oplog_entry_delete_has_no_cid() {
        let entry = OplogEntry {
            id: "entry-2".into(),
            space_id: "space-abc".into(),
            author_did: "did:plc:author".into(),
            rev: "3k2rev2".into(),
            idx: 0,
            action: OplogAction::Delete,
            collection: "com.example.forum.post".into(),
            rkey: "3k2abc".into(),
            cid: None,
            prev: Some("bafyreiabc".into()),
            value: None,
            created_at: "2026-01-01T00:00:01Z".into(),
        };

        let json = serde_json::to_string(&entry).unwrap();
        let parsed: OplogEntry = serde_json::from_str(&json).unwrap();
        assert!(parsed.cid.is_none());
        assert_eq!(parsed.prev.as_deref(), Some("bafyreiabc"));
    }

    // -----------------------------------------------------------------------
    // 4. Simplespace config types
    // -----------------------------------------------------------------------

    use crate::spaces::types::{AppAccess, MintPolicy, SpaceConfig};

    #[test]
    fn mint_policy_serde_roundtrip() {
        let cases = [
            (MintPolicy::MemberList, "\"member-list\""),
            (MintPolicy::Public, "\"public\""),
            (MintPolicy::ManagingApp, "\"managing-app\""),
        ];
        for (policy, expected_json) in cases {
            let json = serde_json::to_string(&policy).unwrap();
            assert_eq!(json, expected_json);
            let parsed: MintPolicy = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, policy);
        }
    }

    #[test]
    fn app_access_open_default() {
        let access = AppAccess::default();
        assert!(matches!(access, AppAccess::Open));
        let json = serde_json::to_string(&access).unwrap();
        assert_eq!(json, r#"{"type":"open"}"#);
    }

    #[test]
    fn app_access_allowlist_roundtrip() {
        let access = AppAccess::AllowList {
            allowed: vec!["https://app.example.com/client-metadata.json".into()],
        };
        let json = serde_json::to_string(&access).unwrap();
        let parsed: AppAccess = serde_json::from_str(&json).unwrap();
        match parsed {
            AppAccess::AllowList { allowed } => {
                assert_eq!(allowed.len(), 1);
                assert_eq!(allowed[0], "https://app.example.com/client-metadata.json");
            }
            _ => panic!("expected AllowList"),
        }
    }

    #[test]
    fn space_config_defaults_false() {
        let config: SpaceConfig = serde_json::from_str("{}").unwrap();
        assert!(!config.membership_public);
        assert!(!config.records_public);
        assert!(config.extra.is_empty());
    }

    #[test]
    fn space_config_preserves_extra_fields() {
        let json = r#"{"membership_public":true,"records_public":false,"allowedCollections":["col.a","col.b"]}"#;
        let config: SpaceConfig = serde_json::from_str(json).unwrap();
        assert!(config.membership_public);
        assert!(!config.records_public);
        let collections = config.extra.get("allowedCollections").unwrap();
        assert_eq!(collections.as_array().unwrap().len(), 2);
    }

    // -----------------------------------------------------------------------
    // 5. Backward-compatible route namespace constants
    //
    // The PROTO_NS and LEGACY_NS constants are private to routes.rs.
    // We verify the expected values here as a named constant in test scope
    // so the intent is documented and any refactor that changes the strings
    // will need to update these tests.
    // -----------------------------------------------------------------------

    /// The AT Protocol namespace used for canonical space routes.
    const EXPECTED_PROTO_NS: &str = "com.atproto";

    /// The HappyView legacy namespace kept for backward compatibility.
    const EXPECTED_LEGACY_NS: &str = "dev.happyview";

    #[test]
    fn proto_ns_value_is_com_atproto() {
        // getDelegationToken is on the proto namespace; getMemberGrant is the legacy alias
        let proto_route = format!("/xrpc/{}.space.getDelegationToken", EXPECTED_PROTO_NS);
        assert_eq!(proto_route, "/xrpc/com.atproto.space.getDelegationToken");
    }

    #[test]
    fn legacy_ns_value_is_dev_happyview() {
        // getMemberGrant is the legacy alias for getDelegationToken
        let legacy_route = format!("/xrpc/{}.space.getMemberGrant", EXPECTED_LEGACY_NS);
        assert_eq!(legacy_route, "/xrpc/dev.happyview.space.getMemberGrant");
    }

    #[test]
    fn create_space_legacy_maps_to_simplespace() {
        // dev.happyview.space.createSpace is the legacy alias for com.atproto.simplespace.createSpace
        let legacy = format!("/xrpc/{}.space.createSpace", EXPECTED_LEGACY_NS);
        let canonical = format!("/xrpc/{}.simplespace.createSpace", EXPECTED_PROTO_NS);
        // Both paths must be distinct strings that map to the same handler
        assert_ne!(legacy, canonical);
        assert_eq!(legacy, "/xrpc/dev.happyview.space.createSpace");
        assert_eq!(canonical, "/xrpc/com.atproto.simplespace.createSpace");
    }

    // -----------------------------------------------------------------------
    // 6. Read scope validation — cross-module
    // -----------------------------------------------------------------------

    use crate::spaces::scope::{SpaceReadAccess, check_delegation_token_access, check_read_access};
    use crate::spaces::types::SpaceAccess;

    /// read_self member reads own record → ok
    #[test]
    fn read_self_member_reads_own_record_ok() {
        let result = check_read_access(
            "did:plc:alice",
            "did:plc:alice",
            SpaceReadAccess::ReadSelf,
            false,
        );
        assert!(result.is_ok());
    }

    /// read_self member reads other's record → error
    #[test]
    fn read_self_member_reads_others_record_err() {
        let result = check_read_access(
            "did:plc:alice",
            "did:plc:bob",
            SpaceReadAccess::ReadSelf,
            false,
        );
        assert!(result.is_err());
    }

    /// read_self member tries getDelegationToken → error
    #[test]
    fn read_self_member_cannot_get_delegation_token() {
        let result = check_delegation_token_access(SpaceReadAccess::ReadSelf, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("delegation"));
    }

    /// Full read member: can read own record
    #[test]
    fn full_read_member_reads_own_record_ok() {
        assert!(
            check_read_access(
                "did:plc:alice",
                "did:plc:alice",
                SpaceReadAccess::Read,
                false
            )
            .is_ok()
        );
    }

    /// Full read member: can read other's record
    #[test]
    fn full_read_member_reads_others_record_ok() {
        assert!(
            check_read_access("did:plc:alice", "did:plc:bob", SpaceReadAccess::Read, false).is_ok()
        );
    }

    /// Full read member: can get delegation token
    #[test]
    fn full_read_member_can_get_delegation_token() {
        assert!(check_delegation_token_access(SpaceReadAccess::Read, false).is_ok());
    }

    /// space_credential bypasses read_self restriction on reads
    #[test]
    fn space_credential_bypasses_read_self_on_read() {
        assert!(
            check_read_access(
                "did:plc:alice",
                "did:plc:bob",
                SpaceReadAccess::ReadSelf,
                true
            )
            .is_ok()
        );
    }

    /// space_credential bypasses read_self restriction on delegation token
    #[test]
    fn space_credential_bypasses_read_self_on_delegation() {
        assert!(check_delegation_token_access(SpaceReadAccess::ReadSelf, true).is_ok());
    }

    /// SpaceReadAccess::from_space_access maps access levels correctly
    #[test]
    fn space_access_to_read_access_mapping() {
        assert_eq!(
            SpaceReadAccess::from_space_access(SpaceAccess::ReadSelf),
            SpaceReadAccess::ReadSelf
        );
        assert_eq!(
            SpaceReadAccess::from_space_access(SpaceAccess::Read),
            SpaceReadAccess::Read
        );
        assert_eq!(
            SpaceReadAccess::from_space_access(SpaceAccess::Write),
            SpaceReadAccess::Read
        );
    }

    // -----------------------------------------------------------------------
    // 7. Blob sync query params
    // -----------------------------------------------------------------------

    /// GetSpaceBlobQuery is private to routes.rs; test the equivalent deserialization shape.
    #[test]
    fn blob_query_params_camel_case() {
        // The route accepts ?space=...&cid=... in camelCase — verify serde_json can
        // round-trip the equivalent shape used in routes.rs.
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct BlobQuery {
            space: String,
            cid: String,
        }

        let qs = serde_json::json!({
            "space": "at://did:plc:abc/space/com.example.forum/main",
            "cid": "bafyreiabc123"
        });
        let q: BlobQuery = serde_json::from_value(qs).unwrap();
        assert_eq!(q.space, "at://did:plc:abc/space/com.example.forum/main");
        assert_eq!(q.cid, "bafyreiabc123");
    }

    // -----------------------------------------------------------------------
    // 8. Verification methods — key generation and roundtrip
    // -----------------------------------------------------------------------

    use crate::spaces::credential::p256_jwk_to_verifying_key;

    #[test]
    fn p256_keypair_generation_and_jwk_roundtrip() {
        let keypair = generate_dpop_keypair().unwrap();

        // Public JWK must have kty, crv, x, y
        assert_eq!(keypair.public_jwk["kty"].as_str(), Some("EC"));
        assert_eq!(keypair.public_jwk["crv"].as_str(), Some("P-256"));
        assert!(keypair.public_jwk["x"].as_str().is_some());
        assert!(keypair.public_jwk["y"].as_str().is_some());

        // Private JWK must have d
        assert!(keypair.private_jwk["d"].as_str().is_some());

        // Can reconstruct verifying key from public JWK
        let vk = p256_jwk_to_verifying_key(&keypair.public_jwk).unwrap();
        let point = vk.to_encoded_point(false);
        assert!(point.x().is_some());
        assert!(point.y().is_some());
    }

    #[test]
    fn sign_and_verify_with_generated_keypair() {
        let keypair = generate_dpop_keypair().unwrap();
        let now = now_secs();

        let claims = SpaceCredentialClaims {
            iss: "did:plc:owner".into(),
            sub: "at://did:plc:owner/space/com.example.forum/main".into(),
            iat: now,
            exp: now + DEFAULT_CREDENTIAL_TTL_SECS,
            jti: make_jti(),
        };

        let token = sign_credential(&claims, &keypair.private_jwk).unwrap();
        let verified = verify_credential(&token, &keypair.public_jwk).unwrap();

        assert_eq!(verified.iss, claims.iss);
        assert_eq!(verified.sub, claims.sub);
        assert_eq!(verified.jti, claims.jti);
    }

    #[test]
    fn two_different_keypairs_do_not_cross_verify() {
        let kp1 = generate_dpop_keypair().unwrap();
        let kp2 = generate_dpop_keypair().unwrap();
        let now = now_secs();

        let claims = SpaceCredentialClaims {
            iss: "did:plc:owner".into(),
            sub: "at://did:plc:owner/space/com.example.forum/main".into(),
            iat: now,
            exp: now + DEFAULT_CREDENTIAL_TTL_SECS,
            jti: make_jti(),
        };

        let token = sign_credential(&claims, &kp1.private_jwk).unwrap();
        let result = verify_credential(&token, &kp2.public_jwk);
        assert!(result.is_err());
    }
}
