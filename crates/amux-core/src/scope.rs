//! Four-tier scope with deterministic inheritance (Invariant 2, RR-0002).
//!
//! Everything configurable lives at one of four scopes:
//! Org -> Global -> Group -> Worker, with the more specific scope winning.
//! One resolver, used everywhere: env, gates, memories, model config,
//! schedules, permissions, automation behavior. Never re-implement
//! inheritance per subsystem — that is how the Python system ended up with
//! env inheritance, gate inheritance, and memory inheritance disagreeing.

use crate::ids::{GroupId, WorkerId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Where a piece of configuration lives. Ordering is precedence: later
/// variants override earlier ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeLevel {
    Org,
    Global,
    Group,
    Worker,
}

impl ScopeLevel {
    /// Resolution order, least to most specific.
    pub const CHAIN: [ScopeLevel; 4] = [
        ScopeLevel::Org,
        ScopeLevel::Global,
        ScopeLevel::Group,
        ScopeLevel::Worker,
    ];
}

/// A concrete scope: the level plus the entity it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "level", rename_all = "snake_case")]
pub enum Scope {
    Org,
    Global,
    Group { id: GroupId },
    Worker { id: WorkerId },
}

impl Scope {
    pub fn level(&self) -> ScopeLevel {
        match self {
            Scope::Org => ScopeLevel::Org,
            Scope::Global => ScopeLevel::Global,
            Scope::Group { .. } => ScopeLevel::Group,
            Scope::Worker { .. } => ScopeLevel::Worker,
        }
    }

    /// Whether a value at `self` is visible when resolving for `target`.
    /// Org/Global are visible to everyone; Group only to that group's
    /// workers; Worker only to that worker.
    pub fn applies_to(&self, target: &ResolutionTarget) -> bool {
        match self {
            Scope::Org | Scope::Global => true,
            Scope::Group { id } => target.group.as_ref() == Some(id),
            Scope::Worker { id } => target.worker.as_ref() == Some(id),
        }
    }
}

/// Who we are resolving configuration for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResolutionTarget {
    pub worker: Option<WorkerId>,
    pub group: Option<GroupId>,
}

/// A value together with the scope that supplied it — provenance is part of
/// the answer, so `amux config show --effective` can print the full chain
/// (RR-0020) and a wrong value is traceable to the layer that set it
/// (ethos rule 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopedValue<T> {
    pub value: T,
    pub source: ScopeLevel,
}

/// Types that can be layered. `merge` applies `other` ON TOP of `self`:
/// fields present in `other` win.
pub trait Mergeable: Clone {
    fn merge(&mut self, other: &Self);
}

/// Resolve the effective config from the four-tier chain. Later (more
/// specific) layers override earlier ones. `None` layers are skipped.
pub fn effective_config<T: Mergeable + Default>(
    org: Option<&T>,
    global: Option<&T>,
    group: Option<&T>,
    worker: Option<&T>,
) -> T {
    let mut eff = T::default();
    for layer in [org, global, group, worker].into_iter().flatten() {
        eff.merge(layer);
    }
    eff
}

/// A string-keyed layered map (environment variables, prefs). Each key
/// resolves independently: the most specific layer that defines the key
/// wins, and the answer carries provenance.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayeredMap {
    pub org: BTreeMap<String, String>,
    pub global: BTreeMap<String, String>,
    pub group: BTreeMap<String, String>,
    pub worker: BTreeMap<String, String>,
}

impl LayeredMap {
    fn layer(&self, level: ScopeLevel) -> &BTreeMap<String, String> {
        match level {
            ScopeLevel::Org => &self.org,
            ScopeLevel::Global => &self.global,
            ScopeLevel::Group => &self.group,
            ScopeLevel::Worker => &self.worker,
        }
    }

    pub fn layer_mut(&mut self, level: ScopeLevel) -> &mut BTreeMap<String, String> {
        match level {
            ScopeLevel::Org => &mut self.org,
            ScopeLevel::Global => &mut self.global,
            ScopeLevel::Group => &mut self.group,
            ScopeLevel::Worker => &mut self.worker,
        }
    }

    /// Effective value for one key, most specific layer wins.
    pub fn resolve(&self, key: &str) -> Option<ScopedValue<&str>> {
        for level in ScopeLevel::CHAIN.iter().rev() {
            if let Some(v) = self.layer(*level).get(key) {
                return Some(ScopedValue {
                    value: v.as_str(),
                    source: *level,
                });
            }
        }
        None
    }

