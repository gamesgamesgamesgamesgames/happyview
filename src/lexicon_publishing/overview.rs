use serde::Serialize;

use crate::AppState;
use crate::catalog::Layer;
use crate::error::AppError;
use crate::linked_repos::types::LinkedRepo;

use super::publish::{parsed_rules, publishable, rule_for};
use super::store::{self, Publication, Rule};
use super::target::{self, TargetInfo};
use super::{Pattern, PublishStatus, compute_status, record_cid};

#[derive(Debug, Clone, Serialize)]
pub struct RuleView {
    #[serde(flatten)]
    pub rule: Rule,
    pub target: TargetInfo,
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
    pub targets: Vec<TargetInfo>,
    pub rules: Vec<RuleView>,
    pub lexicons: Vec<LexiconView>,
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
    let grants = crate::linked_repos::db::list(state).await?;
    let instance = target::instance_target(state).await?;

    let mut rule_views = Vec::with_capacity(rules.len());
    for (rule, _) in &rules {
        rule_views.push(RuleView {
            target: target::target_for_rule(state, rule, &grants, &instance).await,
            rule: rule.clone(),
        });
    }

    let mut targets = vec![instance.clone()];
    targets.extend(grants.iter().map(target::linked_repo_target));

    Ok(Overview {
        targets,
        rules: rule_views,
        lexicons: lexicon_views(state, &rules, &grants, &instance).await?,
    })
}

pub(super) async fn lexicon_views(
    state: &AppState,
    rules: &[(Rule, Pattern)],
    grants: &[LinkedRepo],
    instance: &TargetInfo,
) -> Result<Vec<LexiconView>, AppError> {
    let mut publications = store::list_publications(state).await?;
    let mut listings = publishable(state).await?;
    listings.sort_by(|a, b| a.id.cmp(&b.id));

    let mut views = Vec::with_capacity(listings.len());
    for listing in listings {
        let rule = rule_for(&listing.id, rules);
        let target = match rule {
            Some(rule) => Some(target::target_for_rule(state, rule, grants, instance).await),
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
    Ok(views)
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
