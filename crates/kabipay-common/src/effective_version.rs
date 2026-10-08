//! A dated version replaces earlier starts; a revision corrects the same start.
//! An expired latest start is a configuration gap, never a fallback to old rules.
use chrono::NaiveDate;

pub fn effective<T>(
    values: impl IntoIterator<Item = T>,
    date: NaiveDate,
    key: impl Fn(&T) -> (NaiveDate, i32, Option<NaiveDate>),
) -> Option<T> {
    values
        .into_iter()
        .filter(|v| key(v).0 <= date)
        .max_by_key(|v| {
            let (start, revision, _) = key(v);
            (start, revision)
        })
        .filter(|v| key(v).2.is_none_or(|end| date <= end))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn backdated_correction_preserves_scheduled_start_and_expiry_gap() {
        let d = |s: &str| s.parse::<NaiveDate>().unwrap();
        let april = d("2026-04-01");
        let october = d("2026-10-01");
        let versions = vec![(april, 1, None), (october, 2, None), (april, 3, None)];
        assert_eq!(effective(versions.clone(), october, |v| *v).unwrap().1, 2);
        assert_eq!(
            effective(versions.clone(), d("2026-09-01"), |v| *v)
                .unwrap()
                .1,
            3
        );
        let mut versions = versions;
        versions.push((october, 4, Some(d("2026-10-31"))));
        assert!(effective(versions, d("2026-11-01"), |v| *v).is_none());
    }
}
