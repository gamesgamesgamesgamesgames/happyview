use std::collections::HashSet;

use serde::{Deserialize, Serialize};

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
        ])
    }
}

/// Predefined permission templates.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Template {
    Viewer,
    Operator,
    Manager,
    FullAccess,
}

impl Template {
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
