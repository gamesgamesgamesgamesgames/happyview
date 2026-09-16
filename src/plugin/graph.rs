//! Dependency graph over installed plugins. Pure functions, no I/O.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use semver::{Version, VersionReq};

use crate::plugin::{LoadedPlugin, PluginDependency};

/// The graph's view of a plugin: identity, version, and declared edges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginNode {
    pub id: String,
    pub version: String,
    pub dependencies: Vec<PluginDependency>,
}

impl From<&LoadedPlugin> for PluginNode {
    fn from(p: &LoadedPlugin) -> Self {
        Self {
            id: p.info.id.clone(),
            version: p.info.version.clone(),
            dependencies: p.dependencies().to_vec(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GraphError {
    #[error("plugin '{plugin}' depends on '{dependency}', which is not installed")]
    MissingDependency { plugin: String, dependency: String },
    #[error("plugin '{plugin}' requires '{dependency}' {required}, but {installed} is installed")]
    VersionMismatch {
        plugin: String,
        dependency: String,
        required: String,
        installed: String,
    },
    #[error("circular dependency: {}", .0.join(" -> "))]
    Cycle(Vec<String>),
    #[error("plugin '{plugin}' is required by: {}", .dependents.join(", "))]
    HasDependents {
        plugin: String,
        dependents: Vec<String>,
    },
    #[error(
        "plugin '{plugin}' declares an invalid version requirement '{requirement}' for '{dependency}'"
    )]
    InvalidRequirement {
        plugin: String,
        dependency: String,
        requirement: String,
    },
    #[error("namespace '{namespace}' is already provided by plugin '{by}'")]
    NamespaceTaken { namespace: String, by: String },
}

fn check_edge(
    from: &PluginNode,
    dep: &PluginDependency,
    to: &PluginNode,
) -> Result<(), GraphError> {
    let req = VersionReq::parse(&dep.version).map_err(|_| GraphError::InvalidRequirement {
        plugin: from.id.clone(),
        dependency: dep.id.clone(),
        requirement: dep.version.clone(),
    })?;
    let satisfied = Version::parse(&to.version)
        .map(|v| req.matches(&v))
        .unwrap_or(false);
    if satisfied {
        Ok(())
    } else {
        Err(GraphError::VersionMismatch {
            plugin: from.id.clone(),
            dependency: dep.id.clone(),
            required: dep.version.clone(),
            installed: to.version.clone(),
        })
    }
}

/// Can `candidate` be installed into (or replace its namesake in) `installed`?
/// Checks the candidate's own edges, the edges of everything that already
/// depends on it (an upgrade must not break dependents), and acyclicity.
pub fn validate_install(
    installed: &[PluginNode],
    candidate: &PluginNode,
) -> Result<(), GraphError> {
    let mut nodes: Vec<PluginNode> = installed
        .iter()
        .filter(|n| n.id != candidate.id)
        .cloned()
        .collect();
    nodes.push(candidate.clone());
    let by_id: HashMap<&str, &PluginNode> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    for dep in &candidate.dependencies {
        let target = by_id
            .get(dep.id.as_str())
            .ok_or_else(|| GraphError::MissingDependency {
                plugin: candidate.id.clone(),
                dependency: dep.id.clone(),
            })?;
        check_edge(candidate, dep, target)?;
    }

    for node in nodes.iter().filter(|n| n.id != candidate.id) {
        for dep in node.dependencies.iter().filter(|d| d.id == candidate.id) {
            check_edge(node, dep, candidate)?;
        }
    }

    load_order(&nodes).map(|_| ())
}

/// Direct dependents of `id`, sorted.
fn direct_dependents(installed: &[PluginNode], id: &str) -> Vec<String> {
    let mut out: Vec<String> = installed
        .iter()
        .filter(|n| n.dependencies.iter().any(|d| d.id == id))
        .map(|n| n.id.clone())
        .collect();
    out.sort();
    out
}

/// Removal is refused while anything depends on `id`.
pub fn validate_remove(installed: &[PluginNode], id: &str) -> Result<(), GraphError> {
    let dependents = direct_dependents(installed, id);
    if dependents.is_empty() {
        Ok(())
    } else {
        Err(GraphError::HasDependents {
            plugin: id.to_string(),
            dependents,
        })
    }
}

/// Everything that transitively depends on `id`, ordered so that each entry
/// precedes anything it depends on — i.e. safe to remove front to back.
pub fn transitive_dependents(installed: &[PluginNode], id: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut frontier = vec![id.to_string()];
    while let Some(current) = frontier.pop() {
        for dep in direct_dependents(installed, &current) {
            if seen.insert(dep.clone()) {
                frontier.push(dep);
            }
        }
    }
    let subset: Vec<PluginNode> = installed
        .iter()
        .filter(|n| seen.contains(&n.id))
        .cloned()
        .collect();
    // load_order is leaves-first; removal wants the reverse.
    let mut order = load_order(&subset).unwrap_or_else(|_| seen.iter().cloned().collect());
    order.reverse();
    order
}

/// Topological order, leaves first, ties broken alphabetically. Edges to
/// ids not present in `nodes` are ignored — ordering is not validation.
pub fn load_order(nodes: &[PluginNode]) -> Result<Vec<String>, GraphError> {
    let ids: BTreeSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let mut in_degree: BTreeMap<&str, usize> = ids.iter().map(|id| (*id, 0)).collect();
    let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for node in nodes {
        for dep in &node.dependencies {
            if ids.contains(dep.id.as_str()) {
                *in_degree.get_mut(node.id.as_str()).unwrap() += 1;
                dependents
                    .entry(dep.id.as_str())
                    .or_default()
                    .push(node.id.as_str());
            }
        }
    }

    let mut ready: BTreeSet<&str> = in_degree
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(id, _)| *id)
        .collect();
    let mut order = Vec::with_capacity(nodes.len());
    while let Some(id) = ready.iter().next().copied() {
        ready.remove(id);
        order.push(id.to_string());
        for dependent in dependents.get(id).into_iter().flatten() {
            let d = in_degree.get_mut(dependent).unwrap();
            *d -= 1;
            if *d == 0 {
                ready.insert(dependent);
            }
        }
    }

    if order.len() == nodes.len() {
        return Ok(order);
    }

    // Something is left: walk one cycle so the error names it.
    let remaining: BTreeSet<&str> = in_degree
        .iter()
        .filter(|(_, d)| **d > 0)
        .map(|(id, _)| *id)
        .collect();
    let by_id: HashMap<&str, &PluginNode> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let start = *remaining.iter().next().unwrap();
    let mut path = vec![start];
    let mut current = start;
    loop {
        let next = by_id[current]
            .dependencies
            .iter()
            .map(|d| d.id.as_str())
            .find(|d| remaining.contains(d))
            .unwrap_or(start);
        if let Some(pos) = path.iter().position(|p| *p == next) {
            let mut cycle: Vec<String> = path[pos..].iter().map(|s| s.to_string()).collect();
            cycle.push(next.to_string());
            return Err(GraphError::Cycle(cycle));
        }
        path.push(next);
        current = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::PluginDependency;

    fn node(id: &str, version: &str, deps: &[(&str, &str)]) -> PluginNode {
        PluginNode {
            id: id.into(),
            version: version.into(),
            dependencies: deps
                .iter()
                .map(|(id, v)| PluginDependency {
                    id: (*id).into(),
                    version: (*v).into(),
                })
                .collect(),
        }
    }

    #[test]
    fn install_with_satisfied_dependency_passes() {
        let installed = [node("db", "1.4.0", &[])];
        let candidate = node("record", "1.0.0", &[("db", ">=1.0.0")]);
        assert_eq!(validate_install(&installed, &candidate), Ok(()));
    }

    #[test]
    fn install_with_missing_dependency_fails_naming_it() {
        let candidate = node("record", "1.0.0", &[("db", "*")]);
        let err = validate_install(&[], &candidate).unwrap_err();
        assert_eq!(
            err,
            GraphError::MissingDependency {
                plugin: "record".into(),
                dependency: "db".into()
            }
        );
        assert!(err.to_string().contains("db"));
    }

    #[test]
    fn install_with_wrong_version_fails() {
        let installed = [node("db", "0.9.0", &[])];
        let candidate = node("record", "1.0.0", &[("db", ">=1.0.0")]);
        assert!(matches!(
            validate_install(&installed, &candidate),
            Err(GraphError::VersionMismatch { ref dependency, .. }) if dependency == "db"
        ));
    }

    #[test]
    fn upgrading_a_dependency_out_of_range_fails_naming_the_dependent() {
        let installed = [
            node("db", "1.0.0", &[]),
            node("record", "1.0.0", &[("db", "^1")]),
        ];
        let candidate = node("db", "2.0.0", &[]);
        assert!(matches!(
            validate_install(&installed, &candidate),
            Err(GraphError::VersionMismatch { ref plugin, .. }) if plugin == "record"
        ));
    }

    #[test]
    fn install_detects_cycle() {
        let installed = [node("a", "1.0.0", &[("b", "*")]), node("b", "1.0.0", &[])];
        // b is re-installed with a dependency on a: a -> b -> a
        let candidate = node("b", "1.0.1", &[("a", "*")]);
        let err = validate_install(&installed, &candidate).unwrap_err();
        assert!(matches!(err, GraphError::Cycle(_)), "{err}");
        assert!(err.to_string().contains("a -> b") || err.to_string().contains("b -> a"));
    }

    #[test]
    fn remove_is_blocked_by_dependents() {
        let installed = [
            node("db", "1.0.0", &[]),
            node("record", "1.0.0", &[("db", "*")]),
            node("xrpc", "1.0.0", &[("record", "*")]),
        ];
        assert_eq!(
            validate_remove(&installed, "db"),
            Err(GraphError::HasDependents {
                plugin: "db".into(),
                dependents: vec!["record".into()]
            })
        );
        assert_eq!(validate_remove(&installed, "xrpc"), Ok(()));
    }

    #[test]
    fn transitive_dependents_are_ordered_dependents_first() {
        let installed = [
            node("db", "1.0.0", &[]),
            node("record", "1.0.0", &[("db", "*")]),
            node("xrpc", "1.0.0", &[("record", "*")]),
            node("http", "1.0.0", &[]),
        ];
        assert_eq!(
            transitive_dependents(&installed, "db"),
            vec!["xrpc", "record"]
        );
        assert!(transitive_dependents(&installed, "http").is_empty());
    }

    #[test]
    fn load_order_puts_leaves_first() {
        let nodes = [
            node("xrpc", "1.0.0", &[("record", "*"), ("atproto", "*")]),
            node("record", "1.0.0", &[("db", "*")]),
            node("atproto", "1.0.0", &[("http", "*")]),
            node("db", "1.0.0", &[]),
            node("http", "1.0.0", &[]),
            node("lua", "1.0.0", &[]),
        ];
        let order = load_order(&nodes).unwrap();
        let pos = |id: &str| order.iter().position(|x| x == id).unwrap();
        assert!(pos("db") < pos("record"));
        assert!(pos("http") < pos("atproto"));
        assert!(pos("record") < pos("xrpc"));
        assert!(pos("atproto") < pos("xrpc"));
        assert_eq!(order.len(), 6);
    }

    #[test]
    fn load_order_is_deterministic() {
        let nodes = [node("b", "1.0.0", &[]), node("a", "1.0.0", &[])];
        assert_eq!(load_order(&nodes).unwrap(), vec!["a", "b"]);
    }

    #[test]
    fn load_order_reports_cycle() {
        let nodes = [
            node("a", "1.0.0", &[("b", "*")]),
            node("b", "1.0.0", &[("c", "*")]),
            node("c", "1.0.0", &[("a", "*")]),
        ];
        assert!(matches!(load_order(&nodes), Err(GraphError::Cycle(_))));
    }

    #[test]
    fn load_order_ignores_missing_dependencies() {
        // Ordering is not validation: a node whose dependency is absent still gets a slot.
        let nodes = [node("record", "1.0.0", &[("db", "*")])];
        assert_eq!(load_order(&nodes).unwrap(), vec!["record"]);
    }
}
