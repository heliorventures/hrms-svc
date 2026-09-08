//! Signed invitation capabilities and configured email delivery.
use super::*;
fn signing_key() -> Result<Vec<u8>> {
    let key =
        std::env::var("KABIPAY_PREJOINING_SIGNING_KEY").unwrap_or_default().into_bytes();
    if key.len() < 32 {
        return Err(Error::Internal("pre-joining signing key is not configured".into()));
    }
    Ok(key)
}
pub(super) fn sign(payload: &str, key: &[u8]) -> Result<String> {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(key).map_err(|_|
                    Error::Internal("invalid invitation key".into()))?;
    mac.update(payload.as_bytes());
    Ok(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}
pub fn digest(token: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes()))
}
pub fn issue_token(tenant: Uuid, id: Uuid) -> Result<String> {
    let mut random = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut random);
    let payload = format!("{tenant}.{id}.{}",URL_SAFE_NO_PAD.encode(random));
    Ok(format!("{payload}.{}",sign(&payload,&signing_key()?)?))
}
pub fn verify_token(token: &str) -> Result<(Uuid, Uuid)> {
    verify_token_with_key(token, &signing_key()?)
}
pub(super) fn verify_token_with_key(token: &str, key: &[u8])
    -> Result<(Uuid, Uuid)> {
    if token.len() > 300 { return Err(Error::Unauthorised); }
    let (payload, signature) =
        token.rsplit_once('.').ok_or(Error::Unauthorised)?;
    let signature =
        URL_SAFE_NO_PAD.decode(signature).map_err(|_| Error::Unauthorised)?;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(key).map_err(|_| Error::Unauthorised)?;
    mac.update(payload.as_bytes());
    mac.verify_slice(&signature).map_err(|_| Error::Unauthorised)?;
    let mut parts = payload.split('.');
    let tenant =
        parts.next().and_then(|v|
                        Uuid::parse_str(v).ok()).ok_or(Error::Unauthorised)?;
    let id =
        parts.next().and_then(|v|
                        Uuid::parse_str(v).ok()).ok_or(Error::Unauthorised)?;
    if parts.next().and_then(|v|
                        URL_SAFE_NO_PAD.decode(v).ok()).is_none_or(|v|
                    v.len() != 32) || parts.next().is_some() {
        return Err(Error::Unauthorised);
    }
    Ok((tenant, id))
}
pub fn check_invitation(row: &candidate::Model, token: &str,
    now: DateTime<Utc>) -> Result<()> {
    let actual = Sha256::digest(token.as_bytes());
    let expected =
        row.invitation_digest.as_ref().and_then(|v|
                        URL_SAFE_NO_PAD.decode(v).ok()).ok_or(Error::Unauthorised)?;
    // HMAC verification provides constant-time equality for the stored digest.
    let mut comparator =
        Hmac::<Sha256>::new_from_slice(b"prejoining-digest-compare").map_err(|_|
                    Error::Unauthorised)?;
    comparator.update(&expected);
    let mut actual_mac =
        Hmac::<Sha256>::new_from_slice(b"prejoining-digest-compare").map_err(|_|
                    Error::Unauthorised)?;
    actual_mac.update(&actual);
    comparator.verify_slice(&actual_mac.finalize().into_bytes()).map_err(|_|
                Error::Unauthorised)?;
    if matches!(row.status.as_str(),"JOINED"|"CANCELLED") ||
            row.expires_at.is_none_or(|v| v <= now) {
        return Err(Error::Unauthorised);
    }
    Ok(())
}
pub fn private_url(token: &str) -> Result<String> {
    let raw =
        std::env::var("KABIPAY_PREJOINING_PUBLIC_ORIGIN").map_err(|_|
                    Error::Internal("pre-joining public origin is not configured".into()))?;
    let url =
        reqwest::Url::parse(&raw).map_err(|_|
                    Error::Internal("invalid pre-joining public origin".into()))?;
    if url.scheme() != "https" || url.host_str().is_none() ||
                            !url.username().is_empty() || url.password().is_some() ||
                    url.query().is_some() || url.fragment().is_some() ||
            url.path() != "/" {
        return Err(Error::Internal("pre-joining public origin must be an HTTPS origin".into()));
    }
    Ok(format!("{}/prejoining#token={token}",url.as_str().trim_end_matches('/')))
}
pub async fn send_email(email: &str, url: &str) -> Result<()> {
    let endpoint =
        std::env::var("KABIPAY_PREJOINING_EMAIL_URL").map_err(|_|
                    validation("System email delivery is not configured. Copy the private link."))?;
    let endpoint =
        reqwest::Url::parse(&endpoint).map_err(|_|
                    validation("System email delivery is not configured. Copy the private link."))?;
    let key =
        std::env::var("KABIPAY_PREJOINING_EMAIL_TOKEN").map_err(|_|
                    validation("System email delivery is not configured. Copy the private link."))?;
    if endpoint.scheme() != "https" || key.trim().is_empty() ||
                !endpoint.username().is_empty() ||
            endpoint.password().is_some() {
        return Err(validation("System email delivery is not configured. Copy the private link."));
    }
    let body =
        json!({
                "to":email, "subject":"Your private pre-joining form",
                "text":format!("Complete your private pre-joining form: {url}\nKeep this link private. It grants access to your form and documents.")
            });
    let client =
        reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).redirect(reqwest::redirect::Policy::none()).build().map_err(|_|
                    validation("Email delivery failed. Copy the private link."))?;
    let response =
        client.post(endpoint).bearer_auth(key).header("Content-Type",
                                "application/json").body(body.to_string()).send().await.map_err(|_|
                    validation("Email delivery failed. Copy the private link."))?;
    if !response.status().is_success() {
        return Err(validation("Email delivery failed. Copy the private link."));
    }
    Ok(())
}
