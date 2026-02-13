use anyhow::Result;

use nab::OnePasswordAuth;

pub fn cmd_auth(url: &str) -> Result<()> {
    if !OnePasswordAuth::is_available() {
        println!("❌ 1Password CLI not available or not authenticated");
        println!("   Run: op signin");
        return Ok(());
    }

    println!("🔐 Searching 1Password for: {url}");

    let auth = OnePasswordAuth::new(None);
    match auth.get_credential_for_url(url)? {
        Some(cred) => {
            println!("\n✅ Found credential:");
            println!("   Title: {}", cred.title);
            if let Some(ref username) = cred.username {
                println!("   Username: {username}");
            }
            if cred.password.is_some() {
                println!("   Password: [present]");
            }
            if let Some(ref totp) = cred.totp {
                println!("   TOTP: {totp}");
            }
            if let Some(ref passkey) = cred.passkey_credential_id {
                println!("   Passkey: {passkey}");
            }
        }
        None => {
            println!("\n❌ No credential found for this URL");
        }
    }

    Ok(())
}
