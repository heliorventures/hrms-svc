//! FY2026-27 salary-only rules. Unsupported years/special-rate income require review.
//! Sources: Income-tax Act 2025 ss19,156,202,516, amended Finance Act2026.
pub const RULE_VERSION: &str = "IN-SALARY-FY2026-27-20261004";
pub const SOURCE:&str="https://www.incometaxindia.gov.in/documents/d/guest/income_tax_act_2025_as_amended_by_fa_act_2026-pdf";
pub const NEW_BANDS: &[(i64, i64)] = &[
    (400000, 0),
    (800000, 5),
    (1200000, 10),
    (1600000, 15),
    (2000000, 20),
    (2400000, 25),
    (1_000_000_000_000, 30),
];
