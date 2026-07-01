use std::collections::HashSet;

use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub struct PermissionInfo {
    pub key: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub category: &'static str,
}

/// All permissions in the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Permission {
    #[serde(rename = "lexicons:create")]
    LexiconsCreate,
    #[serde(rename = "lexicons:read")]
    LexiconsRead,
    #[serde(rename = "lexicons:delete")]
    LexiconsDelete,

    #[serde(rename = "records:read")]
    RecordsRead,
    #[serde(rename = "records:delete")]
    RecordsDelete,
    #[serde(rename = "records:delete-collection")]
    RecordsDeleteCollection,

    #[serde(rename = "script-variables:create")]
    ScriptVariablesCreate,
    #[serde(rename = "script-variables:read")]
    ScriptVariablesRead,
    #[serde(rename = "script-variables:delete")]
    ScriptVariablesDelete,

    #[serde(rename = "users:create")]
    UsersCreate,
    #[serde(rename = "users:read")]
    UsersRead,
    #[serde(rename = "users:update")]
    UsersUpdate,
    #[serde(rename = "users:delete")]
    UsersDelete,

    #[serde(rename = "api-keys:create")]
    ApiKeysCreate,
    #[serde(rename = "api-keys:read")]
    ApiKeysRead,
    #[serde(rename = "api-keys:delete")]
    ApiKeysDelete,

    #[serde(rename = "backfill:create")]
    BackfillCreate,
    #[serde(rename = "backfill:read")]
    BackfillRead,

    #[serde(rename = "stats:read")]
    StatsRead,

    #[serde(rename = "events:read")]
    EventsRead,

    #[serde(rename = "labelers:create")]
    LabelersCreate,
    #[serde(rename = "labelers:read")]
    LabelersRead,
    #[serde(rename = "labelers:delete")]
    LabelersDelete,

    #[serde(rename = "settings:manage")]
    SettingsManage,

    #[serde(rename = "plugins:read")]
    PluginsRead,
    #[serde(rename = "plugins:create")]
    PluginsCreate,
    #[serde(rename = "plugins:delete")]
    PluginsDelete,

    #[serde(rename = "api-clients:view")]
    ApiClientsView,
    #[serde(rename = "api-clients:create")]
    ApiClientsCreate,
    #[serde(rename = "api-clients:edit")]
    ApiClientsEdit,
    #[serde(rename = "api-clients:delete")]
    ApiClientsDelete,

    #[serde(rename = "dead-letters:read")]
    DeadLettersRead,
    #[serde(rename = "dead-letters:manage")]
    DeadLettersManage,

    #[serde(rename = "spaces:create")]
    SpacesCreate,
    #[serde(rename = "spaces:read")]
    SpacesRead,
    #[serde(rename = "spaces:update")]
    SpacesUpdate,
    #[serde(rename = "spaces:delete")]
    SpacesDelete,
    #[serde(rename = "spaces:manage-members")]
    SpacesManageMembers,
    #[serde(rename = "spaces:manage-invites")]
    SpacesManageInvites,
    #[serde(rename = "spaces:manage-records")]
    SpacesManageRecords,
    #[serde(rename = "spaces:manage-credentials")]
    SpacesManageCredentials,

    #[serde(rename = "scripts:read")]
    ScriptsRead,
    #[serde(rename = "scripts:manage")]
    ScriptsManage,

    #[serde(rename = "jobs:read")]
    JobsRead,
    #[serde(rename = "jobs:create")]
    JobsCreate,
    #[serde(rename = "jobs:manage")]
    JobsManage,
}

