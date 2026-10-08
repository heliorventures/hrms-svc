use rust_decimal::Decimal;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuestionType {
    SingleChoice,
    MultipleChoice,
    Rating,
    ShortText,
    LongText,
}

#[derive(Clone, Debug)]
pub struct QuestionRule {
    pub question_type: QuestionType,
    pub option_count: usize,
    pub rating_min: Option<Decimal>,
    pub rating_max: Option<Decimal>,
}

pub fn validate_question(rule: &QuestionRule) -> Result<(), String> {
    match rule.question_type {
        QuestionType::SingleChoice | QuestionType::MultipleChoice => {
            if rule.option_count < 2 {
                return Err("Choice questions require at least two options".into());
            }
            if rule.rating_min.is_some() || rule.rating_max.is_some() {
                return Err("Choice questions cannot define a rating range".into());
            }
        }
        QuestionType::Rating => {
            if rule.option_count != 0 {
                return Err("Rating questions cannot define options".into());
            }
            let (Some(minimum), Some(maximum)) = (rule.rating_min, rule.rating_max) else {
                return Err("Rating questions require minimum and maximum values".into());
            };
            if minimum >= maximum {
                return Err("Rating minimum must be less than maximum".into());
            }
        }
        QuestionType::ShortText | QuestionType::LongText => {
            if rule.option_count != 0 || rule.rating_min.is_some() || rule.rating_max.is_some() {
                return Err("Text questions cannot define options or rating ranges".into());
            }
        }
    }
    Ok(())
}

pub fn report_group_is_visible(response_count: usize, threshold: usize) -> bool {
    threshold >= 3 && response_count >= threshold
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choice_questions_require_options_and_text_questions_forbid_them() {
        assert!(validate_question(&QuestionRule { question_type: QuestionType::SingleChoice, option_count: 2, rating_min: None, rating_max: None }).is_ok());
        assert!(validate_question(&QuestionRule { question_type: QuestionType::MultipleChoice, option_count: 1, rating_min: None, rating_max: None }).is_err());
        assert!(validate_question(&QuestionRule { question_type: QuestionType::LongText, option_count: 1, rating_min: None, rating_max: None }).is_err());
    }

    #[test]
    fn rating_questions_require_an_ordered_range() {
        assert!(validate_question(&QuestionRule { question_type: QuestionType::Rating, option_count: 0, rating_min: Some(Decimal::ONE), rating_max: Some(Decimal::new(5, 0)) }).is_ok());
        assert!(validate_question(&QuestionRule { question_type: QuestionType::Rating, option_count: 0, rating_min: Some(Decimal::new(5, 0)), rating_max: Some(Decimal::ONE) }).is_err());
    }

    #[test]
    fn aggregate_visibility_is_fail_closed_and_never_below_three() {
        assert!(!report_group_is_visible(10, 2));
        assert!(!report_group_is_visible(4, 5));
        assert!(report_group_is_visible(5, 5));
    }
}
