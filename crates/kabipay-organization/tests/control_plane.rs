use axum::{http::HeaderMap, routing::post, Json, Router};
use kabipay_organization::{
    identity::IdentityClient,
    models::{Change, EmployeeScope, RoleCapability},
    store::Store,
};
use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
use std::collections::HashMap;
use uuid::Uuid;

async fn snapshot(db: &sea_orm::DatabaseConnection) -> serde_json::Value {
    db.query_one(Statement::from_string(DbBackend::Postgres,"SELECT jsonb_build_object('principal',(SELECT COUNT(*) FROM kabipay_ops.principal),'link',(SELECT COUNT(*) FROM kabipay_ops.principal_login_link),'org',(SELECT COUNT(*) FROM kabipay_ops.organization),'events',(SELECT COUNT(*) FROM kabipay_ops.organization_outbox)) AS state")).await.unwrap().unwrap().try_get("","state").unwrap()
}

#[tokio::test]
#[ignore = "requires scripts/test-organization-control.cjs disposable fixture"]
async fn control_plane_enforces_identity_scope_versions_pending_grants_and_atomic_history() {
    let url = std::env::var("KABIPAY_ORGANIZATION_FIXTURE_URL").expect("disposable fixture only");
    assert!(
        url.starts_with("postgres://organization_fixture:synthetic@127.0.0.1:")
            && url.ends_with("/organization_fixture?sslmode=disable")
    );
    let tenants: Vec<Uuid> =
        serde_json::from_str(&std::env::var("KABIPAY_ORGANIZATION_FIXTURE_TENANTS").unwrap())
            .unwrap();
    let db = Database::connect(&url).await.unwrap();
    db.query_one(Statement::from_string(
        DbBackend::Postgres,
        "SELECT id FROM kabipay_ops.organization_outbox LIMIT 1",
    ))
    .await
    .unwrap();
    let users: HashMap<Uuid, Uuid> = tenants.iter().map(|t| (*t, Uuid::new_v4())).collect();
    let alternate_user = Uuid::new_v4();
    let initial_tenant = tenants[0];
    let app=Router::new().route("/auth/client/organization-identity",post(move |headers:HeaderMap,Json(body):Json<serde_json::Value>| {let users=users.clone();async move {
        let token=headers["authorization"].to_str().unwrap().trim_start_matches("Bearer ");
        let (tenant,user)=if token=="alternate" {(initial_tenant,alternate_user)}else{let tenant=Uuid::parse_str(token).unwrap();(tenant,users[&tenant])};
        Json(serde_json::json!({"nonce":body["nonce"],"tenant_id":tenant,"user_id":user,"issuer":"kabipay-client","display_name":"Same name is not shared identity","permission_scopes":{"role:manage":"ALL"}}))
    }}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = IdentityClient::new(
        tenants
            .iter()
            .map(|id| {
                (
                    *id,
                    format!("http://{address}/auth/client/organization-identity"),
                )
            })
            .collect(),
    )
    .unwrap();
    let a = client
        .verify(tenants[0], &format!("Bearer {}", tenants[0]))
        .await
        .unwrap();
    let b = client
        .verify(tenants[1], &format!("Bearer {}", tenants[1]))
        .await
        .unwrap();
    let store = Store::new(db.clone());
    let pa = store.register(&a).await.unwrap();
    let pb = store.register(&b).await.unwrap();
    assert_ne!(pa, pb);
    assert_eq!(store.register(&a).await.unwrap(), pa);
    let org = store.bootstrap(&a, "Generic Group").await.unwrap();
    assert!(store.bootstrap(&a, "Duplicate occupancy").await.is_err());
    let alternate = client.verify(tenants[0], "Bearer alternate").await.unwrap();
    let before = snapshot(&db).await;
    assert!(store
        .bootstrap(&alternate, "Occupied company from fresh account")
        .await
        .is_err());
    assert_eq!(
        snapshot(&db).await,
        before,
        "ORG-SEC-01: failed bootstrap must not commit new identity or event"
    );
    let third = client
        .verify(tenants[2], &format!("Bearer {}", tenants[2]))
        .await
        .unwrap();
    db.execute(Statement::from_string(DbBackend::Postgres,"ALTER TABLE kabipay_ops.organization_outbox ADD CONSTRAINT fixture_reject_bootstrap CHECK(event_type<>'organization.bootstrapped') NOT VALID")).await.unwrap();
    assert!(store
        .bootstrap(&third, "Injected late failure")
        .await
        .is_err());
    assert_eq!(
        snapshot(&db).await,
        before,
        "late bootstrap failure must roll back registration too"
    );
    db.execute(Statement::from_string(
        DbBackend::Postgres,
        "ALTER TABLE kabipay_ops.organization_outbox DROP CONSTRAINT fixture_reject_bootstrap",
    ))
    .await
    .unwrap();
    assert!(store.organizations(&b).await.unwrap().is_empty());
    assert!(store
        .apply(&b, org.id, 1, Change::InviteCompany { tenant: tenants[2] })
        .await
        .is_err());
    let invite = store
        .apply(&a, org.id, 1, Change::InviteCompany { tenant: tenants[1] })
        .await
        .unwrap();
    assert!(store
        .apply(
            &a,
            org.id,
            2,
            Change::AcceptCompany {
                membership: invite.id,
                version: 1
            }
        )
        .await
        .is_err());
    let accepted = store
        .apply(
            &b,
            org.id,
            2,
            Change::AcceptCompany {
                membership: invite.id,
                version: 1,
            },
        )
        .await
        .unwrap();
    assert_eq!(accepted.organization_version, 3);
    let role = store
        .apply(
            &a,
            org.id,
            3,
            Change::CreateRole {
                name: "Payroll authority".into(),
                capabilities: vec![RoleCapability {
                    code: "payroll:manage".into(),
                    employee_scope: EmployeeScope::All,
                }],
            },
        )
        .await
        .unwrap();
    let grant = store
        .apply(
            &a,
            org.id,
            4,
            Change::GrantRole {
                principal: pb,
                role: role.id,
                companies: vec![invite.id],
            },
        )
        .await
        .unwrap();
    assert_eq!(grant.status, "PENDING");
    assert!(store.organizations(&b).await.unwrap().is_empty());
    let admin = store
        .apply(
            &a,
            org.id,
            5,
            Change::CreateRole {
                name: "Central administrator".into(),
                capabilities: vec![RoleCapability {
                    code: "organization:manage".into(),
                    employee_scope: EmployeeScope::All,
                }],
            },
        )
        .await
        .unwrap();
    let bg = store
        .apply(
            &a,
            org.id,
            6,
            Change::GrantRole {
                principal: pb,
                role: admin.id,
                companies: vec![invite.id],
            },
        )
        .await
        .unwrap();
    assert_eq!(bg.status, "ACTIVE");
    assert_eq!(store.organizations(&b).await.unwrap().len(), 1);
    use kabipay_organization::queries::OrganizationConfigurationKind as Kind;
    let page = store
        .configuration(&a, org.id, Kind::Roles, None, 1)
        .await
        .unwrap();
    assert_eq!(page.items.0.len(), 1);
    assert!(page.next_cursor.is_some());
    let next = store
        .configuration(&a, org.id, Kind::Roles, page.next_cursor, 1)
        .await
        .unwrap();
    assert_ne!(page.items.0[0]["id"], next.items.0[0]["id"]);
    assert_eq!(
        store
            .configuration(&b, org.id, Kind::Companies, None, 100)
            .await
            .unwrap()
            .items
            .0
            .len(),
        2,
        "central metadata administration is organization-wide"
    );
    assert!(store
        .configuration(&a, Uuid::new_v4(), Kind::Roles, None, 1)
        .await
        .is_err());
    assert!(store
        .configuration(&a, org.id, Kind::Roles, None, 101)
        .await
        .is_err());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_address = listener.local_addr().unwrap();
    let router = kabipay_organization::runtime::router(store.clone(), client.clone());
    let organization_http = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let http = reqwest::Client::new();
    let endpoint = format!("http://{http_address}/graphql");
    let query = serde_json::json!({"query":"{ myOrganizations { id } }"});
    let live = http
        .post(&endpoint)
        .header("authorization", format!("Bearer {}", tenants[0]))
        .header("x-login-company-id", tenants[0].to_string())
        .json(&query)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(live["data"]["myOrganizations"][0]["id"], org.id.to_string());
    let forged = http
        .post(&endpoint)
        .header("x-principal-id", pa.to_string())
        .header("x-organization-role", "administrator")
        .json(&query)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert!(forged["errors"]
        .as_array()
        .is_some_and(|errors| !errors.is_empty()));
    let substituted = http
        .post(&endpoint)
        .header("authorization", format!("Bearer {}", tenants[0]))
        .header("x-login-company-id", tenants[1].to_string())
        .json(&query)
        .send()
        .await
        .unwrap();
    assert_eq!(substituted.status(), reqwest::StatusCode::UNAUTHORIZED);
    organization_http.abort();
    assert!(store
        .apply(
            &b,
            org.id,
            7,
            Change::CreateRole {
                name: "Escalation".into(),
                capabilities: vec![RoleCapability {
                    code: "organization:delegate".into(),
                    employee_scope: EmployeeScope::All
                }]
            }
        )
        .await
        .is_err());
    assert!(store
        .apply(
            &a,
            org.id,
            6,
            Change::RevokeBinding {
                binding: grant.id,
                version: 1
            }
        )
        .await
        .is_err());
    let replaced = store
        .apply(
            &a,
            org.id,
            7,
            Change::ReplaceRole {
                role: role.id,
                version: 1,
                capabilities: vec![RoleCapability {
                    code: "leave:approve".into(),
                    employee_scope: EmployeeScope::Team,
                }],
            },
        )
        .await
        .unwrap();
    assert_eq!(replaced.version, 2);
    let scope=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT b.version,s.expected_grant_version,s.activation_status FROM kabipay_ops.principal_role_binding b JOIN kabipay_ops.role_binding_company s ON s.binding_id=b.id WHERE b.id=$1",[grant.id.into()])).await.unwrap().unwrap();
    assert_eq!(scope.try_get::<i64>("", "version").unwrap(), 2);
    assert_eq!(
        scope.try_get::<i64>("", "expected_grant_version").unwrap(),
        2
    );
    assert_eq!(
        scope.try_get::<String>("", "activation_status").unwrap(),
        "PENDING"
    );
    let revoked = store
        .apply(
            &a,
            org.id,
            8,
            Change::RevokeBinding {
                binding: grant.id,
                version: 2,
            },
        )
        .await
        .unwrap();
    assert_eq!(revoked.status, "REVOKED");
    let same_version = revoked.organization_version;
    let (x, y) = tokio::join!(
        store.apply(
            &a,
            org.id,
            same_version,
            Change::InviteCompany { tenant: tenants[2] }
        ),
        store.apply(
            &a,
            org.id,
            same_version,
            Change::InviteCompany { tenant: tenants[2] }
        )
    );
    assert_eq!(usize::from(x.is_ok()) + usize::from(y.is_ok()), 1);
    let count = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT COUNT(*) AS n FROM kabipay_ops.organization_outbox",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap();
    assert_eq!(
        count, 12,
        "registration, bootstrap and only successful changes produce history"
    );
    let suspended = store
        .apply(
            &a,
            org.id,
            same_version + 1,
            Change::SuspendCompany {
                membership: invite.id,
                version: 2,
            },
        )
        .await
        .unwrap();
    assert_eq!(suspended.status, "SUSPENDED");
    assert!(store.organizations(&b).await.unwrap().is_empty());
    assert!(store
        .apply(
            &b,
            org.id,
            suspended.organization_version,
            Change::InviteCompany { tenant: tenants[2] }
        )
        .await
        .is_err());
    let before = snapshot(&db).await;
    db.execute(Statement::from_string(DbBackend::Postgres,"ALTER TABLE kabipay_ops.organization_outbox ADD CONSTRAINT fixture_reject_changes CHECK(event_type<>'organization.configuration_changed') NOT VALID")).await.unwrap();
    assert!(store
        .apply(
            &a,
            org.id,
            suspended.organization_version,
            Change::CreateRole {
                name: "Must roll back".into(),
                capabilities: vec![RoleCapability {
                    code: "organization:manage".into(),
                    employee_scope: EmployeeScope::All
                }]
            }
        )
        .await
        .is_err());
    assert_eq!(snapshot(&db).await, before);
    let leftover = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT COUNT(*) AS n FROM kabipay_ops.organization_role WHERE name='Must roll back'",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap();
    assert_eq!(leftover, 0);
    assert_eq!(
        store.organizations(&a).await.unwrap()[0].version,
        suspended.organization_version
    );
    db.execute(Statement::from_string(
        DbBackend::Postgres,
        "ALTER TABLE kabipay_ops.organization_outbox DROP CONSTRAINT fixture_reject_changes",
    ))
    .await
    .unwrap();
    store.revoke_own_identity(&b, 1).await.unwrap();
    assert!(store.register(&b).await.is_err());
    assert!(store.organizations(&b).await.is_err());
    server.abort();
}
