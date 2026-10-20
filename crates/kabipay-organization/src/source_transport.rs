//! Invalid, interrupted or missing source confirmation is always an unknown
//! outcome. The source receipt and stable original command resolve retries.
use kabipay_organization_protocol::OrganizationCommandOutcome;
use serde_json::json;
use uuid::Uuid;
pub(crate) struct Confirmation {
    pub command_id: Uuid,
    pub resource_id: Uuid,
    pub expected_version: i64,
    pub receipt_read: bool,
}
pub(crate) fn awaiting(c: &Confirmation) -> OrganizationCommandOutcome {
    OrganizationCommandOutcome {
        command_id: c.command_id,
        resource_id: c.resource_id,
        status: "AWAITING_CONFIRMATION".into(),
        resource_version: c.expected_version,
        result: json!({"code":"SOURCE_CONFIRMATION_UNAVAILABLE"}),
    }
}
pub(crate) fn validate_outcome(
    bytes: &[u8],
    c: &Confirmation,
) -> Option<OrganizationCommandOutcome> {
    let out: OrganizationCommandOutcome = serde_json::from_slice(bytes).ok()?;
    (out.command_id == c.command_id
        && out.resource_id == c.resource_id
        && out.result.is_object()
        && out.resource_version >= c.expected_version
        && out.resource_version <= 9_007_199_254_740_991
        && [
            "ACCEPTED",
            "COMMITTED",
            "REJECTED",
            "CANCELLED_NOT_EXECUTED",
        ]
        .contains(&out.status.as_str()))
    .then_some(out)
}
pub(crate) async fn confirmation(
    response: Result<reqwest::Response, reqwest::Error>,
    c: &Confirmation,
) -> OrganizationCommandOutcome {
    match response {
        Ok(mut response) if response.status().is_success() => {
            let mut bytes = Vec::new();
            loop {
                match response.chunk().await {
                    Ok(Some(chunk)) if bytes.len() + chunk.len() <= 4 * 1024 * 1024 => {
                        bytes.extend_from_slice(&chunk)
                    }
                    Ok(None) => return validate_outcome(&bytes, c).unwrap_or_else(|| awaiting(c)),
                    _ => return awaiting(c),
                }
            }
        }
        Ok(response)
            if !c.receipt_read
                && matches!(
                    response.status().as_u16(),
                    400 | 401 | 403 | 404 | 409 | 422
                ) =>
        {
            OrganizationCommandOutcome {
                command_id: c.command_id,
                resource_id: c.resource_id,
                status: "REJECTED".into(),
                resource_version: c.expected_version,
                result: json!({"code":match response.status().as_u16(){401|403=>"AUTHORITY_CHANGED",409=>"SOURCE_VERSION_CHANGED",_=>"SOURCE_VALIDATION_FAILED"}}),
            }
        }
        _ => awaiting(c),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receipts_cannot_change_command_resource_or_version() {
        let c = Confirmation {
            command_id: Uuid::new_v4(),
            resource_id: Uuid::new_v4(),
            expected_version: 1,
            receipt_read: false,
        };
        let base = json!({"command_id":c.command_id,"resource_id":c.resource_id,"resource_version":2,"status":"COMMITTED","result":{}});
        assert!(validate_outcome(&serde_json::to_vec(&base).unwrap(), &c).is_some());
        for (field, value) in [
            ("command_id", json!(Uuid::new_v4())),
            ("resource_id", json!(Uuid::new_v4())),
            ("resource_version", json!(0)),
            ("resource_version", json!(-1)),
            ("status", json!("DELIVERED")),
        ] {
            let mut value_base = base.clone();
            value_base[field] = value;
            assert!(validate_outcome(&serde_json::to_vec(&value_base).unwrap(), &c).is_none());
        }
        let unknown = awaiting(&c);
        assert_eq!(unknown.command_id, c.command_id);
        assert_eq!(unknown.status, "AWAITING_CONFIRMATION");
    }
}