impl Permission {
    /// String representation matching the DB values.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LexiconsCreate => "lexicons:create",
            Self::LexiconsRead => "lexicons:read",
            Self::LexiconsDelete => "lexicons:delete",
            Self::RecordsRead => "records:read",
            Self::RecordsDelete => "records:delete",
            Self::RecordsDeleteCollection => "records:delete-collection",
            Self::ScriptVariablesCreate => "script-variables:create",
            Self::ScriptVariablesRead => "script-variables:read",
            Self::ScriptVariablesDelete => "script-variables:delete",
            Self::UsersCreate => "users:create",
            Self::UsersRead => "users:read",
            Self::UsersUpdate => "users:update",
            Self::UsersDelete => "users:delete",
            Self::ApiKeysCreate => "api-keys:create",
            Self::ApiKeysRead => "api-keys:read",
            Self::ApiKeysDelete => "api-keys:delete",
            Self::BackfillCreate => "backfill:create",
            Self::BackfillRead => "backfill:read",
            Self::StatsRead => "stats:read",
            Self::EventsRead => "events:read",
            Self::LabelersCreate => "labelers:create",
            Self::LabelersRead => "labelers:read",
            Self::LabelersDelete => "labelers:delete",
            Self::SettingsManage => "settings:manage",
            Self::PluginsRead => "plugins:read",
            Self::PluginsCreate => "plugins:create",
            Self::PluginsDelete => "plugins:delete",
            Self::ApiClientsView => "api-clients:view",
            Self::ApiClientsCreate => "api-clients:create",
            Self::ApiClientsEdit => "api-clients:edit",
            Self::ApiClientsDelete => "api-clients:delete",
            Self::DeadLettersRead => "dead-letters:read",
            Self::DeadLettersManage => "dead-letters:manage",
            Self::SpacesCreate => "spaces:create",
            Self::SpacesRead => "spaces:read",
            Self::SpacesUpdate => "spaces:update",
            Self::SpacesDelete => "spaces:delete",
            Self::SpacesManageMembers => "spaces:manage-members",
            Self::SpacesManageInvites => "spaces:manage-invites",
            Self::SpacesManageRecords => "spaces:manage-records",
            Self::SpacesManageCredentials => "spaces:manage-credentials",
            Self::ScriptsRead => "scripts:read",
            Self::ScriptsManage => "scripts:manage",
            Self::JobsRead => "jobs:read",
            Self::JobsCreate => "jobs:create",
            Self::JobsManage => "jobs:manage",
        }
    }

    pub fn info(&self) -> PermissionInfo {
        match self {
            Self::LexiconsCreate => PermissionInfo {
                key: "lexicons:create",
                name: "Create Lexicons",
                description: "Upload and register new lexicon schemas",
                category: "Lexicons",
            },
            Self::LexiconsRead => PermissionInfo {
                key: "lexicons:read",
                name: "View Lexicons",
                description: "View registered lexicon schemas",
                category: "Lexicons",
            },
            Self::LexiconsDelete => PermissionInfo {
                key: "lexicons:delete",
                name: "Delete Lexicons",
                description: "Remove lexicon schemas",
                category: "Lexicons",
            },
            Self::RecordsRead => PermissionInfo {
                key: "records:read",
                name: "View Records",
                description: "Browse indexed AT Protocol records",
                category: "Records",
            },
            Self::RecordsDelete => PermissionInfo {
                key: "records:delete",
                name: "Delete Records",
                description: "Delete individual records from the index",
                category: "Records",
            },
            Self::RecordsDeleteCollection => PermissionInfo {
                key: "records:delete-collection",
                name: "Delete Collections",
                description: "Bulk-delete all records in a collection",
                category: "Records",
            },
            Self::ScriptVariablesCreate => PermissionInfo {
                key: "script-variables:create",
                name: "Create Script Variables",
                description: "Add or update environment variables for Lua scripts",
                category: "Script Variables",
            },
            Self::ScriptVariablesRead => PermissionInfo {
                key: "script-variables:read",
                name: "View Script Variables",
                description: "View script environment variable keys and values",
                category: "Script Variables",
            },
            Self::ScriptVariablesDelete => PermissionInfo {
                key: "script-variables:delete",
                name: "Delete Script Variables",
                description: "Remove script environment variables",
                category: "Script Variables",
            },
            Self::UsersCreate => PermissionInfo {
                key: "users:create",
                name: "Create Users",
                description: "Add new dashboard users",
                category: "Users",
            },
            Self::UsersRead => PermissionInfo {
                key: "users:read",
                name: "View Users",
                description: "View the user list and their permissions",
                category: "Users",
            },
            Self::UsersUpdate => PermissionInfo {
                key: "users:update",
                name: "Update Users",
                description: "Modify user permissions",
                category: "Users",
            },
            Self::UsersDelete => PermissionInfo {
                key: "users:delete",
                name: "Delete Users",
                description: "Remove dashboard users",
                category: "Users",
            },
            Self::ApiKeysCreate => PermissionInfo {
                key: "api-keys:create",
                name: "Create API Keys",
                description: "Generate new API keys for admin access",
                category: "API Keys",
            },
            Self::ApiKeysRead => PermissionInfo {
                key: "api-keys:read",
                name: "View API Keys",
                description: "View existing API keys",
                category: "API Keys",
            },
            Self::ApiKeysDelete => PermissionInfo {
                key: "api-keys:delete",
                name: "Revoke API Keys",
                description: "Revoke existing API keys",
                category: "API Keys",
            },
            Self::BackfillCreate => PermissionInfo {
                key: "backfill:create",
                name: "Start Backfill",
                description: "Trigger historical record backfill jobs",
                category: "Backfill",
            },
            Self::BackfillRead => PermissionInfo {
                key: "backfill:read",
                name: "View Backfill",
                description: "View backfill job status and progress",
                category: "Backfill",
            },
            Self::StatsRead => PermissionInfo {
                key: "stats:read",
                name: "View Stats",
                description: "View collection statistics and record counts",
                category: "System",
            },
            Self::EventsRead => PermissionInfo {
                key: "events:read",
                name: "View Events",
                description: "View the event log",
                category: "System",
            },
            Self::LabelersCreate => PermissionInfo {
                key: "labelers:create",
                name: "Add Labelers",
                description: "Subscribe to external labeler services",
                category: "Labelers",
            },
            Self::LabelersRead => PermissionInfo {
                key: "labelers:read",
                name: "View Labelers",
                description: "View subscribed labeler services",
                category: "Labelers",
            },
            Self::LabelersDelete => PermissionInfo {
                key: "labelers:delete",
                name: "Remove Labelers",
                description: "Unsubscribe from labeler services",
                category: "Labelers",
            },
            Self::SettingsManage => PermissionInfo {
                key: "settings:manage",
                name: "Manage Settings",
                description: "Modify instance settings, logo, and configuration",
                category: "Settings",
            },
            Self::PluginsRead => PermissionInfo {
                key: "plugins:read",
                name: "View Plugins",
                description: "View installed plugins and their configuration",
                category: "Plugins",
            },
            Self::PluginsCreate => PermissionInfo {
                key: "plugins:create",
                name: "Install Plugins",
                description: "Install and configure new plugins",
                category: "Plugins",
            },
            Self::PluginsDelete => PermissionInfo {
                key: "plugins:delete",
                name: "Remove Plugins",
                description: "Uninstall plugins",
                category: "Plugins",
            },
            Self::ApiClientsView => PermissionInfo {
                key: "api-clients:view",
                name: "View API Clients",
                description: "View registered OAuth API clients",
                category: "API Clients",
            },
            Self::ApiClientsCreate => PermissionInfo {
                key: "api-clients:create",
                name: "Create API Clients",
                description: "Register new OAuth API clients",
                category: "API Clients",
            },
            Self::ApiClientsEdit => PermissionInfo {
                key: "api-clients:edit",
                name: "Edit API Clients",
                description: "Modify API client settings and credentials",
                category: "API Clients",
            },
            Self::ApiClientsDelete => PermissionInfo {
                key: "api-clients:delete",
                name: "Delete API Clients",
                description: "Remove registered API clients",
                category: "API Clients",
            },
            Self::DeadLettersRead => PermissionInfo {
                key: "dead-letters:read",
                name: "View Dead Letters",
                description: "View failed hook executions",
                category: "Dead Letters",
            },
            Self::DeadLettersManage => PermissionInfo {
                key: "dead-letters:manage",
                name: "Manage Dead Letters",
                description: "Retry, re-index, or dismiss dead letters",
                category: "Dead Letters",
            },
            Self::SpacesCreate => PermissionInfo {
                key: "spaces:create",
                name: "Create Spaces",
                description: "Create new permissioned data spaces",
                category: "Spaces",
            },
            Self::SpacesRead => PermissionInfo {
                key: "spaces:read",
                name: "View Spaces",
                description: "View space details and metadata",
                category: "Spaces",
            },
            Self::SpacesUpdate => PermissionInfo {
                key: "spaces:update",
                name: "Update Spaces",
                description: "Modify space settings",
                category: "Spaces",
            },
            Self::SpacesDelete => PermissionInfo {
                key: "spaces:delete",
                name: "Delete Spaces",
                description: "Remove spaces and their data",
                category: "Spaces",
            },
            Self::SpacesManageMembers => PermissionInfo {
                key: "spaces:manage-members",
                name: "Manage Members",
                description: "Add or remove space members and roles",
                category: "Spaces",
            },
            Self::SpacesManageInvites => PermissionInfo {
                key: "spaces:manage-invites",
                name: "Manage Invites",
                description: "Create and revoke space invitations",
                category: "Spaces",
            },
            Self::SpacesManageRecords => PermissionInfo {
                key: "spaces:manage-records",
                name: "Manage Records",
                description: "Read and write records within spaces",
                category: "Spaces",
            },
            Self::SpacesManageCredentials => PermissionInfo {
                key: "spaces:manage-credentials",
                name: "Manage Credentials",
                description: "Issue and revoke space access credentials",
                category: "Spaces",
            },
            Self::ScriptsRead => PermissionInfo {
                key: "scripts:read",
                name: "View Scripts",
                description: "View trigger-keyed scripts",
                category: "Scripts",
            },
            Self::ScriptsManage => PermissionInfo {
                key: "scripts:manage",
                name: "Manage Scripts",
                description: "Create, update, and delete trigger-keyed scripts",
                category: "Scripts",
            },
            Self::JobsRead => PermissionInfo {
                key: "jobs:read",
                name: "View Jobs",
                description: "View background job status and progress",
                category: "Jobs",
            },
            Self::JobsCreate => PermissionInfo {
                key: "jobs:create",
                name: "Create Jobs",
                description: "Queue new background jobs",
                category: "Jobs",
            },
            Self::JobsManage => PermissionInfo {
                key: "jobs:manage",
                name: "Manage Jobs",
                description: "Cancel, pause, and resume background jobs",
                category: "Jobs",
            },
        }
    }

    /// All permissions.
    pub fn all() -> HashSet<Permission> {
        HashSet::from([
            Self::LexiconsCreate,
            Self::LexiconsRead,
            Self::LexiconsDelete,
            Self::RecordsRead,
            Self::RecordsDelete,
            Self::RecordsDeleteCollection,
            Self::ScriptVariablesCreate,
            Self::ScriptVariablesRead,
            Self::ScriptVariablesDelete,
            Self::UsersCreate,
            Self::UsersRead,
            Self::UsersUpdate,
            Self::UsersDelete,
            Self::ApiKeysCreate,
            Self::ApiKeysRead,
            Self::ApiKeysDelete,
            Self::BackfillCreate,
            Self::BackfillRead,
            Self::StatsRead,
            Self::EventsRead,
            Self::LabelersCreate,
            Self::LabelersRead,
            Self::LabelersDelete,
            Self::SettingsManage,
            Self::PluginsRead,
            Self::PluginsCreate,
            Self::PluginsDelete,
            Self::ApiClientsView,
            Self::ApiClientsCreate,
            Self::ApiClientsEdit,
            Self::ApiClientsDelete,
            Self::DeadLettersRead,
            Self::DeadLettersManage,
            Self::SpacesCreate,
            Self::SpacesRead,
            Self::SpacesUpdate,
            Self::SpacesDelete,
            Self::SpacesManageMembers,
            Self::SpacesManageInvites,
            Self::SpacesManageRecords,
            Self::SpacesManageCredentials,
            Self::ScriptsRead,
            Self::ScriptsManage,
            Self::JobsRead,
            Self::JobsCreate,
            Self::JobsManage,
        ])
    }
}

