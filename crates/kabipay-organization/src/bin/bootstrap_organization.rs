//! Run only under an operator-reviewed metadata database identity.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        anyhow::bail!("usage: bootstrap_organization <company-uuid> <organization-name>; token comes from KABIPAY_ORGANIZATION_BOOTSTRAP_TOKEN");
    }
    let tenant = args[0].parse::<uuid::Uuid>()?;
    let token = std::env::var("KABIPAY_ORGANIZATION_BOOTSTRAP_TOKEN")
        .map_err(|_| anyhow::anyhow!("bootstrap source token is required"))?;
    let identities = kabipay_organization::runtime::identity_client_from_env()?;
    let actor = identities
        .verify(tenant, &format!("Bearer {token}"))
        .await?;
    let store = kabipay_organization::runtime::store_from_env().await?;
    let result = store.bootstrap(&actor, &args[1]).await?;
    println!("organization_id={} version={}", result.id, result.version);
    Ok(())
}
