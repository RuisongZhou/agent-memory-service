//! Memory URIs: the read-only tree agents browse with `memory_ls` and `memory_read` (docs/design/memory-fs.md).
//! A URI names a place, never a permission: the engine still checks what the caller may see. No I/O.
//!
//! ```text
//! mem://                                   root
//! mem://projects/                          every project with records or dialogue
//! mem://projects/{project}/                one project: its type directories and sessions
//! mem://projects/{project}/facts/{id}      one record, in the directory of its type
//! mem://projects/{project}/sessions/{s}/   one session's turns, in input order
//! mem://projects/{project}/sessions/{s}/{requestId}   one turn, the dialogue as submitted
//! mem://global/...                         the same for global records and sessions with no project folder
//! mem://platform/{platform}/facts/...      a platform's platform-scoped records (no sessions)
//! ```
//!
//! Dynamic segments (project ids, which are folder paths, sessions, platforms) are quoted with
//! [`memory_rules::util::quote`], so a `/` inside one never reads as a separator.
use memory_rules::util::{quote, unquote};

use super::types::{MemScope, MemoryRec};

pub const SCHEME: &str = "mem://";

/// Where records and turns live: one project, the global scope, or one platform's platform-scoped records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Space {
    Project(String),
    Global,
    Platform(String),
}

impl Space {
    /// The space of a record's scope; `None` for a scope kind the daemon does not write.
    pub fn of_scope(scope: &MemScope) -> Option<Self> {
        match (scope.kind.as_str(), &scope.project, &scope.platform) {
            ("global", _, _) => Some(Self::Global),
            ("project", Some(p), _) if !p.is_empty() => Some(Self::Project(p.clone())),
            ("platform", _, Some(p)) if !p.is_empty() => Some(Self::Platform(p.clone())),
            _ => None,
        }
    }

    /// The space of a stored request's `project`: a global session's requests are filed under `""`.
    pub fn of_request(project: &str) -> Self {
        if project.is_empty() {
            Self::Global
        } else {
            Self::Project(project.into())
        }
    }

    /// Whether a record with `scope` lives here.
    pub fn holds(&self, scope: &MemScope) -> bool {
        Self::of_scope(scope).as_ref() == Some(self)
    }

    /// The `project` stored requests of this space carry; a platform space has no sessions.
    pub fn request_project(&self) -> Option<&str> {
        match self {
            Self::Project(p) => Some(p),
            Self::Global => Some(""),
            Self::Platform(_) => None,
        }
    }

    fn path(&self) -> String {
        match self {
            Self::Project(p) => format!("projects/{}/", quote(p)),
            Self::Global => "global/".into(),
            Self::Platform(p) => format!("platform/{}/", quote(p)),
        }
    }
}

/// The directory of one memory type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Facts,
    Instructions,
    Experiences,
    Profiles,
    Skills,
}

impl Kind {
    pub const ALL: [Kind; 5] = [
        Kind::Instructions,
        Kind::Facts,
        Kind::Experiences,
        Kind::Profiles,
        Kind::Skills,
    ];

    pub fn of_type(mtype: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.mtype() == mtype)
    }

    /// The record `type` this directory holds.
    pub fn mtype(self) -> &'static str {
        match self {
            Kind::Facts => "fact",
            Kind::Instructions => "instruction",
            Kind::Experiences => "experience",
            Kind::Profiles => "profile",
            Kind::Skills => "skill",
        }
    }

    pub fn dir(self) -> &'static str {
        match self {
            Kind::Facts => "facts",
            Kind::Instructions => "instructions",
            Kind::Experiences => "experiences",
            Kind::Profiles => "profiles",
            Kind::Skills => "skills",
        }
    }

    fn of_dir(dir: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.dir() == dir)
    }
}

