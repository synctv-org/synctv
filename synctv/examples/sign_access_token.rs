use std::env;

use synctv_core::{models::UserId, service::JwtService};

fn main() -> anyhow::Result<()> {
    let secret = env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: sign_access_token <jwt-secret> <user-id>"))?;
    let user_id = env::args()
        .nth(2)
        .ok_or_else(|| anyhow::anyhow!("usage: sign_access_token <jwt-secret> <user-id>"))?
        .parse::<UserId>()
        .map_err(|error| anyhow::anyhow!(error))?;

    let jwt = JwtService::new(&secret)?;
    println!("{}", jwt.sign_access_token(&user_id, 0)?);
    Ok(())
}
