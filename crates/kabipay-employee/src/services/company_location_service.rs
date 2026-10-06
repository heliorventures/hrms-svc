use chrono::NaiveDate;
use kabipay_common::{tenant_business_clock::TenantBusinessClock, KabiPayError, KabiPayResult};

pub fn normalized_name(name: &str) -> KabiPayResult<String> {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() || name.chars().count() > 200 {
        return Err(KabiPayError::Validation(
            "location name must contain 1 to 200 characters".into(),
        ));
    }
    Ok(name)
}
pub fn validate_assignment_date(date: NaiveDate, clock: TenantBusinessClock) -> KabiPayResult<()> {
    if date != clock.now_date() {
        return Err(KabiPayError::Validation(
            "employee location changes must use today's company business date".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn company_location_normalizes_names_and_rejects_empty() {
        assert_eq!(
            normalized_name("  Pune   Office \t ").unwrap(),
            "Pune Office"
        );
        assert!(normalized_name(" \t ").is_err());
        assert!(normalized_name(&"a".repeat(201)).is_err());
    }
    #[test]
    fn company_location_assignment_requires_current_business_date() {
        let clock = TenantBusinessClock::from_name("Asia/Kolkata").unwrap();
        assert!(validate_assignment_date(clock.now_date(), clock).is_ok());
        assert!(validate_assignment_date(clock.now_date().succ_opt().unwrap(), clock).is_err());
        assert!(validate_assignment_date(clock.now_date().pred_opt().unwrap(), clock).is_err());
    }
}
