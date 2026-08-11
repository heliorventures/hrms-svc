use kabipay_common::{password, KabiPayError, KabiPayResult};

pub async fn verify(plaintext: String, stored_hash: String) -> KabiPayResult<bool> {
    tokio::task::spawn_blocking(move || password::verify(&plaintext, &stored_hash))
        .await
        .map_err(|error| {
            KabiPayError::Internal(format!("password verification task failed: {error}"))
        })?
}

pub async fn hash(plaintext: String) -> KabiPayResult<String> {
    tokio::task::spawn_blocking(move || password::hash(&plaintext))
        .await
        .map_err(|error| KabiPayError::Internal(format!("password hashing task failed: {error}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hash_and_verify_run_through_async_boundary() {
        let encoded = hash("correct horse battery staple".to_string())
            .await
            .unwrap();
        assert!(verify("correct horse battery staple".to_string(), encoded.clone())
            .await
            .unwrap());
        assert!(!verify("wrong".to_string(), encoded).await.unwrap());
    }
}
