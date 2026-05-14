//! Curated safe-list: which services may be stopped, which processes may be
//! suspended, and which are *explicitly denied*.
//!
//! This is the trust boundary between "user-authored profile" and "OS-level
//! action." A profile can request `stop_services = ["WinDefend"]`; the
//! planner checks against this list and rejects it. The denylist is therefore
//! authoritative: even if a future contributor adds `WinDefend` to the
//! allowlist by mistake, the explicit denylist entry blocks it.
//!
//! Data is loaded once from `safe_lists/services.json` and
//! `safe_lists/processes.json`, both vendored at compile time via
//! `include_str!`. Keeping the data in JSON (not Rust) means contributors
//! review the rationale prose without touching code, and downstream tooling
//! can scrape the same source.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use serde::Deserialize;
use thiserror::Error;
use tracing::warn;

const SERVICES_JSON: &str = include_str!("safe_lists/services.json");
const PROCESSES_JSON: &str = include_str!("safe_lists/processes.json");

#[derive(Debug, Error)]
pub enum SafeListError {
    #[error("safe-list JSON parse error: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("safe-list schema_version {found} is not supported (expected {expected})")]
    UnsupportedSchema { expected: u32, found: u32 },
    #[error("safe-list contains duplicate id: {0}")]
    DuplicateId(String),
}

/// Versioned envelope around the curated lists.
const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
struct ServicesFile {
    schema_version: u32,
    services: Vec<SafeServiceEntry>,
    #[serde(default)]
    denylist: Vec<DeniedEntry>,
}

#[derive(Debug, Deserialize)]
struct ProcessesFile {
    schema_version: u32,
    processes: Vec<SafeProcessEntry>,
    #[serde(default)]
    denylist: Vec<DeniedEntry>,
}

/// Allow-listed Windows service. The presence of an entry means "the project
/// has reviewed this service and confirmed that stopping it temporarily is
/// safe under normal use." The `default_stop` flag is advisory — UI can
/// pre-check or recommend, but a profile must still ask explicitly.
#[derive(Debug, Clone, Deserialize)]
pub struct SafeServiceEntry {
    pub id: String,
    pub display_name: String,
    pub rationale: String,
    #[serde(default)]
    pub default_stop: bool,
}

/// Allow-listed process. Same semantics as `SafeServiceEntry`, but for
/// suspend/resume. Match against `exe` is case-insensitive against the
/// trailing path component (no directory matching).
#[derive(Debug, Clone, Deserialize)]
pub struct SafeProcessEntry {
    pub exe: String,
    pub display_name: String,
    pub rationale: String,
    #[serde(default)]
    pub default_suspend: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct DeniedEntry {
    #[serde(alias = "id", alias = "exe")]
    name: String,
    rationale: String,
}

/// In-memory, parsed view of the curated lists.
///
/// Hashmaps + sets are case-folded once at construction so lookups are
/// case-insensitive without per-call allocation.
#[derive(Debug, Clone)]
pub struct SafeList {
    services: HashMap<String, SafeServiceEntry>,
    service_denied: HashMap<String, String>,
    processes: HashMap<String, SafeProcessEntry>,
    process_denied: HashMap<String, String>,
}

impl SafeList {
    /// Parse the vendored JSON. Returns errors for unsupported schema versions
    /// or duplicate ids.
    pub fn from_bundled() -> Result<Self, SafeListError> {
        let services_file: ServicesFile = serde_json::from_str(SERVICES_JSON)?;
        let processes_file: ProcessesFile = serde_json::from_str(PROCESSES_JSON)?;

        if services_file.schema_version != SCHEMA_VERSION {
            return Err(SafeListError::UnsupportedSchema {
                expected: SCHEMA_VERSION,
                found: services_file.schema_version,
            });
        }
        if processes_file.schema_version != SCHEMA_VERSION {
            return Err(SafeListError::UnsupportedSchema {
                expected: SCHEMA_VERSION,
                found: processes_file.schema_version,
            });
        }

        let mut services = HashMap::new();
        for entry in services_file.services {
            let key = entry.id.to_ascii_lowercase();
            if services.insert(key.clone(), entry.clone()).is_some() {
                return Err(SafeListError::DuplicateId(entry.id));
            }
        }
        let mut service_denied = HashMap::new();
        for d in services_file.denylist {
            service_denied.insert(d.name.to_ascii_lowercase(), d.rationale);
        }

        let mut processes = HashMap::new();
        for entry in processes_file.processes {
            let key = entry.exe.to_ascii_lowercase();
            if processes.insert(key.clone(), entry.clone()).is_some() {
                return Err(SafeListError::DuplicateId(entry.exe));
            }
        }
        let mut process_denied = HashMap::new();
        for d in processes_file.denylist {
            process_denied.insert(d.name.to_ascii_lowercase(), d.rationale);
        }

        Ok(SafeList {
            services,
            service_denied,
            processes,
            process_denied,
        })
    }

