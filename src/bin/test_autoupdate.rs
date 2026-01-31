//! Test autoupdate functionality

use microfetch::fingerprint::autoupdate::BrowserVersions;

fn main() {
    println!("Testing browser version auto-update...\n");

    let versions = BrowserVersions::load_or_update();

    println!("Chrome versions: {:?}", versions.chrome);
    println!("Firefox versions: {:?}", versions.firefox);
    println!("Safari versions: {:?}", versions.safari);
    println!("\nLast updated: {}", versions.last_updated);
    println!("Safari last checked: {}", versions.safari_last_checked);
}
