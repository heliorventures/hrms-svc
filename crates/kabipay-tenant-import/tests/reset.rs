use kabipay_tenant_import::reset::{deletion_order, foundational};
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
