use kabipay_tax::domain::{TaxHistoryEntry, TaxSettingsInput};
use serde_json::json;

#[test]
fn database_decimal_padding_is_not_fractional_paise() {
    use kabipay_tax::domain::validate_amount;
    use rust_decimal::Decimal;
    for value in ["0.0000", "15000.0000", "123.4500"] {
        assert!(validate_amount(value.parse::<Decimal>().unwrap()).is_ok());
    }
    for value in ["0.0010", "123.4560", "-0.0100"] {
        assert!(validate_amount(value.parse::<Decimal>().unwrap()).is_err());
    }
}

fn settings() -> serde_json::Value {
    json!({"regime":"NEW","method":"PERCENTAGE_OVERRIDE","percentage":"0.10",
        "basis_components":["BASIC","HRA"],"effective_from":"2026-10-01",
        "effective_until":null,"reason":"Reviewed client withholding instruction"})
}

#[test]
fn override_requires_rate_basis_reason_and_valid_dates() {
    let input: TaxSettingsInput = serde_json::from_value(settings()).unwrap();
    assert!(input.validate().is_ok());
    for (field, bad) in [
        ("percentage", json!(null)),
        ("percentage", json!("1.01")),
        ("percentage", json!("-0.1")),
        ("basis_components", json!([])),
        ("reason", json!(" ")),
        ("effective_until", json!("2026-09-30")),
    ] {
        let mut value = settings();
        value[field] = bad;
        let input: TaxSettingsInput = serde_json::from_value(value).unwrap();
        assert!(input.validate().is_err(), "invalid {field} accepted");
    }
}

#[test]
fn history_preserves_unknown_and_confirmed_zero_tds() {
    let mut value = json!({"fiscal_year":2026,"period_start":"2026-04-01","period_end":"2026-08-31",
        "employer":"CURRENT","source_key":"opening-2026","earnings":"150000.00",
        "components":{"BASIC":"150000.00"},"tds":null,"coverage":"INCOMPLETE",
        "reason":"Client opening history","evidence":"IMPORTED_ACTUAL"});
    let unknown: TaxHistoryEntry = serde_json::from_value(value.clone()).unwrap();
    assert!(unknown.tds.is_none());
    assert!(unknown.validate().is_ok());
    value["tds"] = json!("0.00");
    value["coverage"] = json!("COMPLETE");
    let zero: TaxHistoryEntry = serde_json::from_value(value).unwrap();
    assert_eq!(zero.tds.unwrap().to_string(), "0.00");
    assert!(zero.validate().is_ok());
}

#[test]
fn history_rejects_projection_as_actual_and_out_of_year_dates() {
    let base = json!({"fiscal_year":2026,"period_start":"2026-04-01","period_end":"2026-08-31",
        "employer":"CURRENT","source_key":"opening","earnings":"100.00","components":{"BASIC":"100.00"},
        "tds":null,"coverage":"INCOMPLETE","reason":"Client supplied","evidence":"IMPORTED_ACTUAL"});
    for (field, bad) in [
        ("evidence", json!("HISTORICAL_ESTIMATE")),
        ("period_end", json!("2027-04-01")),
        ("coverage", json!("COMPLETE")),
        ("earnings", json!("-1")),
    ] {
        let mut value = base.clone();
        value[field] = bad;
        let input: TaxHistoryEntry = serde_json::from_value(value).unwrap();
        assert!(input.validate().is_err(), "invalid {field} accepted");
    }
}