/// One place in the tree. Directories are everything but `Record` and `Turn`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Root,
    Projects,
    Platforms,
    Space(Space),
    Kind(Space, Kind),
    Record(Space, Kind, String),
    Sessions(Space),
    Session(Space, String),
    /// A turn by its request id: turn ids carry `/` and are only unique per platform.
    Turn(Space, String, String),
}

impl Node {
    /// Parse a `mem://` URI. A trailing `/` is optional on directories and refused on files; every dynamic
    /// segment must be nonempty and correctly quoted. `mem://~/` is the MCP bridge's alias for its default
    /// project and never reaches the daemon.
    pub fn parse(uri: &str) -> Result<Self, String> {
        let rest = uri
            .strip_prefix(SCHEME)
            .ok_or_else(|| format!("a memory URI starts with {SCHEME}, not {uri:?}"))?;
        let dir = rest.is_empty() || rest.ends_with('/');
        let parts: Vec<&str> = rest.trim_end_matches('/').split('/').collect();
        let parts: &[&str] = if parts == [""] { &[] } else { &parts };
        let segment = |raw: &str| -> Result<String, String> {
            match unquote(raw) {
                Some(s) if !s.is_empty() => Ok(s),
                _ => Err(format!("{uri:?} has an empty or malformed segment {raw:?}")),
            }
        };
        let node = match parts {
            [] => Self::Root,
            ["~", ..] => {
                return Err(
                    "mem://~/ stands for the MCP connection's default project; over HTTP name the project: \
                     mem://projects/{project}/"
                        .into(),
                )
            }
            ["projects"] => Self::Projects,
            ["platform"] => Self::Platforms,
            ["projects", project, rest @ ..] => {
                let project = segment(project)?;
                if project == "*" {
                    return Err("a memory URI names one project, not *".into());
                }
                Self::within(Space::Project(project), rest, uri, segment)?
            }
            ["global", rest @ ..] => Self::within(Space::Global, rest, uri, segment)?,
            ["platform", platform, rest @ ..] => {
                Self::within(Space::Platform(segment(platform)?), rest, uri, segment)?
            }
            _ => return Err(format!("{uri:?} is not in the memory tree")),
        };
        if dir && node.is_file() && !rest.is_empty() {
            return Err(format!("{uri:?} names a file: drop the trailing /"));
        }
        Ok(node)
    }

    fn within(
        space: Space,
        rest: &[&str],
        uri: &str,
        segment: impl Fn(&str) -> Result<String, String>,
    ) -> Result<Self, String> {
        let sessions = !matches!(space, Space::Platform(_));
        Ok(match rest {
            [] => Self::Space(space),
            ["sessions"] if sessions => Self::Sessions(space),
            ["sessions", s] if sessions => Self::Session(space, segment(s)?),
            ["sessions", s, request] if sessions => {
                Self::Turn(space, segment(s)?, segment(request)?)
            }
            [dir] => Self::Kind(space, Self::kind(dir, uri)?),
            [dir, id] => Self::Record(space, Self::kind(dir, uri)?, segment(id)?),
            _ => return Err(format!("{uri:?} is not in the memory tree")),
        })
    }

    fn kind(dir: &str, uri: &str) -> Result<Kind, String> {
        Kind::of_dir(dir).ok_or_else(|| {
            format!(
                "{uri:?}: {dir:?} is not a directory here (instructions, facts, experiences, profiles, skills, sessions)"
            )
        })
    }

    pub fn is_file(&self) -> bool {
        matches!(self, Self::Record(..) | Self::Turn(..))
    }

    /// The canonical URI: directories end with `/`.
    pub fn uri(&self) -> String {
        let path = match self {
            Self::Root => String::new(),
            Self::Projects => "projects/".into(),
            Self::Platforms => "platform/".into(),
            Self::Space(s) => s.path(),
            Self::Kind(s, k) => format!("{}{}/", s.path(), k.dir()),
            Self::Record(s, k, id) => format!("{}{}/{}", s.path(), k.dir(), quote(id)),
            Self::Sessions(s) => format!("{}sessions/", s.path()),
            Self::Session(s, session) => format!("{}sessions/{}/", s.path(), quote(session)),
            Self::Turn(s, session, request) => {
                format!("{}sessions/{}/{}", s.path(), quote(session), quote(request))
            }
        };
        format!("{SCHEME}{path}")
    }
}

