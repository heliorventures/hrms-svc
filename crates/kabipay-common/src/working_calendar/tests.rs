use super::*;

#[test]
fn working_calendar_dated_transfer_inheritance_holiday_and_roster() {
    let employee = uuid::Uuid::new_v4();
    let first = uuid::Uuid::new_v4();
    let second = uuid::Uuid::new_v4();
    let default_id = uuid::Uuid::new_v4();
    let override_id = uuid::Uuid::new_v4();
    let date = |value: &str| value.parse::<chrono::NaiveDate>().unwrap();
    let mut snapshot = WorkingCalendarSnapshot {
        activation_date: Some(date("2026-10-01")),
        revision: Some(2),
        from_date: date("2026-09-30"),
        to_date: date("2026-10-31"),
        employees: [employee].into(),
        assignments: Default::default(),
        policies: Default::default(),
        holidays: Default::default(),
        roster_work: Default::default(),
    };
    snapshot.assignments.insert(
        employee,
        vec![
            LocationVersion {
                id: first,
                location_id: Some(first),
                effective_from: date("2026-10-01"),
                revision: 1,
            },
            LocationVersion {
                id: second,
                location_id: Some(second),
                effective_from: date("2026-10-20"),
                revision: 2,
            },
        ],
    );
    snapshot.policies.insert(
        None,
        vec![PolicyVersion {
            id: default_id,
            effective_from: date("2026-10-01"),
            inherits_default: false,
            rule: WeeklyOffRule {
                fixed_weekdays: vec![7],
                saturday_ordinals: vec![],
            },
        }],
    );
    snapshot.policies.insert(
        Some(first),
        vec![
            PolicyVersion {
                id: override_id,
                effective_from: date("2026-10-01"),
                inherits_default: false,
                rule: WeeklyOffRule {
                    fixed_weekdays: vec![7],
                    saturday_ordinals: vec![2, 4],
                },
            },
            PolicyVersion {
                id: uuid::Uuid::new_v4(),
                effective_from: date("2026-10-15"),
                inherits_default: true,
                rule: WeeklyOffRule::default(),
            },
        ],
    );
    snapshot.holidays.extend([
        (date("2026-10-02"), Some(second)),
        (date("2026-10-03"), None),
        (date("2026-10-21"), Some(second)),
    ]);
    assert!(
        !snapshot
            .day(employee, date("2026-09-30"))
            .unwrap()
            .activated
    );
    assert!(!snapshot.day(employee, date("2026-10-02")).unwrap().holiday);
    assert!(snapshot.day(employee, date("2026-10-03")).unwrap().holiday);
    assert_eq!(
        snapshot
            .day(employee, date("2026-10-10"))
            .unwrap()
            .policy_version_id,
        Some(override_id)
    );
    assert!(
        snapshot
            .day(employee, date("2026-10-10"))
            .unwrap()
            .weekly_off
    );
    snapshot.roster_work.insert((employee, date("2026-10-10")));
    assert!(
        !snapshot
            .day(employee, date("2026-10-10"))
            .unwrap()
            .weekly_off
    );
    assert_eq!(
        snapshot
            .day(employee, date("2026-10-17"))
            .unwrap()
            .policy_version_id,
        Some(default_id)
    );
    assert!(snapshot.day(employee, date("2026-10-21")).unwrap().holiday);
    assert!(
        !snapshot
            .day(employee, date("2026-10-24"))
            .unwrap()
            .weekly_off
    );
    assert!(snapshot
        .day(uuid::Uuid::new_v4(), date("2026-10-10"))
        .is_err());
    assert!(snapshot.day(employee, date("2026-11-01")).is_err());
    snapshot.assignments.clear();
    assert_eq!(
        snapshot
            .day(employee, date("2026-10-11"))
            .unwrap()
            .location_id,
        None
    );
    assert!(
        snapshot
            .day(employee, date("2026-10-11"))
            .unwrap()
            .weekly_off
    );
}

#[test]
fn working_calendar_second_fourth_saturday_october_2026() {
    let rule = WeeklyOffRule {
        fixed_weekdays: vec![7],
        saturday_ordinals: vec![2, 4],
    };
    for value in ["2026-10-10", "2026-10-24", "2026-10-11"] {
        assert!(
            is_weekly_off(value.parse().unwrap(), &rule),
            "{value} must be off"
        );
    }
    for value in ["2026-10-03", "2026-10-17", "2026-10-31"] {
        assert!(
            !is_weekly_off(value.parse().unwrap(), &rule),
            "{value} must be working"
        );
    }
}

#[test]
fn working_calendar_validates_ranges_duplicates_and_saturday_conflicts() {
    for rule in [
        WeeklyOffRule {
            fixed_weekdays: vec![0],
            saturday_ordinals: vec![],
        },
        WeeklyOffRule {
            fixed_weekdays: vec![8],
            saturday_ordinals: vec![],
        },
        WeeklyOffRule {
            fixed_weekdays: vec![7, 7],
            saturday_ordinals: vec![],
        },
        WeeklyOffRule {
            fixed_weekdays: vec![],
            saturday_ordinals: vec![0],
        },
        WeeklyOffRule {
            fixed_weekdays: vec![],
            saturday_ordinals: vec![6],
        },
        WeeklyOffRule {
            fixed_weekdays: vec![],
            saturday_ordinals: vec![2, 2],
        },
        WeeklyOffRule {
            fixed_weekdays: vec![6, 7],
            saturday_ordinals: vec![2, 4],
        },
    ] {
        assert!(
            validate_rule(&rule).is_err(),
            "invalid rule accepted: {rule:?}"
        );
    }
    assert!(validate_rule(&WeeklyOffRule::default()).is_ok());
}

#[test]
fn working_calendar_fifth_saturday_is_not_a_last_saturday_rule() {
    let rule = WeeklyOffRule {
        fixed_weekdays: vec![],
        saturday_ordinals: vec![5],
    };
    assert!(is_weekly_off("2026-10-31".parse().unwrap(), &rule));
    assert!(!is_weekly_off("2026-11-28".parse().unwrap(), &rule));
    assert!(is_weekly_off("2028-01-29".parse().unwrap(), &rule));
}

#[test]
fn working_calendar_leap_day_year_boundary_and_empty_rule() {
    let saturday = WeeklyOffRule {
        fixed_weekdays: vec![6],
        saturday_ordinals: vec![],
    };
    assert!(is_weekly_off("2020-02-29".parse().unwrap(), &saturday));
    let thursday = WeeklyOffRule {
        fixed_weekdays: vec![4],
        saturday_ordinals: vec![],
    };
    assert!(is_weekly_off("2026-12-31".parse().unwrap(), &thursday));
    assert!(!is_weekly_off("2027-01-01".parse().unwrap(), &thursday));
    for date in ["2026-10-11", "2026-10-31"] {
        assert!(!is_weekly_off(
            date.parse().unwrap(),
            &WeeklyOffRule::default()
        ));
    }
}