    /// Singleton handle to the bundled list. Constructs on first call; future
    /// calls share the same parsed instance. Panics if the bundled JSON fails
    /// to parse — which would indicate a build-time bug, since the JSON is a
    /// repo asset reviewed in tests.
    pub fn bundled() -> &'static Self {
        static INSTANCE: OnceLock<SafeList> = OnceLock::new();
        INSTANCE.get_or_init(|| {
            Self::from_bundled().expect("bundled safe-list JSON failed to parse — this is a bug")
        })
    }

    /// Yes-this-service-may-be-stopped check. Returns the curated entry on
    /// allow, an explanation on deny, or `None` if the id isn't listed.
    pub fn check_service(&self, id: &str) -> ServiceVerdict<'_> {
        let key = id.to_ascii_lowercase();
        if let Some(reason) = self.service_denied.get(&key) {
            return ServiceVerdict::Denied(reason);
        }
        match self.services.get(&key) {
            Some(entry) => ServiceVerdict::Allowed(entry),
            None => ServiceVerdict::Unlisted,
        }
    }

    /// Yes-this-process-may-be-suspended check.
    pub fn check_process(&self, exe: &str) -> ProcessVerdict<'_> {
        let key = exe.to_ascii_lowercase();
        if let Some(reason) = self.process_denied.get(&key) {
            return ProcessVerdict::Denied(reason);
        }
        match self.processes.get(&key) {
            Some(entry) => ProcessVerdict::Allowed(entry),
            None => ProcessVerdict::Unlisted,
        }
    }

    /// Iterate every allow-listed service entry. Order is unspecified.
    pub fn services(&self) -> impl Iterator<Item = &SafeServiceEntry> {
        self.services.values()
    }

    /// Iterate every allow-listed process entry. Order is unspecified.
    pub fn processes(&self) -> impl Iterator<Item = &SafeProcessEntry> {
        self.processes.values()
    }

    /// Filter a requested list of service ids into (allowed, rejected) buckets
    /// against this safe-list. Useful for the planner so we batch-validate
    /// once and surface rejections together. The rejected list pairs the id
    /// with the reason (denied or unlisted) for surfacing in logs / UI.
    pub fn partition_services<'a>(
        &'a self,
        requested: &'a [String],
    ) -> (Vec<&'a SafeServiceEntry>, Vec<Rejection>) {
        let mut allowed = Vec::new();
        let mut rejected = Vec::new();
        for id in requested {
            match self.check_service(id) {
                ServiceVerdict::Allowed(e) => allowed.push(e),
                ServiceVerdict::Denied(reason) => {
                    warn!(service = %id, reason, "service request denied by safe-list");
                    rejected.push(Rejection {
                        id: id.clone(),
                        kind: RejectionKind::Denied,
                        reason: reason.to_owned(),
                    });
                }
                ServiceVerdict::Unlisted => {
                    warn!(service = %id, "service not in safe-list, ignoring");
                    rejected.push(Rejection {
                        id: id.clone(),
                        kind: RejectionKind::Unlisted,
                        reason: "service not in curated safe-list".into(),
                    });
                }
            }
        }
        (allowed, rejected)
    }

    /// Like `partition_services` but for processes.
    pub fn partition_processes<'a>(
        &'a self,
        requested: &'a [String],
    ) -> (Vec<&'a SafeProcessEntry>, Vec<Rejection>) {
        let mut allowed = Vec::new();
        let mut rejected = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for exe in requested {
            // Dedupe case-insensitively — users might double-list by accident.
            if !seen.insert(exe.to_ascii_lowercase()) {
                continue;
            }
            match self.check_process(exe) {
                ProcessVerdict::Allowed(e) => allowed.push(e),
                ProcessVerdict::Denied(reason) => {
                    warn!(process = %exe, reason, "process request denied by safe-list");
                    rejected.push(Rejection {
                        id: exe.clone(),
                        kind: RejectionKind::Denied,
                        reason: reason.to_owned(),
                    });
                }
                ProcessVerdict::Unlisted => {
                    warn!(process = %exe, "process not in safe-list, ignoring");
                    rejected.push(Rejection {
                        id: exe.clone(),
                        kind: RejectionKind::Unlisted,
                        reason: "process not in curated safe-list".into(),
                    });
                }
            }
        }
        (allowed, rejected)
    }
}

