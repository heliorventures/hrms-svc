use kabipay_tenant_import::reset::{deletion_order, foundational};

#[test]
fn reviewed_no_backup_requires_a_reason_and_round_trips() {
    let mut value: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../hrms-database/import-templates/v1/operator-options.example.json"
    ))
    .unwrap();
    value["replacement_backup"] = serde_json::json!({
        "mode": "SKIP", "reason": "Operator approved pre-live clean reload"
    });
    let options: kabipay_tenant_import::options::ImportOptions =
        serde_json::from_value(value.clone()).unwrap();
    options.validate().unwrap();
    assert_eq!(
        serde_json::to_value(&options).unwrap()["replacement_backup"],
        value["replacement_backup"]
    );
    for reason in ["", "   ", "invalid\nreason"] {
        value["replacement_backup"]["reason"] = reason.into();
        let options: kabipay_tenant_import::options::ImportOptions =
            serde_json::from_value(value.clone()).unwrap();
        assert!(options.validate().is_err());
    }
}
#[test]
fn dependency_order_deletes_children_before_parents() {
    let tables = vec![
        "employee".into(),
        "payslip".into(),
        "payslip_component".into(),
    ];
    let links = vec![
        ("payslip".into(), "employee".into()),
        ("payslip_component".into(), "payslip".into()),
    ];
    assert_eq!(
        deletion_order(&tables, &links).unwrap(),
        vec!["payslip_component", "payslip", "employee"]
    );
}
#[test]
fn cycles_fail_without_cascade() {
    assert!(deletion_order(
        &["a".into(), "b".into()],
        &[("a".into(), "b".into()), ("b".into(), "a".into())]
    )
    .is_err());
}
#[test]
fn authorization_and_import_history_are_protected() {
    for name in [
        "permission",
        "role",
        "role_permission",
        "tenant_import_run",
        "tenant_import_record",
    ] {
        assert!(foundational(name, "tenant_fixture"));
    }
}

#[test]
fn nullable_dependencies_are_only_cleared_when_required_by_a_cycle() {
    use kabipay_tenant_import::{
        reset::ClearLink,
        reset_order::{self, Dependency},
    };
    let edge = |child: &str, parent: &str| Dependency {
        child: child.into(),
        parent: parent.into(),
        clear: Some(ClearLink {
            child: child.into(),
            parent: parent.into(),
            child_columns: vec!["parent_id".into()],
            parent_columns: vec!["id".into()],
            clear_columns: vec!["parent_id".into()],
            rows: 1,
        }),
    };
    let tables = vec!["parent".into(), "child".into()];
    let (order, clears) = reset_order::plan(&tables, vec![edge("child", "parent")]).unwrap();
    assert_eq!(order, vec!["child", "parent"]);
    assert!(clears.is_empty());
    let (_, clears) = reset_order::plan(
        &tables,
        vec![edge("child", "parent"), edge("parent", "child")],
    )
    .unwrap();
    assert_eq!(clears.len(), 1);
    assert!(reset_order::plan(
        &tables,
        vec![
            Dependency {
                child: "child".into(),
                parent: "parent".into(),
                clear: None
            },
            Dependency {
                child: "parent".into(),
                parent: "child".into(),
                clear: None
            }
        ]
    )
    .is_err());
}

#[test]
fn failure_evidence_does_not_expose_database_messages_or_assume_rollback() {
    let error = anyhow::Error::new(sea_orm::DbErr::Custom(
        "credential-and-employee-secret".into(),
    ));
    let evidence = serde_json::to_value(kabipay_tenant_import::failure::describe(&error)).unwrap();
    assert!(!evidence
        .to_string()
        .contains("credential-and-employee-secret"));
    assert_eq!(evidence["commit_status"], "UNCONFIRMED_CHECK_PERSISTED_RUN");
}