    /// The full effective map with per-key provenance.
    pub fn resolve_all(&self) -> BTreeMap<String, ScopedValue<String>> {
        let mut out = BTreeMap::new();
        // Walk least->most specific; later inserts overwrite, so the most
        // specific layer ends up in the map.
        for level in ScopeLevel::CHAIN {
            for (k, v) in self.layer(level) {
                out.insert(
                    k.clone(),
                    ScopedValue {
                        value: v.clone(),
                        source: level,
                    },
                );
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Default, PartialEq)]
    struct Cfg {
        model: Option<String>,
        max_tokens: Option<u32>,
    }

    impl Mergeable for Cfg {
        fn merge(&mut self, other: &Self) {
            if other.model.is_some() {
                self.model = other.model.clone();
            }
            if other.max_tokens.is_some() {
                self.max_tokens = other.max_tokens;
            }
        }
    }

    #[test]
    fn worker_wins_conflicts() {
        let global = Cfg {
            model: Some("global-model".into()),
            max_tokens: Some(100),
        };
        let group = Cfg {
            model: Some("group-model".into()),
            max_tokens: None,
        };
        let worker = Cfg {
            model: Some("worker-model".into()),
            max_tokens: None,
        };
        let eff = effective_config(None, Some(&global), Some(&group), Some(&worker));
        assert_eq!(eff.model.as_deref(), Some("worker-model"));
        // Worker did not set max_tokens -> global's survives through the chain.
        assert_eq!(eff.max_tokens, Some(100));
    }

    #[test]
    fn group_overrides_global_when_worker_silent() {
        let global = Cfg {
            model: Some("global-model".into()),
            max_tokens: Some(100),
        };
        let group = Cfg {
            model: Some("group-model".into()),
            max_tokens: None,
        };
        let eff = effective_config(None, Some(&global), Some(&group), None);
        assert_eq!(eff.model.as_deref(), Some("group-model"));
        assert_eq!(eff.max_tokens, Some(100));
    }

    #[test]
    fn org_is_the_base_layer() {
        let org = Cfg {
            model: Some("org-model".into()),
            max_tokens: Some(1),
        };
        let eff = effective_config(Some(&org), None, None, None);
        assert_eq!(eff.model.as_deref(), Some("org-model"));
    }

    #[test]
    fn layered_map_worker_env_overrides_group_env() {
        let mut m = LayeredMap::default();
        m.global.insert("API_URL".into(), "https://global".into());
        m.group.insert("API_URL".into(), "https://group".into());
        m.worker.insert("API_URL".into(), "https://worker".into());
        m.group.insert("GROUP_ONLY".into(), "g".into());

        let v = m.resolve("API_URL").unwrap();
        assert_eq!(v.value, "https://worker");
        assert_eq!(v.source, ScopeLevel::Worker);

        let g = m.resolve("GROUP_ONLY").unwrap();
        assert_eq!(g.value, "g");
        assert_eq!(g.source, ScopeLevel::Group);

        assert!(m.resolve("MISSING").is_none());
    }

    #[test]
    fn resolve_all_carries_provenance() {
        let mut m = LayeredMap::default();
        m.org.insert("A".into(), "org".into());
        m.global.insert("A".into(), "global".into());
        m.worker.insert("B".into(), "worker".into());
        let all = m.resolve_all();
        assert_eq!(all["A"].source, ScopeLevel::Global);
        assert_eq!(all["A"].value, "global");
        assert_eq!(all["B"].source, ScopeLevel::Worker);
    }

    #[test]
    fn scope_visibility() {
        let gid = GroupId::from_ulid("01JGXV0000000000000000TEST".parse().unwrap());
        let wid = WorkerId::from_ulid("01JGXV0000000000000000TEST".parse().unwrap());
        let target = ResolutionTarget {
            worker: Some(wid.clone()),
            group: Some(gid.clone()),
        };
        assert!(Scope::Global.applies_to(&target));
        assert!(Scope::Group { id: gid.clone() }.applies_to(&target));
        assert!(Scope::Worker { id: wid }.applies_to(&target));

        let other_group = GroupId::from_ulid("01JGXV00000000000000XXXXXX".parse().unwrap());
        assert!(!Scope::Group { id: other_group }.applies_to(&target));
    }
}
