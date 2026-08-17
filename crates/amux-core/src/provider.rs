//! Provider identity, capabilities, and normalized usage (RR-0007,
//! Invariants 8 and 20).
//!
//! Invariant 8: amux orchestrates work; the model runtime is pluggable.
//! [`ProviderId`] is an OPEN string newtype, not a closed enum — adding a
//! provider ("gemini", "codex", "ollama", one that does not exist yet) must
//! never require recompiling amux-core. A closed enum here would be the
//! ceiling the ethos warns about: the harness must get better as providers
//! multiply, not gate them through a code change.
//!
//! Invariant 20: usage is normalized across fundamentally different
//! billing/quota models **without inventing numbers that don't exist**.
//! "Tokens remaining" means different things per provider (API budget,
//! subscription window, TPM rate limit, daily allowance, local Ollama =
//! effectively unlimited), so usage is a list of [`UsageWindow`]s whose
//! numeric fields are all `Option`. If a provider exposes a number, amux
//! records it; if not, the field stays `None`. Every window also says HOW
//! the number was obtained ([`UsageProvenance`]) and how much to trust it
//! ([`UsageConfidence`]) — a scheduler routing on an estimated number should
//! know it is estimated (ethos rule 4: a wrong answer must be detectable
//! from the data we keep).

use serde::{Deserialize, Serialize};
use std::fmt;

/// Open provider identity: `"claude-code"`, `"gemini"`, `"codex"`,
/// `"ollama"`, etc. Registry-resolved by string, never matched as a closed
/// set (Invariant 8).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderId(pub String);

impl ProviderId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ProviderId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for ProviderId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// The quota/billing shape a [`UsageWindow`] describes. One provider may
/// report several at once (a per-minute rate limit AND a weekly subscription
/// allowance) — that is why [`ProviderUsage`] holds a `Vec`, not one flattened
/// "tokens remaining" that would have to lie for somebody (Invariant 20).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageWindowKind {
    PerMinute,
    PerHour,
    Rolling,
    Daily,
    Weekly,
    Monthly,
    BillingPeriod,
    SubscriptionAllowance,
}

/// How much to trust the numbers in a window. Ordered from best to worst by
/// [`UsageConfidence::rank`]; there is deliberately no way to construct
/// "confident" data out of nothing — a window with no observations is
/// `Unknown`, never a made-up `Exact` zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageConfidence {
    /// The provider reported the number directly.
    Exact,
    /// Derived from real signals, but rounded/partial.
    Approximate,
    /// Was real once; the observation is old enough to have drifted.
    Stale,
    /// No data. The honest default (Invariant 20: never invent numbers).
    Unknown,
}

impl UsageConfidence {
    /// Higher is more trustworthy. Exists so callers can take the WEAKEST
    /// confidence across windows without deriving `Ord` (declaration order
    /// would make `min` mean "best", which is exactly the silent inversion
    /// ethos rule 7 warns about).
    pub fn rank(self) -> u8 {
        match self {
            UsageConfidence::Unknown => 0,
            UsageConfidence::Stale => 1,
            UsageConfidence::Approximate => 2,
            UsageConfidence::Exact => 3,
        }
    }
}

/// HOW a usage number was obtained. The 2026-07 schedule_runs incident
/// (ethos rule 4) is why this exists: two byte-identical numbers from an API
/// and from a terminal scrape are NOT the same fact, and a consumer deciding
/// whether to route work on one must be able to see which it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageProvenance {
    /// The provider's own usage API.
    Api,
    /// The provider's CLI reported it (structured output).
    ProviderCli,
    /// A structured runtime event (hook, OpenCode protocol).
    StructuredRuntime,
    /// Parsed from `X-RateLimit-*`-style HTTP headers.
    HttpHeaders,
    /// Scraped from terminal output — the D1 fallback, lowest-fidelity
    /// observation of a real signal.
    TerminalFallback,
    /// amux's own token counting.
    LocalAccounting,
    /// Computed from other observations; not observed at all.
    DerivedEstimate,
}

/// One normalized quota window. Every numeric field is `Option<u64>`:
/// `u64` makes "negative usage" unrepresentable (the validation the type
/// system does for free), and `Option` makes "we don't know" expressible
/// without a sentinel. `used > limit` is deliberately LEGAL — providers do
/// let accounts run over — so arithmetic goes through the saturating
/// helpers instead of a constructor rejecting real-world states.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageWindow {
    pub window_kind: UsageWindowKind,
    /// Units consumed in this window (tokens, requests — the window's own
    /// unit). `None` = not reported, never 0-as-unknown.
    pub used: Option<u64>,
    /// The window's cap. `None` = uncapped or unreported (Ollama).
    pub limit: Option<u64>,
    /// When the window resets, if the provider says.
    pub resets_at: Option<chrono::DateTime<chrono::Utc>>,
    pub confidence: UsageConfidence,
    pub provenance: UsageProvenance,
}

impl UsageWindow {
    /// A window we know nothing about numerically: all fields `None`,
    /// confidence `Unknown`. The only honest starting point.
    pub fn unknown(window_kind: UsageWindowKind, provenance: UsageProvenance) -> Self {
        Self {
            window_kind,
            used: None,
            limit: None,
            resets_at: None,
            confidence: UsageConfidence::Unknown,
            provenance,
        }
    }

    /// Capacity left, when both sides are known. Saturating: an over-limit
    /// account reports 0 remaining, not an underflow panic — over-limit is a
    /// real state, not a bug (Invariant 20).
    pub fn remaining(&self) -> Option<u64> {
        match (self.used, self.limit) {
            (Some(used), Some(limit)) => Some(limit.saturating_sub(used)),
            _ => None,
        }
    }