/// Ordered list of all permissions with metadata.
pub fn catalog() -> Vec<PermissionInfo> {
    use Permission::*;
    [
        LexiconsCreate,
        LexiconsRead,
        LexiconsDelete,
        RecordsRead,
        RecordsDelete,
        RecordsDeleteCollection,
        ScriptsRead,
        ScriptsManage,
        ScriptVariablesCreate,
        ScriptVariablesRead,
        ScriptVariablesDelete,
        UsersCreate,
        UsersRead,
        UsersUpdate,
        UsersDelete,
        ApiKeysCreate,
        ApiKeysRead,
        ApiKeysDelete,
        BackfillCreate,
        BackfillRead,
        StatsRead,
        EventsRead,
        LabelersCreate,
        LabelersRead,
        LabelersDelete,
        SettingsManage,
        PluginsRead,
        PluginsCreate,
        PluginsDelete,
        ApiClientsView,
        ApiClientsCreate,
        ApiClientsEdit,
        ApiClientsDelete,
        DeadLettersRead,
        DeadLettersManage,
        SpacesCreate,
        SpacesRead,
        SpacesUpdate,
        SpacesDelete,
        SpacesManageMembers,
        SpacesManageInvites,
        SpacesManageRecords,
        SpacesManageCredentials,
        JobsRead,
        JobsCreate,
        JobsManage,
    ]
    .iter()
    .map(|p| p.info())
    .collect()
}

