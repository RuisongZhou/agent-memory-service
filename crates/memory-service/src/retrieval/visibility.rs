//! Scope primitives shared by memory algorithms. Each algorithm keeps its request defaults.
use crate::domain::types::{MemScope, ReqScope};

#[derive(Debug, Clone, PartialEq)]
pub enum Lvl {
    /// "*": every value at this level
    Any,
    /// no value given: nothing at this level is visible
    No,
    Is(String),
}

#[derive(Debug, Clone)]
pub struct Vis {
    pub project: Lvl,
    pub platform: Lvl,
}

impl Vis {
    /// `scope.project` defaults to nothing (only global + platform memories are visible without a project).
    /// The platform always comes from the connection (2.2); the only value a caller may pass is "*".
    pub fn new(scope: &ReqScope, caller_platform: &str) -> Self {
        let project = match scope.project.as_deref().map(str::trim) {
            Some("*") => Lvl::Any,
            Some(p) if !p.is_empty() => Lvl::Is(p.to_string()),
            _ => Lvl::No,
        };
        let platform = match scope.platform.as_deref().map(str::trim) {
            Some("*") => Lvl::Any,
            _ => Lvl::Is(caller_platform.to_string()),
        };
        Self { project, platform }
    }

    pub fn sees(&self, s: &MemScope) -> bool {
        let lvl = |l: &Lvl, v: &Option<String>| match l {
            Lvl::Any => true,
            Lvl::No => false,
            Lvl::Is(x) => v.as_deref() == Some(x.as_str()),
        };
        match s.kind.as_str() {
            "global" => true,
            "project" => lvl(&self.project, &s.project),
            "platform" => lvl(&self.platform, &s.platform),
            _ => false,
        }
    }

    /// The same rule as a Tantivy filter (OR-ed field/value pairs).
    pub fn ft_pairs(&self) -> Vec<(String, String)> {
        let mut v = vec![("scope_kind".to_string(), "global".to_string())];
        match &self.project {
            Lvl::Any => v.push(("scope_kind".into(), "project".into())),
            Lvl::Is(p) => v.push(("project".into(), p.clone())),
            Lvl::No => {}
        }
        match &self.platform {
            Lvl::Any => v.push(("scope_kind".into(), "platform".into())),
            Lvl::Is(p) => v.push(("platform".into(), p.clone())),
            Lvl::No => {}
        }
        v
    }

    pub fn current_project(&self) -> Option<&str> {
        match &self.project {
            Lvl::Is(p) => Some(p.as_str()),
            _ => None,
        }
    }
}

/// Exact scope matching used by formation recall.
pub fn visible(scope: &MemScope, project: &str, platform: &str) -> bool {
    scope.kind == "global"
        || (scope.kind == "project" && scope.project.as_deref() == Some(project))
        || (scope.kind == "platform" && scope.platform.as_deref() == Some(platform))
}
