pub fn cmd_fingerprint(count: usize) {
    println!("🎭 Generating {count} browser fingerprints:\n");

    for i in 0..count {
        let profile = nab::random_profile();
        println!("Profile {}:", i + 1);
        println!("   UA: {}", profile.user_agent);
        println!("   Accept-Language: {}", profile.accept_language);
        if !profile.sec_ch_ua.is_empty() {
            println!("   Sec-CH-UA: {}", profile.sec_ch_ua);
        }
        println!();
    }
}