    /// How far past the cap the account is (0 when within it). The mirror
    /// of [`Self::remaining`] so over-limit is visible, not clamped away.
    pub fn overage(&self) -> Option<u64> {
        match (self.used, self.limit) {
            (Some(used), Some(limit)) => Some(used.saturating_sub(limit)),
            _ => None,
        }
    }
}

/// Normalized usage for one provider: zero or more windows. An unknown
/// provider (no adapter, no data) has zero windows and therefore
/// [`UsageConfidence::Unknown`] — the type cannot be coaxed into a
/// confident answer it has no basis for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderUsage {
    pub provider: ProviderId,
    pub windows: Vec<UsageWindow>,
}

impl ProviderUsage {
    pub fn new(provider: ProviderId, windows: Vec<UsageWindow>) -> Self {
        Self { provider, windows }
    }

    /// Usage for a provider we know nothing about. Zero windows; the
    /// headline confidence is `Unknown` (RR-0007 test contract).
    pub fn unknown(provider: ProviderId) -> Self {
        Self {
            provider,
            windows: Vec::new(),
        }
    }

    /// Headline confidence: the WEAKEST across windows, `Unknown` when
    /// there are none. Weakest, not best, because a scheduler acting on the
    /// aggregate is only as safe as its least reliable component — reporting
    /// the best window's confidence would launder an estimate into an
    /// authoritative-looking answer (ethos rule 4).
    pub fn confidence(&self) -> UsageConfidence {
        self.windows
            .iter()
            .map(|w| w.confidence)
            .min_by_key(|c| c.rank())
            .unwrap_or(UsageConfidence::Unknown)
    }
}

/// What a provider adapter can do, so the orchestrator plans around missing
/// capabilities instead of discovering them as failures. All-`false` default
/// = the conservative assumption for a provider nobody has described yet
/// (Invariant 8: unknown providers are legal, so they need safe defaults).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    /// Can switch models mid-session without a restart.
    pub hot_model_switch: bool,
    /// Reports usage through some structured channel (any provenance above
    /// `TerminalFallback`).
    pub reports_usage: bool,
    /// Emits structured runtime events (hooks/protocol) rather than only
    /// terminal text — the D1 discriminator.
    pub structured_events: bool,
    /// Supports lifecycle hooks amux can subscribe to.
    pub hooks: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_windows_never_negative() {
        // used > limit is legal (over-limit accounts exist); u64 makes a
        // negative count unrepresentable, and the saturating helpers keep
        // arithmetic in-bounds instead of panicking.
        let w = UsageWindow {
            window_kind: UsageWindowKind::Daily,
            used: Some(1200),
            limit: Some(1000),
            resets_at: None,
            confidence: UsageConfidence::Exact,
            provenance: UsageProvenance::Api,
        };
        assert_eq!(w.remaining(), Some(0)); // saturated, not underflowed
        assert_eq!(w.overage(), Some(200)); // over-limit stays visible

        let within = UsageWindow {
            used: Some(300),
            limit: Some(1000),
            ..w.clone()
        };
        assert_eq!(within.remaining(), Some(700));
        assert_eq!(within.overage(), Some(0));
    }

    #[test]
    fn no_invented_numbers() {
        // A window with nothing reported stays None everywhere — never a
        // fabricated zero (Invariant 20).
        let w = UsageWindow::unknown(UsageWindowKind::Rolling, UsageProvenance::TerminalFallback);
        assert_eq!(w.used, None);
        assert_eq!(w.limit, None);
        assert_eq!(w.resets_at, None);
        assert_eq!(w.remaining(), None);
        assert_eq!(w.overage(), None);
        assert_eq!(w.confidence, UsageConfidence::Unknown);
    }

    #[test]
    fn unknown_provider_reports_unknown_confidence() {
        let usage = ProviderUsage::unknown(ProviderId::new("some-brand-new-provider"));
        assert!(usage.windows.is_empty());
        assert_eq!(usage.confidence(), UsageConfidence::Unknown);
    }

    #[test]
    fn headline_confidence_is_the_weakest_window() {
        let exact = UsageWindow {
            window_kind: UsageWindowKind::PerMinute,
            used: Some(10),
            limit: Some(100),
            resets_at: None,
            confidence: UsageConfidence::Exact,
            provenance: UsageProvenance::Api,
        };
        let stale = UsageWindow {
            window_kind: UsageWindowKind::Weekly,
            confidence: UsageConfidence::Stale,
            provenance: UsageProvenance::TerminalFallback,
            ..exact.clone()
        };
        let usage = ProviderUsage::new(ProviderId::new("claude-code"), vec![exact, stale]);
        assert_eq!(usage.confidence(), UsageConfidence::Stale);
    }

    #[test]
    fn provider_id_is_open_and_transparent() {
        // Any string is a valid provider (Invariant 8): no registry check,
        // no enum. Serde is transparent so IDs read naturally in payloads.
        let id = ProviderId::new("a-provider-from-2031");
        assert_eq!(id.as_str(), "a-provider-from-2031");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"a-provider-from-2031\"");
        let back: ProviderId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn default_capabilities_are_conservative() {
        let caps = ProviderCapabilities::default();
        assert!(!caps.hot_model_switch);
        assert!(!caps.reports_usage);
        assert!(!caps.structured_events);
        assert!(!caps.hooks);
    }
}
