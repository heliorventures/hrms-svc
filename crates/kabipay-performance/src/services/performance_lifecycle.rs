use std::collections::{HashMap, HashSet};

use chrono::{Datelike, Days, Months, NaiveDate};
use rust_decimal::Decimal;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cadence {
    Monthly,
    Quarterly,
    Yearly,
    Manual,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PerformancePeriod {
    pub key: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CycleStage {
    GoalSetting,
    SelfReview,
    ManagerReview,
    HrCalibration,
    EmployeeAcknowledgement,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuestionType {
    MultipleChoice,
    LongText,
    ShortText,
    Rating,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionRule {
    pub id: String,
    pub parent_id: Option<String>,
    pub question_type: QuestionType,
    pub option_count: usize,
}

pub fn period_for_date(cadence: Cadence, date: NaiveDate) -> PerformancePeriod {
    let (key, start_date, next_start) = match cadence {
        Cadence::Monthly => {
            let start = NaiveDate::from_ymd_opt(date.year(), date.month(), 1)
                .expect("a calendar month always has a first day");
            (
                format!("{:04}-{:02}", date.year(), date.month()),
                start,
                start.checked_add_months(Months::new(1))
                    .expect("supported dates have a following month"),
            )
        }
        Cadence::Quarterly => {
            let quarter = (date.month0() / 3) + 1;
            let start_month = ((quarter - 1) * 3) + 1;
            let start = NaiveDate::from_ymd_opt(date.year(), start_month, 1)
                .expect("a calendar quarter always has a first day");
            (
                format!("{}-Q{quarter}", date.year()),
                start,
                start.checked_add_months(Months::new(3))
                    .expect("supported dates have a following quarter"),
            )
        }
        Cadence::Yearly => {
            let start = NaiveDate::from_ymd_opt(date.year(), 1, 1)
                .expect("a calendar year always has a first day");
            (
                date.year().to_string(),
                start,
                NaiveDate::from_ymd_opt(date.year() + 1, 1, 1)
                    .expect("supported dates have a following year"),
            )
        }
        Cadence::Manual => {
            let next = date.checked_add_days(Days::new(1))
                .expect("supported dates have a following day");
            (format!("MANUAL-{date}"), date, next)
        }
    };
    PerformancePeriod {
        key,
        start_date,
        end_date: next_start
            .checked_sub_days(Days::new(1))
            .expect("a period has at least one day"),
    }
}

pub fn period_for_scheduled_date(cadence: Cadence, date: NaiveDate) -> Option<PerformancePeriod> {
    (cadence != Cadence::Manual).then(|| period_for_date(cadence, date))
}

pub fn next_stage(
    stage: CycleStage,
    include_calibration: bool,
    include_acknowledgement: bool,
) -> Option<CycleStage> {
    match stage {
        CycleStage::GoalSetting => Some(CycleStage::SelfReview),
        CycleStage::SelfReview => Some(CycleStage::ManagerReview),
        CycleStage::ManagerReview if include_calibration => Some(CycleStage::HrCalibration),
        CycleStage::ManagerReview if include_acknowledgement => {
            Some(CycleStage::EmployeeAcknowledgement)
        }
        CycleStage::ManagerReview => Some(CycleStage::Closed),
        CycleStage::HrCalibration if include_acknowledgement => {
            Some(CycleStage::EmployeeAcknowledgement)
        }
        CycleStage::HrCalibration => Some(CycleStage::Closed),
        CycleStage::EmployeeAcknowledgement => Some(CycleStage::Closed),
        CycleStage::Closed => None,
    }
}

pub fn validate_goal_weight_total(weights: &[Decimal]) -> Result<(), String> {
    if weights.is_empty() {
        return Err("At least one goal is required".into());
    }
    if weights.iter().any(Decimal::is_sign_negative) {
        return Err("Goal weights cannot be negative".into());
    }
    let total: Decimal = weights.iter().copied().sum();
    if total != Decimal::ONE_HUNDRED {
        return Err("Goal weights must total exactly 100".into());
    }
    Ok(())
}

pub fn validate_approved_goal_set(weights: &[Decimal], statuses: &[&str]) -> Result<(), String> {
    if weights.len() != statuses.len() || statuses.iter().any(|status| *status != "APPROVED") {
        return Err("Every goal must have a weight and manager approval".into());
    }
    validate_goal_weight_total(weights)
}

pub fn manager_matches_snapshot(
    actor_employee_id: uuid::Uuid,
    manager_id: Option<uuid::Uuid>,
) -> bool {
    manager_id == Some(actor_employee_id)
}

pub fn validate_questionnaire(questions: &[QuestionRule]) -> Result<(), String> {
    if questions.is_empty() {
        return Err("At least one appraisal question is required".into());
    }
    let mut parents = HashMap::new();
    let mut identifiers = HashSet::new();
    for question in questions {
        if question.id.trim().is_empty() || !identifiers.insert(question.id.as_str()) {
            return Err("Question identifiers must be non-empty and unique".into());
        }
        parents.insert(question.id.as_str(), question.parent_id.as_deref());
        match question.question_type {
            QuestionType::MultipleChoice if question.option_count < 2 => {
                return Err("Multiple-choice questions require at least two options".into());
            }
            QuestionType::MultipleChoice => {}
            _ if question.option_count != 0 => {
                return Err("Only multiple-choice questions may define options".into());
            }
            _ => {}
        }
    }
    for question in questions {
        if let Some(parent_id) = question.parent_id.as_deref() {
            let Some(parent_parent) = parents.get(parent_id) else {
                return Err("Every subquestion must reference a question in the template".into());
            };
            if parent_id == question.id || parent_parent.is_some() {
                return Err("Appraisal questions support only one subquestion level".into());
            }
        }
    }
    Ok(())
}

pub fn validate_rating(
    value: Decimal,
    minimum: Decimal,
    maximum: Decimal,
) -> Result<(), String> {
    if minimum > maximum {
        return Err("Rating range is invalid".into());
    }
    if value < minimum || value > maximum {
        return Err(format!("Rating must be between {minimum} and {maximum}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn derives_stable_calendar_periods() {
        let date = NaiveDate::from_ymd_opt(2026, 9, 8).unwrap();
        assert_eq!(period_for_date(Cadence::Monthly, date), PerformancePeriod {
            key: "2026-09".into(),
            start_date: NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
            end_date: NaiveDate::from_ymd_opt(2026, 9, 30).unwrap(),
        });
        assert_eq!(period_for_date(Cadence::Quarterly, date), PerformancePeriod {
            key: "2026-Q3".into(),
            start_date: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
            end_date: NaiveDate::from_ymd_opt(2026, 9, 30).unwrap(),
        });
        assert_eq!(period_for_date(Cadence::Yearly, date), PerformancePeriod {
            key: "2026".into(),
            start_date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            end_date: NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
        });
    }

    #[test]
    fn manual_programs_are_never_scheduled_automatically() {
        let date = NaiveDate::from_ymd_opt(2026, 9, 8).unwrap();
        assert_eq!(period_for_scheduled_date(Cadence::Manual, date), None);
        assert!(period_for_scheduled_date(Cadence::Monthly, date).is_some());
    }

    #[test]
    fn lifecycle_skips_optional_stages_without_reopening_closed_cycles() {
        assert_eq!(next_stage(CycleStage::GoalSetting, true, true), Some(CycleStage::SelfReview));
        assert_eq!(next_stage(CycleStage::SelfReview, true, true), Some(CycleStage::ManagerReview));
        assert_eq!(next_stage(CycleStage::ManagerReview, true, true), Some(CycleStage::HrCalibration));
        assert_eq!(next_stage(CycleStage::ManagerReview, false, true), Some(CycleStage::EmployeeAcknowledgement));
        assert_eq!(next_stage(CycleStage::ManagerReview, false, false), Some(CycleStage::Closed));
        assert_eq!(next_stage(CycleStage::HrCalibration, true, false), Some(CycleStage::Closed));
        assert_eq!(next_stage(CycleStage::Closed, true, true), None);
    }

    #[test]
    fn goal_weights_must_be_non_negative_and_total_exactly_one_hundred() {
        assert!(validate_goal_weight_total(&[Decimal::new(5000, 2), Decimal::new(5000, 2)]).is_ok());
        assert!(validate_goal_weight_total(&[Decimal::new(9999, 2)]).is_err());
        assert!(validate_goal_weight_total(&[Decimal::new(-1, 0), Decimal::new(101, 0)]).is_err());
    }

    #[test]
    fn cycle_cannot_leave_goal_setting_until_every_goal_is_approved() {
        let weights = [Decimal::new(5000, 2), Decimal::new(5000, 2)];
        assert!(validate_approved_goal_set(&weights, &["APPROVED", "APPROVED"]).is_ok());
        assert!(validate_approved_goal_set(&weights, &["APPROVED", "PROPOSED"]).is_err());
        assert!(validate_approved_goal_set(&[], &[]).is_err());
    }

    #[test]
    fn manager_authorization_uses_the_launch_snapshot() {
        let manager = Uuid::new_v4();
        assert!(manager_matches_snapshot(manager, Some(manager)));
        assert!(!manager_matches_snapshot(Uuid::new_v4(), Some(manager)));
        assert!(!manager_matches_snapshot(manager, None));
    }

    #[test]
    fn questionnaire_rejects_deep_nesting_and_invalid_multiple_choice() {
        let parent = QuestionRule { id: "parent".into(), parent_id: None, question_type: QuestionType::LongText, option_count: 0 };
        let child = QuestionRule { id: "child".into(), parent_id: Some("parent".into()), question_type: QuestionType::ShortText, option_count: 0 };
        let grandchild = QuestionRule { id: "grandchild".into(), parent_id: Some("child".into()), question_type: QuestionType::ShortText, option_count: 0 };
        assert!(validate_questionnaire(&[parent.clone(), child.clone()]).is_ok());
        assert!(validate_questionnaire(&[parent, child, grandchild]).is_err());
        assert!(validate_questionnaire(&[QuestionRule { id: "mcq".into(), parent_id: None, question_type: QuestionType::MultipleChoice, option_count: 1 }]).is_err());
    }

    #[test]
    fn rating_bounds_are_enforced() {
        assert!(validate_rating(Decimal::new(35, 1), Decimal::ONE, Decimal::new(5, 0)).is_ok());
        assert!(validate_rating(Decimal::ZERO, Decimal::ONE, Decimal::new(5, 0)).is_err());
        assert!(validate_rating(Decimal::new(51, 1), Decimal::ONE, Decimal::new(5, 0)).is_err());
    }
}