#[derive(Debug)]
pub enum ServiceVerdict<'a> {
    Allowed(&'a SafeServiceEntry),
    Denied(&'a str),
    Unlisted,
}

#[derive(Debug)]
pub enum ProcessVerdict<'a> {
    Allowed(&'a SafeProcessEntry),
    Denied(&'a str),
    Unlisted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    pub id: String,
    pub kind: RejectionKind,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RejectionKind {
    /// Explicitly denied — denylist wins over allowlist.
    Denied,
    /// Not present anywhere in the safe-list.
    Unlisted,
    /// The action is documented but framesage doesn't have a clean user-mode
    /// implementation on this Windows version, so the planner refuses to claim
    /// it ran. Surfaces as a visible rejection instead of a silent no-op.
    NotImplemented,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_safe_list_parses() {
        // The whole point of vendoring JSON in-tree: if a contributor breaks
        // the format, this test fires before anything else.
        let list = SafeList::from_bundled().expect("bundled JSON parses");
        assert!(!list.services.is_empty(), "no services listed");
        assert!(!list.processes.is_empty(), "no processes listed");
        assert!(!list.service_denied.is_empty(), "no service denylist");
        assert!(!list.process_denied.is_empty(), "no process denylist");
    }

    #[test]
    fn av_and_anticheat_services_are_explicitly_denied() {
        let list = SafeList::bundled();
        for must_deny in [
            "WinDefend",
            "MpsSvc",
            "vgc",
            "EasyAntiCheat",
            "BEService",
            "AudioSrv",
            "Dhcp",
            "Dnscache",
        ] {
            match list.check_service(must_deny) {
                ServiceVerdict::Denied(_) => {}
                other => panic!("{must_deny} must be denied, got {other:?}"),
            }
        }
    }

    #[test]
    fn shell_and_kernel_processes_are_explicitly_denied() {
        let list = SafeList::bundled();
        for must_deny in [
            "MsMpEng.exe",
            "explorer.exe",
            "winlogon.exe",
            "csrss.exe",
            "lsass.exe",
            "services.exe",
        ] {
            match list.check_process(must_deny) {
                ProcessVerdict::Denied(_) => {}
                other => panic!("{must_deny} must be denied, got {other:?}"),
            }
        }
    }

    #[test]
    fn service_lookups_are_case_insensitive() {
        let list = SafeList::bundled();
        match list.check_service("sysmain") {
            ServiceVerdict::Allowed(e) => assert_eq!(e.id, "SysMain"),
            other => panic!("expected allowed, got {other:?}"),
        }
        match list.check_service("SYSMAIN") {
            ServiceVerdict::Allowed(e) => assert_eq!(e.id, "SysMain"),
            other => panic!("expected allowed, got {other:?}"),
        }
    }

    #[test]
    fn process_lookups_are_case_insensitive() {
        let list = SafeList::bundled();
        match list.check_process("ONEDRIVE.EXE") {
            ProcessVerdict::Allowed(_) => {}
            other => panic!("expected allowed, got {other:?}"),
        }
    }

    #[test]
    fn unlisted_service_reports_unlisted() {
        let list = SafeList::bundled();
        match list.check_service("ThisServiceDoesNotExist") {
            ServiceVerdict::Unlisted => {}
            other => panic!("expected Unlisted, got {other:?}"),
        }
    }

    #[test]
    fn partition_separates_allowed_denied_and_unlisted() {
        let list = SafeList::bundled();
        let requested = vec![
            "SysMain".to_string(),    // allow
            "WinDefend".to_string(),  // deny
            "MadeUp1234".to_string(), // unlisted
            "WSearch".to_string(),    // allow
            "MpsSvc".to_string(),     // deny
        ];
        let (allowed, rejected) = list.partition_services(&requested);
        assert_eq!(allowed.len(), 2);
        assert_eq!(rejected.len(), 3);

        let denied_count = rejected
            .iter()
            .filter(|r| r.kind == RejectionKind::Denied)
            .count();
        let unlisted_count = rejected
            .iter()
            .filter(|r| r.kind == RejectionKind::Unlisted)
            .count();
        assert_eq!(denied_count, 2);
        assert_eq!(unlisted_count, 1);
    }

    #[test]
    fn partition_processes_dedupes_case_insensitively() {
        let list = SafeList::bundled();
        let requested = vec![
            "OneDrive.exe".to_string(),
            "onedrive.exe".to_string(),
            "ONEDRIVE.EXE".to_string(),
        ];
        let (allowed, rejected) = list.partition_processes(&requested);
        assert_eq!(allowed.len(), 1, "dedup should collapse case variants");
        assert!(rejected.is_empty());
    }

    #[test]
    fn singleton_bundled_returns_same_instance() {
        let a = SafeList::bundled() as *const _;
        let b = SafeList::bundled() as *const _;
        assert!(std::ptr::eq(a, b));
    }
}
