use anyhow::Result;

use nab::OtpRetriever;

pub fn cmd_otp(domain: &str) -> Result<()> {
    println!("🔐 Searching for OTP codes for: {domain}\n");

    // Extract domain from URL if needed (falls back to raw input)
    let extracted = super::extract_domain(domain);
    let clean_domain = if extracted.is_empty() {
        domain.to_string()
    } else {
        extracted
    };

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
