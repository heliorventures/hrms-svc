use kabipay_notification::email::{rotation::RotationMode, rotation_cli::parse_options};

fn arguments() -> Vec<String> {
    let root = std::env::temp_dir();
    [
        "--tenant".into(),
        "11111111-1111-4111-8111-111111111111".into(),
        "--expected-database".into(),
        "company_example".into(),
        "--schema".into(),
        "tenant_example".into(),
        "--expected-version".into(),
        "3".into(),
        "--current-key-id".into(),
        "v1".into(),
        "--new-key-id".into(),
        "v2".into(),
        "--database-url-file".into(),
        root.join("synthetic-url-file").to_str().unwrap().into(),
        "--keyring-file".into(),
        root.join("synthetic-keyring-file").to_str().unwrap().into(),
    ]
    .into()
}
#[test]
fn default_is_preview_and_apply_is_explicit() {
    let args = arguments();
    assert_eq!(
        parse_options(&args).unwrap().request.mode,
        RotationMode::Preview
    );
    let mut apply = args;
    apply.push("--apply".into());
    assert_eq!(
        parse_options(&apply).unwrap().request.mode,
        RotationMode::Apply
    );
}
#[test]
fn unknown_secret_duplicate_and_incomplete_flags_are_refused_without_echo() {
    for extra in [
        ["--database-url", "postgres://sensitive"],
        ["--password", "sensitive"],
        ["--tenant", "11111111-1111-4111-8111-111111111111"],
    ] {
        let mut args = arguments();
        args.extend(extra.map(str::to_owned));
        let error = parse_options(&args).unwrap_err();
        assert!(!format!("{error:?}").contains("sensitive"));
    }
    assert!(parse_options(&[]).is_err());
    let mut args = arguments();
    args.pop();
    assert!(parse_options(&args).is_err());
    let mut args = arguments();
    args.extend(["--apply".into(), "--apply".into()]);
    assert!(parse_options(&args).is_err());
    let mut args = arguments();
    let position = args
        .iter()
        .position(|value| value == "--keyring-file")
        .unwrap();
    args[position + 1] = "relative.json".into();
    assert!(parse_options(&args).is_err());
}
