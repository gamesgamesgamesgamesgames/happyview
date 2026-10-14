use serde::Serialize;

use crate::AppState;
use crate::catalog::Layer;
use crate::error::AppError;

use super::publish::{parsed_rules, publishable, rule_for};
use super::store::{self, Publication, Rule};
use super::target::{self, LinkedRepoTargets, TargetInfo};
use super::{Pattern, PublishStatus, compute_status, record_cid};

#[derive(Debug, Clone, Serialize)]
pub struct RuleView {
    #[serde(flatten)]
    pub rule: Rule,
    pub target: TargetInfo,
}

impl RuleView {
    pub fn redact_linked_repo(&mut self) {
        if self.rule.target_kind == target::KIND_LINKED_REPO {
            self.rule.linked_repo_id = None;
        }
        self.target.redact_linked_repo();
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LexiconView {
    pub nsid: String,
    pub layer: Layer,
    pub rule_id: Option<String>,
    pub rule_pattern: Option<String>,
    pub auto_publish: bool,
    /// The matched rule's target; `None` when no rule matches.
    pub target: Option<TargetInfo>,
    pub status: PublishStatus,
    pub publication: Option<Publication>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Overview {
    /// See [`usable`].
    pub usable: bool,
    pub targets: Vec<TargetInfo>,
    pub rules: Vec<RuleView>,
    pub lexicons: Vec<LexiconView>,
    /// Records still on a PDS whose lexicon HappyView no longer publishes,
    /// listed so they can be unpublished.
    pub orphans: Vec<Publication>,
}

impl Overview {
    /// Publication rows keep their DID: the record is public in that repo, and
    /// its `uri` names the DID anyway.
    pub fn redact_linked_repos(&mut self) {
        for target in &mut self.targets {
            target.redact_linked_repo();
        }
        for rule in &mut self.rules {
            rule.redact_linked_repo();
        }
        for target in self.lexicons.iter_mut().filter_map(|l| l.target.as_mut()) {
            target.redact_linked_repo();
        }
    }
}

/// Whether any account can take a publish, or give up a record HappyView put
/// there. With neither, defaults and a publish button have nothing to act on,
/// so the dashboard keeps the publishing page closed.
pub fn usable(targets: &[TargetInfo]) -> bool {
    targets.iter().any(|t| t.available || t.can_unpublish)
}

/// The DID `_lexicon.<authority>` should name for `nsid`: the account its
/// default publishes to, or failing that the account already holding its
/// record. `None` for a lexicon HappyView does not publish, or when neither
/// is known. A caller who may not see linked repos learns a linked repo's DID
/// only from a publication, where the record's `uri` names it anyway.
pub async fn expected_dns_did(
    state: &AppState,
    nsid: &str,
    can_see_linked_repos: bool,
) -> Result<Option<String>, AppError> {
    if !publishable(state).await?.iter().any(|l| l.id == nsid) {
        return Ok(None);
    }
    let rules = parsed_rules(store::list_rules(state).await?);
    if let Some(rule) = rule_for(nsid, &rules) {
        let linked = LinkedRepoTargets::load(state).await?;
        let instance = target::instance_target(state).await?;
        let mut target = target::target_for_rule(state, rule, &linked, &instance).await;
        if !can_see_linked_repos {
            target.redact_linked_repo();
        }
        if target.did.is_some() {
            return Ok(target.did);
        }
    }
    Ok(store::get_publication(state, nsid)
        .await?
        .and_then(|p| p.did))
}

/// Most specific first, the order in which they win a match.
fn sort_rules(rules: &mut [(Rule, Pattern)]) {
    rules.sort_by(|(a, pa), (b, pb)| {
        pb.specificity()
            .cmp(&pa.specificity())
            .then_with(|| a.pattern.cmp(&b.pattern))
    });
}

pub async fn overview(state: &AppState) -> Result<Overview, AppError> {
    let mut rules = parsed_rules(store::list_rules(state).await?);
    sort_rules(&mut rules);
    let linked = LinkedRepoTargets::load(state).await?;
    let instance = target::instance_target(state).await?;

    let mut rule_views = Vec::with_capacity(rules.len());
    for (rule, _) in &rules {
        rule_views.push(RuleView {
            target: target::target_for_rule(state, rule, &linked, &instance).await,
            rule: rule.clone(),
        });
    }

    let mut targets = vec![instance.clone()];
    targets.extend(linked.targets());
    let usable = usable(&targets);

    let (lexicons, unmatched) = lexicon_views(state, &rules, &linked, &instance).await?;
    let mut orphans: Vec<Publication> = unmatched.into_iter().filter(|p| p.did.is_some()).collect();
    orphans.sort_by(|a, b| a.nsid.cmp(&b.nsid));
    Ok(Overview {
        usable,
        targets,
        rules: rule_views,
        lexicons,
        orphans,
    })
}

/// Also returns the publication rows that matched no publishable lexicon.
pub(super) async fn lexicon_views(
    state: &AppState,
    rules: &[(Rule, Pattern)],
    linked: &LinkedRepoTargets,
    instance: &TargetInfo,
) -> Result<(Vec<LexiconView>, Vec<Publication>), AppError> {
    let mut publications = store::list_publications(state).await?;
    let mut listings = publishable(state).await?;
    listings.sort_by(|a, b| a.id.cmp(&b.id));

    let mut views = Vec::with_capacity(listings.len());
    for listing in listings {
        let rule = rule_for(&listing.id, rules);
        let target = match rule {
            Some(rule) => Some(target::target_for_rule(state, rule, linked, instance).await),
            None => None,
        };
        let publication = publications
            .iter()
            .position(|p| p.nsid == listing.id)
            .map(|i| publications.swap_remove(i));
        let published = publication
            .as_ref()
            .and_then(|p| Some((p.did.as_deref()?, p.cid.as_deref()?)));
        let local_cid = record_cid(&listing.lexicon_json);
        let status = compute_status(
            rule.is_some(),
            target.as_ref().and_then(TargetInfo::writable_did),
            published,
            publication
                .as_ref()
                .is_some_and(|p| p.withdrawn_at.is_some()),
            local_cid.as_deref(),
        );
        views.push(LexiconView {
            rule_id: rule.map(|r| r.id.clone()),
            rule_pattern: rule.map(|r| r.pattern.clone()),
            auto_publish: rule.is_some_and(|r| r.auto_publish),
            nsid: listing.id,
            layer: listing.layer,
            target,
            status,
            publication,
        });
    }
    Ok((views, publications))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(pattern: &str) -> (Rule, Pattern) {
        let r = Rule {
            id: pattern.into(),
            pattern: pattern.into(),
            target_kind: "instance".into(),
            linked_repo_id: None,
            auto_publish: false,
            created_by: "did:plc:a".into(),
            created_at: String::new(),
            updated_at: String::new(),
        };
        let p = r.parsed();
        (r, p)
    }

    fn target(available: bool, can_unpublish: bool) -> TargetInfo {
        TargetInfo {
            kind: "linked_repo".into(),
            linked_repo_id: Some("r".into()),
            did: Some("did:plc:a".into()),
            handle: None,
            available,
            unavailable_reason: None,
            can_unpublish,
        }
    }

    #[test]
    fn usable_needs_a_target_that_can_publish_or_unpublish() {
        assert!(!usable(&[]));
        assert!(!usable(&[target(false, false)]));
        assert!(usable(&[target(false, false), target(true, false)]));
        assert!(usable(&[target(false, true)]));
    }

    #[test]
    fn rules_sort_most_specific_first_then_by_pattern() {
        let mut rules = vec![
            rule("*"),
            rule("com.example.*"),
            rule("com.example.foo.bar"),
            rule("com.example.sub.*"),
            rule("com.another.*"),
        ];
        sort_rules(&mut rules);
        let order: Vec<&str> = rules.iter().map(|(r, _)| r.pattern.as_str()).collect();
        assert_eq!(
            order,
            [
                "com.example.foo.bar",
                "com.example.sub.*",
                "com.another.*",
                "com.example.*",
                "*"
            ]
        );
    }
}
