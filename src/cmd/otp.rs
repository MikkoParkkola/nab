use anyhow::Result;

use nab::OtpRetriever;

pub fn cmd_otp(domain: &str) -> Result<()> {
    println!("🔐 Searching for OTP codes for: {domain}\n");

    // Extract domain from URL if needed
    let clean_domain = url::Url::parse(domain)
        .ok()
        .and_then(|u| u.host_str().map(std::string::ToString::to_string))
        .unwrap_or_else(|| domain.to_string());

    if let Some(otp) = OtpRetriever::get_otp_for_domain(&clean_domain)? {
        println!("✅ Found OTP code!");
        println!("   Code: {}", otp.code);
        println!("   Source: {}", otp.source);
        if let Some(expires) = otp.expires_in_seconds {
            println!("   Expires in: {expires}s");
        }
    } else {
        println!("❌ No OTP code found from any source");
        println!("\nSearched:");
        println!("   1. 1Password TOTP");
        println!("   2. SMS via Beeper");
        println!("   3. Email via Gmail");
    }

    Ok(())
}
