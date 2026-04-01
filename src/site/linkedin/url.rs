//! URL classification for `LinkedIn` pages.

/// `LinkedIn` URL content type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkedInUrlKind {
    /// `/in/username` — personal profile
    Profile,
    /// `/company/name` — company page
    Company,
    /// `/posts/...` — individual post
    Post,
    /// `/pulse/...` — long-form article
    Pulse,
    /// `/feed/update/...` — feed activity item
    FeedUpdate,
    /// `/in/username/recent-activity/...` — activity feed
    Activity,
}

impl LinkedInUrlKind {
    /// Whether this URL kind can fall back to oEmbed when authenticated fetch fails.
    #[cfg(any(test, feature = "impersonate"))]
    pub(super) fn has_oembed_fallback(self) -> bool {
        matches!(self, Self::Post | Self::Pulse | Self::FeedUpdate)
    }

    /// Whether this URL kind requires cookies (no public fallback).
    pub(super) fn requires_auth(self) -> bool {
        matches!(self, Self::Profile | Self::Company | Self::Activity)
    }
}

/// Classify a `LinkedIn` URL into its content type.
#[must_use]
pub fn classify_linkedin_url(url: &str) -> Option<LinkedInUrlKind> {
    let lower = url.to_lowercase();
    let path = lower.split('?').next().unwrap_or(&lower);

    if !path.contains("linkedin.com/") {
        return None;
    }

    // Order matters: more specific patterns first
    if path.contains("/recent-activity/") {
        return Some(LinkedInUrlKind::Activity);
    }
    if path.contains("/feed/update/") {
        return Some(LinkedInUrlKind::FeedUpdate);
    }
    // /feed/ or /feed (home feed) — requires auth, no oEmbed fallback
    if path.ends_with("/feed/") || path.ends_with("/feed") {
        return Some(LinkedInUrlKind::Activity);
    }
    // Other authenticated sections: mynetwork, jobs, messaging, notifications
    for section in &["/mynetwork/", "/jobs/", "/messaging/", "/notifications/"] {
        if path.contains(section) {
            return Some(LinkedInUrlKind::Activity);
        }
    }
    if path.contains("/posts/") {
        return Some(LinkedInUrlKind::Post);
    }
    if path.contains("/pulse/") {
        return Some(LinkedInUrlKind::Pulse);
    }
    if path.contains("/company/") {
        return Some(LinkedInUrlKind::Company);
    }
    // Profile: /in/username — must have something after /in/
    if let Some(after) = path.split("/in/").nth(1) {
        let segment = after.split('/').next().unwrap_or("");
        if !segment.is_empty() {
            return Some(LinkedInUrlKind::Profile);
        }
    }

    None
}