/// The URI of a record, in the directory of its scope and type; `None` for a scope or type the tree has no
/// place for.
pub fn memory_uri(m: &MemoryRec) -> Option<String> {
    Some(
        Node::Record(
            Space::of_scope(&m.scope)?,
            Kind::of_type(&m.mtype)?,
            m.id.clone(),
        )
        .uri(),
    )
}

/// The URI of a stored turn: its request's project (`""` for a global session), session and request id.
pub fn turn_uri(project: &str, session: &str, request_id: &str) -> String {
    Node::Turn(
        Space::of_request(project),
        session.into(),
        request_id.into(),
    )
    .uri()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_round_trip_and_quote_their_dynamic_segments() {
        let project = Space::Project("/Users/me/my repo".into());
        let nodes = [
            (Node::Root, "mem://"),
            (Node::Projects, "mem://projects/"),
            (Node::Platforms, "mem://platform/"),
            (
                Node::Space(project.clone()),
                "mem://projects/%2FUsers%2Fme%2Fmy%20repo/",
            ),
            (
                Node::Kind(Space::Global, Kind::Instructions),
                "mem://global/instructions/",
            ),
            (
                Node::Record(Space::Platform("codex".into()), Kind::Facts, "m1".into()),
                "mem://platform/codex/facts/m1",
            ),
            (Node::Sessions(Space::Global), "mem://global/sessions/"),
            (
                Node::Session(project.clone(), "s/1".into()),
                "mem://projects/%2FUsers%2Fme%2Fmy%20repo/sessions/s%2F1/",
            ),
            (
                Node::Turn(project, "s/1".into(), "rq1".into()),
                "mem://projects/%2FUsers%2Fme%2Fmy%20repo/sessions/s%2F1/rq1",
            ),
        ];
        for (node, uri) in nodes {
            assert_eq!(node.uri(), uri);
            assert_eq!(Node::parse(uri), Ok(node.clone()), "{uri}");
        }
        // A directory may leave out its trailing slash; a file may not have one.
        assert_eq!(
            Node::parse("mem://global/facts"),
            Ok(Node::Kind(Space::Global, Kind::Facts))
        );
        assert!(Node::parse("mem://global/facts/m1/").is_err());
    }

    #[test]
    fn what_is_not_in_the_tree_is_refused() {
        for uri in [
            "viking://resources/",
            "mem:/global/",
            "mem://~/facts/",
            "mem://projects/*/facts/",
            "mem://projects//facts/",
            "mem://projects/%zz/",
            "mem://global/notes/",
            "mem://global/facts/m1/extra",
            "mem://platform/codex/sessions/",
            "mem://elsewhere/",
        ] {
            assert!(Node::parse(uri).is_err(), "{uri}");
        }
    }

    #[test]
    fn records_and_turns_are_addressed_by_scope_type_and_id() {
        let fact = MemoryRec {
            id: "m1".into(),
            mtype: "fact".into(),
            scope: MemScope::project("/w/p"),
            ..Default::default()
        };
        assert_eq!(
            memory_uri(&fact).unwrap(),
            "mem://projects/%2Fw%2Fp/facts/m1"
        );
        let unknown = MemoryRec {
            mtype: "note".into(),
            ..fact.clone()
        };
        assert_eq!(memory_uri(&unknown), None);
        assert_eq!(turn_uri("", "s1", "rq1"), "mem://global/sessions/s1/rq1");
        assert!(Space::Global.holds(&MemScope::global()));
        assert!(!Space::Project("/w/p".into()).holds(&MemScope::project("/w/q")));
    }
}
