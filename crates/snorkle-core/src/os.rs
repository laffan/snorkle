//! Minimum OS policy.
//!
//! WebKit security fixes ship only with OS updates, so the browser's
//! security depends on the OS version. Policy (docs/ARCHITECTURE.md §OS):
//! - `MINIMUM` is also the Xcode deployment target, so the App Store will
//!   not install the app below it. The runtime check is defence in depth
//!   (e.g. sideloaded/TestFlight builds with a lower target): below it the
//!   browser refuses to create web views.
//! - Within a supported major release, versions older than the newest
//!   patch level known at build time get a persistent warning.
//!
//! These constants must be updated for every OS security release. The
//! values below were current as of 2026-10-29 as far as could be verified
//! (iPadOS 27.0 released 2026-09-14; latest 26.x security update found was
//! 26.6.1). See docs/OPEN_QUESTIONS.md.

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct OsVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl OsVersion {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        OsVersion { major, minor, patch }
    }
}

impl std::fmt::Display for OsVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Current release (27) and one prior (26).
pub const MINIMUM: OsVersion = OsVersion::new(26, 0, 0);

/// Newest known security patch level per supported major version.
pub const LATEST_KNOWN_PATCH: &[OsVersion] = &[OsVersion::new(26, 6, 1), OsVersion::new(27, 0, 0)];

/// macOS: WebKit is updated with Safari, on the current macOS and the
/// earlier ones Apple still supports (Safari 27 runs on macOS 15, 26 and
/// 27), so the policy is about the installed Safari's version rather than
/// the OS version. Below `MACOS_SAFARI_MINIMUM` no web view is created;
/// below `MACOS_SAFARI_LATEST` (on any macOS) a banner asks for an update.
/// As of 2026-11-03: Safari 27.2 is the newest release with security fixes
/// found (Apple's "About the security content of Safari 27.2").
pub const MACOS_SAFARI_MINIMUM: OsVersion = OsVersion::new(26, 0, 0);
pub const MACOS_SAFARI_LATEST: OsVersion = OsVersion::new(27, 2, 0);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum OsSupport {
    Supported,
    UpdateRecommended { current: OsVersion, recommended: OsVersion },
    Unsupported { current: OsVersion, minimum: OsVersion },
    /// Desktop development build: this platform has no content host, so
    /// pages are never rendered (only the chrome UI runs).
    PreviewOnly,
}

impl OsSupport {
    pub fn allows_browsing(&self) -> bool {
        !matches!(self, OsSupport::Unsupported { .. } | OsSupport::PreviewOnly)
    }
}

pub fn check(current: OsVersion) -> OsSupport {
    if current < MINIMUM {
        return OsSupport::Unsupported { current, minimum: MINIMUM };
    }
    match LATEST_KNOWN_PATCH.iter().find(|v| v.major == current.major) {
        Some(latest) if current < *latest => OsSupport::UpdateRecommended { current, recommended: *latest },
        // Newer major versions than we know about are assumed current.
        _ => OsSupport::Supported,
    }
}

/// The macOS policy, from the installed Safari's version (see
/// [`MACOS_SAFARI_LATEST`]). Newer versions than known are current.
pub fn check_macos_safari(safari: OsVersion) -> OsSupport {
    if safari < MACOS_SAFARI_MINIMUM {
        OsSupport::Unsupported { current: safari, minimum: MACOS_SAFARI_MINIMUM }
    } else if safari < MACOS_SAFARI_LATEST {
        OsSupport::UpdateRecommended { current: safari, recommended: MACOS_SAFARI_LATEST }
    } else {
        OsSupport::Supported
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy() {
        assert!(matches!(check(OsVersion::new(18, 7, 0)), OsSupport::Unsupported { .. }));
        assert!(matches!(check(OsVersion::new(26, 5, 0)), OsSupport::UpdateRecommended { .. }));
        assert_eq!(check(OsVersion::new(26, 6, 1)), OsSupport::Supported);
        assert_eq!(check(OsVersion::new(27, 0, 0)), OsSupport::Supported);
        assert_eq!(check(OsVersion::new(28, 0, 0)), OsSupport::Supported);
        assert!(!check(OsVersion::new(25, 9, 9)).allows_browsing());
    }

    #[test]
    fn macos_policy_follows_safari() {
        assert!(matches!(check_macos_safari(OsVersion::new(18, 6, 0)), OsSupport::Unsupported { .. }));
        assert!(!check_macos_safari(OsVersion::new(18, 6, 0)).allows_browsing());
        assert!(matches!(check_macos_safari(OsVersion::new(26, 4, 0)), OsSupport::UpdateRecommended { .. }));
        assert!(matches!(check_macos_safari(OsVersion::new(27, 1, 0)), OsSupport::UpdateRecommended { .. }));
        assert_eq!(check_macos_safari(OsVersion::new(27, 2, 0)), OsSupport::Supported);
        assert_eq!(check_macos_safari(OsVersion::new(28, 0, 0)), OsSupport::Supported);
    }
}
