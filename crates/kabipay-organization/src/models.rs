use async_graphql::{Enum, InputObject, SimpleObject};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EmployeeScope {
    SelfOnly,
    Team,
    Department,
    All,
}
impl EmployeeScope {
    pub fn code(self) -> &'static str {
        match self {
            Self::SelfOnly => "SELF",
            Self::Team => "TEAM",
            Self::Department => "DEPARTMENT",
            Self::All => "ALL",
        }
    }
}
#[derive(Debug, Clone, InputObject, Serialize, Deserialize)]
pub struct RoleCapability {
    pub code: String,
    pub employee_scope: EmployeeScope,
}
#[derive(Debug, Clone, SimpleObject)]
pub struct Organization {
    pub id: Uuid,
    pub name: String,
    pub status: String,
    pub version: i64,
}
#[derive(Debug, Clone, SimpleObject)]
pub struct ChangeResult {
    pub id: Uuid,
    pub status: String,
    pub version: i64,
    pub organization_version: i64,
}
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum Change {
    InviteCompany {
        tenant: Uuid,
    },
    AcceptCompany {
        membership: Uuid,
        version: i64,
    },
    SuspendCompany {
        membership: Uuid,
        version: i64,
    },
    ResumeCompany {
        membership: Uuid,
        version: i64,
    },
    BeginDetachCompany {
        membership: Uuid,
        version: i64,
    },
    CreateRole {
        name: String,
        capabilities: Vec<RoleCapability>,
    },
    ReplaceRole {
        role: Uuid,
        version: i64,
        capabilities: Vec<RoleCapability>,
    },
    GrantRole {
        principal: Uuid,
        role: Uuid,
        companies: Vec<Uuid>,
    },
    RevokeBinding {
        binding: Uuid,
        version: i64,
    },
}
