//! `ctx` and the event inputs, per script kind, as the runners fill them:
//! `script::build_input` from each runner's `Invocation`.

use happyview_plugin_sdk::declarations::{CtxShapes, EventShapes, ObjectType, Property, TypeExpr};

pub fn ctx_shapes() -> CtxShapes {
    CtxShapes {
        query: with(&[
            caller_did().optional(),
            method(),
            collection(),
            space().optional(),
        ]),
        procedure: with(&[
            caller_did(),
            method(),
            collection(),
            Property::new("params", map(TypeExpr::Unknown))
                .describe("The query parameters the request carried"),
            Property::new("delegate_did", TypeExpr::string().nullable())
                .optional()
                .describe("The account the caller is acting for, when it delegated"),
            space().optional(),
        ]),
        record: with(&[caller_did().describe("The record's author"), collection()]),
        label: with(&[]),
        job: with(&[
            caller_did()
                .optional()
                .describe("Who created the job, when anyone did"),
            Property::new(
                "job",
                TypeExpr::Object(ObjectType {
                    properties: vec![
                        Property::new("id", TypeExpr::string()).describe("The job's id"),
                    ],
                }),
            )
            .describe("The job this run is"),
        ]),
        test: with(&[]),
    }
}

pub fn event_shapes() -> EventShapes {
    let string = |name: &str, description: &str| {
        Property::new(name, TypeExpr::string()).describe(description)
    };
    EventShapes {
        record: ObjectType {
            properties: vec![
                Property::new(
                    "action",
                    TypeExpr::Union {
                        members: ["create", "update", "delete"]
                            .into_iter()
                            .map(|action| TypeExpr::Literal {
                                value: action.into(),
                            })
                            .collect(),
                        open: false,
                    },
                ),
                string("uri", "The record's AT URI"),
                string("did", "The repo the record is in"),
                string("collection", "The record's collection NSID"),
                string("rkey", "The record's key"),
                Property::new(
                    "record",
                    TypeExpr::Var {
                        name: "record".into(),
                    }
                    .nullable(),
                )
                .describe("The record body; null for a delete"),
            ],
        },
        label: ObjectType {
            properties: vec![
                string("src", "The labeler's DID"),
                string("uri", "What the label applies to"),
                string("val", "The label's value"),
                Property::new("neg", TypeExpr::Boolean).describe("Whether this removes the label"),
                string("cts", "When the label was created"),
                Property::new("exp", TypeExpr::string().nullable())
                    .optional()
                    .describe("When the label expires"),
            ],
        },
    }
}

/// The fields every kind gets, then `extra`.
fn with(extra: &[Property]) -> ObjectType {
    let mut properties = vec![
        Property::new("trigger", TypeExpr::string()).describe("The trigger that ran this script"),
        Property::new("has_pds_auth", TypeExpr::Boolean)
            .describe("Whether library calls can act as the caller on their PDS"),
        Property::new("env", map(TypeExpr::string())).describe("The script variables"),
    ];
    properties.extend(extra.iter().cloned());
    ObjectType { properties }
}

fn caller_did() -> Property {
    Property::new("caller_did", TypeExpr::string()).describe("The caller's DID")
}

fn method() -> Property {
    Property::new("method", TypeExpr::string()).describe("The XRPC method's NSID")
}

fn collection() -> Property {
    Property::new("collection", TypeExpr::string())
        .describe("The record collection this method operates on")
}

fn space() -> Property {
    Property::new(
        "space",
        TypeExpr::Object(ObjectType {
            properties: ["uri", "id", "did", "authority_did", "spaceType", "skey"]
                .into_iter()
                .map(|name| Property::new(name, TypeExpr::string()))
                .collect(),
        }),
    )
    .describe("The space the request is scoped to")
}

fn map(values: TypeExpr) -> TypeExpr {
    TypeExpr::Map {
        values: Box::new(values),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::plugin::{ScriptExecuteContext, ScriptJob, ScriptSpace};

    fn keys(object: &ObjectType) -> BTreeSet<String> {
        object.properties.iter().map(|p| p.name.clone()).collect()
    }

    fn every_shape() -> Vec<(&'static str, ObjectType)> {
        let s = ctx_shapes();
        vec![
            ("query", s.query),
            ("procedure", s.procedure),
            ("record", s.record),
            ("label", s.label),
            ("job", s.job),
            ("test", s.test),
        ]
    }

    /// A field added to `ExecuteContext` has to be described here, and a
    /// field described here has to exist there: the two are one contract.
    #[test]
    fn the_shapes_cover_exactly_what_the_context_carries() {
        let full = ScriptExecuteContext {
            trigger: "t".into(),
            caller_did: Some("did:plc:a".into()),
            has_pds_auth: true,
            env: BTreeMap::from([("K".into(), "v".into())]),
            method: Some("m".into()),
            collection: Some("c".into()),
            params: Some(Default::default()),
            delegate_did: Some("did:plc:b".into()),
            space: Some(ScriptSpace {
                uri: "u".into(),
                id: "i".into(),
                did: "d".into(),
                authority_did: "a".into(),
                type_nsid: "s".into(),
                skey: "k".into(),
            }),
            job: Some(ScriptJob { id: "j".into() }),
        };
        let wire = serde_json::to_value(&full).unwrap();
        let carried: BTreeSet<String> = wire.as_object().unwrap().keys().cloned().collect();
        let described: BTreeSet<String> = every_shape().iter().flat_map(|(_, s)| keys(s)).collect();
        assert_eq!(carried, described);

        let space_keys: BTreeSet<String> =
            wire["space"].as_object().unwrap().keys().cloned().collect();
        let shape = ctx_shapes().query;
        let TypeExpr::Object(space) = &shape
            .properties
            .iter()
            .find(|p| p.name == "space")
            .unwrap()
            .ty
        else {
            panic!("space is not an object")
        };
        assert_eq!(space_keys, keys(space));
    }

    /// What every kind gets is what `build_input` always sets.
    #[test]
    fn every_kind_starts_with_the_common_fields() {
        for (kind, shape) in every_shape() {
            let names: Vec<&str> = shape.properties.iter().map(|p| p.name.as_str()).collect();
            assert_eq!(names[..3], ["trigger", "has_pds_auth", "env"], "{kind}");
            assert!(shape.properties[..3].iter().all(|p| p.required), "{kind}");
        }
    }

    #[test]
    fn the_label_event_matches_what_the_runner_sends() {
        let event = crate::lua::scripts::LabelAppliedEvent {
            src: "did:plc:l".into(),
            uri: "at://x".into(),
            val: "spam".into(),
            neg: false,
            cts: "2026-01-01T00:00:00+00:00".into(),
            exp: Some("2027-01-01T00:00:00+00:00".into()),
        };
        let wire = serde_json::to_value(&event).unwrap();
        let sent: BTreeSet<String> = wire.as_object().unwrap().keys().cloned().collect();
        assert_eq!(sent, keys(&event_shapes().label));
    }
}
