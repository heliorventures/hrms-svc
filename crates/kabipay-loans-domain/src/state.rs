use crate::LoanDomainError;
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RequestState {
    Draft,
    Submitted,
    UnderReview,
    Returned,
    Approved,
    Rejected,
    Withdrawn,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum RequestAction {
    Submit,
    StartReview,
    Return,
    Approve,
    Reject,
    Withdraw,
}
pub fn transition_request(
    state: RequestState,
    action: RequestAction,
) -> Result<RequestState, LoanDomainError> {
    use RequestAction as A;
    use RequestState as S;
    Ok(match (state, action) {
        (S::Draft | S::Returned, A::Submit) => S::Submitted,
        (S::Submitted, A::StartReview) => S::UnderReview,
        (S::Submitted | S::UnderReview, A::Return) => S::Returned,
        (S::Submitted | S::UnderReview, A::Approve) => S::Approved,
        (S::Submitted | S::UnderReview, A::Reject) => S::Rejected,
        (S::Draft | S::Submitted | S::UnderReview | S::Returned, A::Withdraw) => S::Withdrawn,
        _ => return Err(LoanDomainError::InvalidTerms),
    })
}