/// Check whether a permission string is recognized.
#[allow(dead_code)]
pub fn is_valid(key: &str) -> bool {
    serde_json::from_value::<Permission>(serde_json::Value::String(key.to_string())).is_ok()
}

/// Predefined permission templates.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Template {
    Viewer,
    Operator,
    Manager,
    FullAccess,
}

#[derive(Serialize)]
pub struct TemplateInfo {
    pub key: String,
    pub label: &'static str,
    pub permissions: Vec<&'static str>,
}

impl Template {
    pub const ALL: &[Template] = &[
        Template::Viewer,
        Template::Operator,
        Template::Manager,
        Template::FullAccess,
    ];

    pub fn key(&self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Operator => "operator",
            Self::Manager => "manager",
            Self::FullAccess => "full_access",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Viewer => "Viewer",
            Self::Operator => "Operator",
            Self::Manager => "Manager",
            Self::FullAccess => "Full Access",
        }
    }

    pub fn info(&self) -> TemplateInfo {
        TemplateInfo {
            key: self.key().to_string(),
            label: self.label(),
            permissions: self.permissions().iter().map(|p| p.as_str()).collect(),
        }
    }

    pub fn permissions(&self) -> HashSet<Permission> {
        match self {
            Self::Viewer => HashSet::from([
                Permission::LexiconsRead,
                Permission::RecordsRead,
                Permission::ScriptVariablesRead,
                Permission::ScriptsRead,
                Permission::UsersRead,
                Permission::ApiKeysRead,
                Permission::BackfillRead,
                Permission::StatsRead,
                Permission::EventsRead,
                Permission::DeadLettersRead,
            ]),
            Self::Operator => {
                let mut perms = Self::Viewer.permissions();
                perms.insert(Permission::BackfillCreate);
                perms.insert(Permission::ApiKeysCreate);
                perms.insert(Permission::ApiKeysDelete);
                perms.insert(Permission::DeadLettersManage);
                perms
            }
            Self::Manager => {
                let mut perms = Self::Operator.permissions();
                perms.insert(Permission::LexiconsCreate);
                perms.insert(Permission::LexiconsDelete);
                perms.insert(Permission::ScriptVariablesCreate);
                perms.insert(Permission::ScriptVariablesDelete);
                perms.insert(Permission::ScriptsManage);
                perms.insert(Permission::RecordsDelete);
                perms.insert(Permission::LabelersCreate);
                perms.insert(Permission::LabelersRead);
                perms.insert(Permission::LabelersDelete);
                perms.insert(Permission::SettingsManage);
                perms.insert(Permission::PluginsRead);
                perms.insert(Permission::PluginsCreate);
                perms.insert(Permission::PluginsDelete);
                perms.insert(Permission::ApiClientsView);
                perms.insert(Permission::ApiClientsCreate);
                perms.insert(Permission::ApiClientsEdit);
                perms.insert(Permission::ApiClientsDelete);
                perms.insert(Permission::SpacesCreate);
                perms.insert(Permission::SpacesRead);
                perms.insert(Permission::SpacesUpdate);
                perms.insert(Permission::SpacesDelete);
                perms.insert(Permission::SpacesManageMembers);
                perms.insert(Permission::SpacesManageInvites);
                perms.insert(Permission::SpacesManageRecords);
                perms.insert(Permission::SpacesManageCredentials);
                perms
            }
            Self::FullAccess => Permission::all(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_covers_all_permissions() {
        let catalog_keys: Vec<&str> = catalog().iter().map(|p| p.key).collect();
        for perm in Permission::all() {
            assert!(
                catalog_keys.contains(&perm.as_str()),
                "Permission {} missing from catalog()",
                perm.as_str()
            );
        }
    }

    #[test]
    fn catalog_has_no_duplicates() {
        let entries = catalog();
        let mut seen = std::collections::HashSet::new();
        for entry in &entries {
            assert!(
                seen.insert(entry.key),
                "Duplicate key in catalog: {}",
                entry.key
            );
        }
    }

    #[test]
    fn info_key_matches_as_str() {
        for perm in Permission::all() {
            assert_eq!(perm.info().key, perm.as_str());
        }
    }

    #[test]
    fn info_fields_are_nonempty() {
        for perm in Permission::all() {
            let info = perm.info();
            assert!(!info.name.is_empty(), "{} has empty name", info.key);
            assert!(
                !info.description.is_empty(),
                "{} has empty description",
                info.key
            );
            assert!(!info.category.is_empty(), "{} has empty category", info.key);
        }
    }

    #[test]
    fn is_valid_accepts_known_permissions() {
        assert!(is_valid("lexicons:create"));
        assert!(is_valid("spaces:manage-members"));
    }

    #[test]
    fn is_valid_rejects_unknown_permissions() {
        assert!(!is_valid("fake:permission"));
        assert!(!is_valid(""));
    }

    #[test]
    fn template_full_access_covers_all() {
        assert_eq!(Template::FullAccess.permissions(), Permission::all());
    }

    #[test]
    fn template_viewer_is_subset_of_operator() {
        let viewer = Template::Viewer.permissions();
        let operator = Template::Operator.permissions();
        assert!(viewer.is_subset(&operator));
    }

    #[test]
    fn template_operator_is_subset_of_manager() {
        let operator = Template::Operator.permissions();
        let manager = Template::Manager.permissions();
        assert!(operator.is_subset(&manager));
    }

    #[test]
    fn template_info_permissions_match_template_permissions() {
        for t in Template::ALL {
            let info = t.info();
            let expected: HashSet<&str> = t.permissions().iter().map(|p| p.as_str()).collect();
            let actual: HashSet<&str> = info.permissions.into_iter().collect();
            assert_eq!(expected, actual, "Template {:?} info mismatch", t);
        }
    }

    #[test]
    fn spaces_permissions_are_in_spaces_category() {
        for entry in catalog() {
            if entry.key.starts_with("spaces:") {
                assert_eq!(
                    entry.category, "Spaces",
                    "{} should be in Spaces category",
                    entry.key
                );
            }
        }
    }
}
