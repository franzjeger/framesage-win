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

/// Normalize a raw process input string to the canonical denylist lookup
/// key.
///
/// Deliberately dumb: trim whitespace, split on both `\` and `/`, take the
/// trailing path component, lowercase. That's the whole thing. No `\\?\`
/// extended-length-prefix parsing, no extension-alias logic — the
/// `\\?\C:\Windows\System32\csrss.exe` variant passes precisely *because*
/// the normalizer doesn't understand path syntax; it just keeps the last
/// segment.
///
/// **Do NOT "improve" this to parse `\\?\` prefixes or to strip extensions
/// beyond what [`strip_dot_exe`] already handles at storage construction
/// time** — either change reintroduces over-matching, which is exactly
/// what the denylist exists to prevent. The invariant is: normalization
/// may only ever make matching *more aggressive*, never less.
fn normalize_process_key(input: &str) -> String {
    input
        .trim()
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Strip a literal trailing `.exe` (case-insensitive). Returns `None` if
/// there is no `.exe` suffix to strip, so callers can decide whether a
/// dual key is warranted.
///
/// Strips ONLY `.exe`, not `.bat` / `.cmd` / `.scr` / etc — dual-key
/// insertion is a hardening for the one extension a kernel-critical
/// binary normally uses, not a general extension-alias map.
///
/// Preserves the middle of multi-dot names: `NVDisplay.Container.exe` →
/// `NVDisplay.Container`. The trailing `.exe` is removed; inner dots
/// stay.
///
/// Requires the stripped stem to be strictly non-empty (`s.len() > 4`,
/// not `>= 4`) — `.exe`-only input returns `None` instead of an empty
/// stem, which would otherwise let an empty key slip into
/// `process_denied` and match any empty / whitespace-only lookup. The
/// caller can rely on the returned `&str` being non-empty.
///
/// Byte-slice note: `s[s.len() - 4..]` would panic if the final 4 bytes
/// straddle a multi-byte UTF-8 boundary (e.g. a non-ASCII character
/// whose encoding ends within the last 4 bytes). All real process
/// names + denylist entries are ASCII, so this is safe in practice;
/// flagging it here so a future contributor adding non-ASCII names
/// knows to switch to a `char`-aware suffix check.
fn strip_dot_exe(s: &str) -> Option<&str> {
    if s.len() > 4 && s[s.len() - 4..].eq_ignore_ascii_case(".exe") {
        Some(&s[..s.len() - 4])
    } else {
        None
    }
}

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
            // Insert under BOTH the normalized key and its `.exe`-stripped
            // form. This makes every kernel-critical name match regardless
            // of whether the caller passes "csrss.exe", "csrss", a path-
            // prefixed spelling, etc. — the trailing-component normalizer
            // (above) handles paths; the dual key handles extension-
            // optional input. `strip_dot_exe` only strips a literal
            // `.exe`, so multi-dot names like "NVDisplay.Container.exe"
            // produce keys "nvdisplay.container.exe" + "nvdisplay.container"
            // (the inner dot is preserved).
            //
            // If the entry has no `.exe` to strip, only the primary key
            // is inserted. The bidirectional canonical-list test (in
            // tests::denylist_matrix) is what keeps this storage
            // structure honest against the JSON content.
            let key = normalize_process_key(&d.name);
            if let Some(stripped) = strip_dot_exe(&key) {
                process_denied.insert(stripped.to_string(), d.rationale.clone());
            }
            process_denied.insert(key, d.rationale);
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
    /// allow, an explanation on deny, or `Unlisted` if the id isn't listed.
    ///
    /// Trims input whitespace before lowercasing. Real production service
    /// IDs never carry whitespace, so this is defense-in-depth + symmetry
    /// with the process side's normalizer: a caller that accidentally
    /// passes `"  WinDefend  "` (e.g. from a hand-edited YAML profile
    /// that preserved indentation) still gets a Denied verdict, matching
    /// the process-side behavior. Service IDs have no path component, so
    /// no `\`/`/` splitting is needed.
    pub fn check_service(&self, id: &str) -> ServiceVerdict<'_> {
        let key = id.trim().to_ascii_lowercase();
        if let Some(reason) = self.service_denied.get(&key) {
            return ServiceVerdict::Denied(reason);
        }
        match self.services.get(&key) {
            Some(entry) => ServiceVerdict::Allowed(entry),
            None => ServiceVerdict::Unlisted,
        }
    }

    /// Yes-this-process-may-be-suspended check.
    ///
    /// Normalizes the input via [`normalize_process_key`] (trim, split on
    /// `\`/`/`, take last segment, lowercase) before probing the denylist
    /// and allowlist. Combined with the dual-key denylist storage from
    /// [`SafeList::from_bundled`], this means every kernel-critical name
    /// is denied regardless of how the caller spells it — case, with or
    /// without `.exe`, path-prefixed, `\\?\` extended-length, leading or
    /// trailing whitespace.
    pub fn check_process(&self, exe: &str) -> ProcessVerdict<'_> {
        let key = normalize_process_key(exe);
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

    /// Names (lower-cased, normalized) of every process this list explicitly
    /// denies. Other safety-critical code paths — like ProBalance's dynamic
    /// priority restraint — consult this so they never touch dwm, audiodg,
    /// csrss, kernel-mode drivers, anti-cheat, AV, or other entries the
    /// curated denylist flagged as dangerous to perturb.
    ///
    /// After PR 1's denylist hardening, every `.exe` entry contributes
    /// **both** its with-extension form (e.g. `csrss.exe`) and its
    /// `.exe`-stripped form (e.g. `csrss`) to the iterator — see
    /// [`SafeList::from_bundled`]. Consumers building a membership set
    /// from this iterator (e.g. `engine/src/lib.rs:462`) inherit both
    /// spellings automatically, so the engine matches kernel-critical
    /// names regardless of whether the caller passes the extension.
    pub fn denied_process_names(&self) -> impl Iterator<Item = &str> {
        self.process_denied.keys().map(String::as_str)
    }

    /// Symmetric counterpart to [`SafeList::denied_process_names`] for the
    /// services denylist. Returned IDs are lower-cased; service IDs have
    /// no path component and no extension, so this iterator emits each
    /// entry exactly once (no dual-key inflation).
    pub fn denied_service_names(&self) -> impl Iterator<Item = &str> {
        self.service_denied.keys().map(String::as_str)
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
            // Dedupe by normalized key — collapses case variants AND
            // path-prefixed variants (so e.g. "csrss.exe" and
            // "C:\Windows\System32\csrss.exe" count as one).
            if !seen.insert(normalize_process_key(exe)) {
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

impl ServiceVerdict<'_> {
    /// `true` iff this verdict is [`ServiceVerdict::Denied`]. Convenience for
    /// callsites that don't need the rationale string — the denylist-matrix
    /// tests use it heavily.
    pub fn is_denied(&self) -> bool {
        matches!(self, ServiceVerdict::Denied(_))
    }
}

#[derive(Debug)]
pub enum ProcessVerdict<'a> {
    Allowed(&'a SafeProcessEntry),
    Denied(&'a str),
    Unlisted,
}

impl ProcessVerdict<'_> {
    /// `true` iff this verdict is [`ProcessVerdict::Denied`]. Convenience for
    /// callsites that don't need the rationale string — the denylist-matrix
    /// tests use it heavily.
    pub fn is_denied(&self) -> bool {
        matches!(self, ProcessVerdict::Denied(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Rejection {
    pub id: String,
    pub kind: RejectionKind,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
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
            "FACEITService",
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
    fn anticheat_user_mode_processes_are_explicitly_denied() {
        let list = SafeList::bundled();
        for must_deny in ["faceitservice.exe", "faceitclient.exe"] {
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

    #[test]
    fn strip_dot_exe_rejects_exe_only_input() {
        // Boundary guard: `.exe` (len exactly 4) must NOT strip to
        // `Some("")` — that would insert an empty-string key into
        // `process_denied` and let any empty/whitespace input
        // `check_process` to a spurious Denied verdict. Stem must be
        // strictly non-empty.
        assert_eq!(
            strip_dot_exe(".exe"),
            None,
            ".exe-only must not strip to empty"
        );
        assert_eq!(
            strip_dot_exe(".EXE"),
            None,
            "case variant of .exe-only must not strip"
        );
        assert_eq!(strip_dot_exe(""), None, "empty input must return None");
        assert_eq!(
            strip_dot_exe("a.exe"),
            Some("a"),
            "1-char stem is the smallest valid case"
        );
        assert_eq!(strip_dot_exe("csrss.exe"), Some("csrss"));
        assert_eq!(
            strip_dot_exe("NVDisplay.Container.exe"),
            Some("NVDisplay.Container"),
            "multi-dot middle preserved"
        );
        assert_eq!(strip_dot_exe("noext"), None);
    }

    /// Hardening matrix for the denylist + normalizer. Enforces that every
    /// kernel-critical name is `Denied` regardless of how the caller
    /// spells it (case, with/without `.exe`, path-prefixed,
    /// `\\?\`-prefixed, whitespace), and that the two code paths that
    /// consult the denylist (the `check_process` / `check_service`
    /// lookups, and the engine's pre-computed `safe_list_denied_exes`
    /// set built from `denied_process_names()` — see
    /// `engine/src/lib.rs:462`) agree about which names are denied.
    ///
    /// The bidirectional canonical-list tests are the load-bearing pieces:
    /// they catch "JSON entry added without matrix coverage" and "JSON
    /// entry removed without matrix update" symmetrically. Drift is
    /// forbidden unless it goes through `EXEMPT_FROM_MATRIX_PROCESSES`
    /// or `EXEMPT_FROM_MATRIX_SERVICES` (typed per list so a process
    /// exemption can never satisfy a service-side check).
    mod denylist_matrix {
        use super::*;
        use std::collections::HashSet;

        // Canonical list of kernel-critical process names — must match
        // the `processes.json` denylist 1:1 modulo
        // `EXEMPT_FROM_MATRIX_PROCESSES`. Names are spelled with `.exe`
        // and lowercased to match the primary form stored in
        // `process_denied`. Adding a new denylist entry without adding
        // it here trips the bidirectional test; removing one without
        // removing it here does the same.
        const KERNEL_CRITICAL_PROCESSES: &[&str] = &[
            // Defender / Security Center
            "msmpeng.exe",
            "nissrv.exe",
            "securityhealthservice.exe",
            // Shell + compositor + audio + font
            "explorer.exe",
            "dwm.exe",
            "audiodg.exe",
            "fontdrvhost.exe",
            // Kernel-adjacent / logon / SCM
            "winlogon.exe",
            "csrss.exe",
            "lsass.exe",
            "services.exe",
            "smss.exe",
            "wininit.exe",
            // Shell infrastructure / UWP hosts
            "sihost.exe",
            "applicationframehost.exe",
            "runtimebroker.exe",
            "lockapp.exe",
            "startmenuexperiencehost.exe",
            "shellexperiencehost.exe",
            "textinputhost.exe",
            "searchhost.exe",
            "ctfmon.exe",
            // GPU driver hosts
            "nvcontainer.exe",
            "nvdisplay.container.exe",
            "atiesrxx.exe",
            "radeonsoftware.exe",
            // Anti-cheat (user-mode). Issue #148 G2: the Vanguard /
            // EAC / BattlEye hosts mirror `ac_detect.rs`'s marker list
            // (minus ESEAClient — ESEA migrated to FACEIT AC in 2023).
            "faceitservice.exe",
            "faceitclient.exe",
            "faceit_ac.exe",
            "faceit_start_protected_game.exe",
            "vgc.exe",
            "vgtray.exe",
            "easyanticheat.exe",
            "easyanticheat_eos.exe",
            "beservice.exe",
            "beservicelauncher.exe",
        ];

        // Canonical list of kernel-critical service IDs — must match the
        // `services.json` denylist 1:1 modulo
        // `EXEMPT_FROM_MATRIX_SERVICES`. Lowercased to match the storage
        // key form. The JSON includes both `AudioSrv` and `Audiosrv` as
        // case-variant aliases; both collapse to `audiosrv` in
        // `service_denied`, so the canonical list has it once.
        const KERNEL_CRITICAL_SERVICES: &[&str] = &[
            // AV / firewall
            "windefend",
            "mpssvc",
            // Anti-cheat
            "vgc",
            "easyanticheat",
            "easyanticheat_eos",
            "beservice",
            "faceitservice",
            // Network
            "dhcp",
            "dnscache",
            "nlasvc",
            // Audio
            "audiosrv",
            "audioendpointbuilder",
            // COM / RPC
            "rpcss",
            "rpceptmapper",
            "dcomlaunch",
            // Auth / profile / group policy
            "gpsvc",
            "samss",
            "profsvc",
            // Theming (affects compositor)
            "themes",
        ];

        // Typed per-list exemptions. A process exemption MUST NOT satisfy
        // a service-side check and vice versa — that's the entire reason
        // these are typed.
        //
        // Empty in this PR. Add `(name, reason)` pairs for any future
        // legitimately-asymmetric entry (e.g. a service that exists only
        // on some Windows versions but stays in JSON for compatibility,
        // or a process kept in the canonical list for hypothetical
        // future inclusion in the JSON).
        const EXEMPT_FROM_MATRIX_PROCESSES: &[(&str, &str)] = &[];
        const EXEMPT_FROM_MATRIX_SERVICES: &[(&str, &str)] = &[];

        /// Derive the JSON-denied-process canonical-form set from the
        /// parsed `SafeList`. After PR 1's dual-key storage every `.exe`
        /// entry is keyed under both forms; we keep only the primary
        /// `.exe` spelling for direct comparison against
        /// `KERNEL_CRITICAL_PROCESSES`.
        fn json_denied_processes() -> HashSet<String> {
            SafeList::bundled()
                .denied_process_names()
                .filter(|s| s.ends_with(".exe"))
                .map(String::from)
                .collect()
        }

        /// Derive the JSON-denied-service set. Services have no extension
        /// dual-key inflation, so this is a direct view.
        fn json_denied_services() -> HashSet<String> {
            SafeList::bundled()
                .denied_service_names()
                .map(String::from)
                .collect()
        }

        // ── (1) Bidirectional canonical-list tests ───────────────────────

        #[test]
        fn process_denylist_matches_canonical_bidirectionally() {
            let json = json_denied_processes();
            let canonical: HashSet<String> = KERNEL_CRITICAL_PROCESSES
                .iter()
                .map(|s| (*s).to_string())
                .collect();
            let exempt: HashSet<String> = EXEMPT_FROM_MATRIX_PROCESSES
                .iter()
                .map(|(n, _)| (*n).to_string())
                .collect();

            let json_minus: HashSet<_> = json.difference(&exempt).cloned().collect();
            let canon_minus: HashSet<_> = canonical.difference(&exempt).cloned().collect();

            let missing_from_canon: Vec<_> = json_minus.difference(&canon_minus).cloned().collect();
            assert!(
                missing_from_canon.is_empty(),
                "processes.json denylist entries are missing from \
                 KERNEL_CRITICAL_PROCESSES (or EXEMPT_FROM_MATRIX_PROCESSES). \
                 Add them to one or the other — silent drift is forbidden. \
                 Missing: {missing_from_canon:?}"
            );
            let missing_from_json: Vec<_> = canon_minus.difference(&json_minus).cloned().collect();
            assert!(
                missing_from_json.is_empty(),
                "KERNEL_CRITICAL_PROCESSES entries are missing from \
                 processes.json denylist (or EXEMPT_FROM_MATRIX_PROCESSES). \
                 Add them to one or the other — silent drift is forbidden. \
                 Missing: {missing_from_json:?}"
            );
        }

        #[test]
        fn service_denylist_matches_canonical_bidirectionally() {
            let json = json_denied_services();
            let canonical: HashSet<String> = KERNEL_CRITICAL_SERVICES
                .iter()
                .map(|s| (*s).to_string())
                .collect();
            let exempt: HashSet<String> = EXEMPT_FROM_MATRIX_SERVICES
                .iter()
                .map(|(n, _)| (*n).to_string())
                .collect();

            let json_minus: HashSet<_> = json.difference(&exempt).cloned().collect();
            let canon_minus: HashSet<_> = canonical.difference(&exempt).cloned().collect();

            let missing_from_canon: Vec<_> = json_minus.difference(&canon_minus).cloned().collect();
            assert!(
                missing_from_canon.is_empty(),
                "services.json denylist entries are missing from \
                 KERNEL_CRITICAL_SERVICES (or EXEMPT_FROM_MATRIX_SERVICES). \
                 Missing: {missing_from_canon:?}"
            );
            let missing_from_json: Vec<_> = canon_minus.difference(&json_minus).cloned().collect();
            assert!(
                missing_from_json.is_empty(),
                "KERNEL_CRITICAL_SERVICES entries are missing from \
                 services.json denylist (or EXEMPT_FROM_MATRIX_SERVICES). \
                 Missing: {missing_from_json:?}"
            );
        }

        // ── (2) Variant matrix over every kernel-critical process ───────

        /// Build every spelling we expect the engine, IPC layer, or a UI
        /// caller could plausibly pass. The engine receives bare NTQSI
        /// filenames (no paths), but IPC / planner consumers may pass
        /// path-prefixed or `\\?\` extended-length variants from
        /// arbitrary user-authored policy. Every spelling must be
        /// Denied.
        fn process_variants(canonical_name: &str) -> Vec<String> {
            let stem = strip_dot_exe(canonical_name).unwrap_or(canonical_name);
            vec![
                canonical_name.to_string(),                                // csrss.exe
                stem.to_string(),                                          // csrss
                canonical_name.to_ascii_uppercase(),                       // CSRSS.EXE
                stem.to_ascii_uppercase(),                                 // CSRSS
                format!("C:\\Windows\\System32\\{canonical_name}"),        // backslash path
                format!("/usr/bin/{canonical_name}"),                      // forward-slash path
                format!("\\\\?\\C:\\Windows\\System32\\{canonical_name}"), // \\?\ extended
                format!("  {canonical_name}  "),                           // whitespace
                format!("\t{}\t", canonical_name.to_ascii_uppercase()),    // tabs + case
            ]
        }

        #[test]
        fn every_kernel_critical_process_denied_under_every_variant() {
            let list = SafeList::bundled();
            for name in KERNEL_CRITICAL_PROCESSES {
                for variant in process_variants(name) {
                    assert!(
                        list.check_process(&variant).is_denied(),
                        "{name}: variant {variant:?} must be Denied"
                    );
                }
            }
        }

        // ── Bisect targets: single-test diagnostics so a future failure
        //    points at one entry, not the whole matrix ───────────────────

        #[test]
        fn nvdisplay_container_denied_with_and_without_extension() {
            // Multi-dot name: strip_dot_exe must remove only the trailing
            // ".exe", not the inner dot. All four spellings must be
            // Denied via the dual-key denylist storage + normalizer.
            let list = SafeList::bundled();
            assert!(list.check_process("NVDisplay.Container.exe").is_denied());
            assert!(list.check_process("NVDisplay.Container").is_denied());
            assert!(list.check_process("nvdisplay.container").is_denied());
            assert!(list.check_process("NVDISPLAY.CONTAINER.EXE").is_denied());
        }

        #[test]
        fn path_prefixed_csrss_denied() {
            // The normalizer is deliberately dumb — it splits on `\` and
            // `/` and takes the last segment. The `\\?\` extended-length
            // prefix matches precisely because the normalizer doesn't
            // understand path syntax: it just keeps "csrss.exe".
            let list = SafeList::bundled();
            assert!(list
                .check_process("C:\\Windows\\System32\\csrss.exe")
                .is_denied());
            assert!(list
                .check_process(r"\\?\C:\Windows\System32\csrss.exe")
                .is_denied());
            assert!(list
                .check_process("C:/Windows/System32/csrss.exe")
                .is_denied());
        }

        // ── (3) Partition return-type matrix ────────────────────────────

        #[test]
        fn partition_processes_returns_denied_not_unlisted_for_every_variant() {
            let list = SafeList::bundled();
            let mut requested = Vec::new();
            for name in KERNEL_CRITICAL_PROCESSES {
                for v in process_variants(name) {
                    requested.push(v);
                }
            }
            let (allowed, rejected) = list.partition_processes(&requested);
            assert!(
                allowed.is_empty(),
                "kernel-critical names must never be Allowed; got {} entries",
                allowed.len()
            );
            for r in &rejected {
                assert_eq!(
                    r.kind,
                    RejectionKind::Denied,
                    "{}: expected RejectionKind::Denied, got {:?}",
                    r.id,
                    r.kind
                );
            }
        }

        #[test]
        fn partition_services_returns_denied_not_unlisted_for_every_variant() {
            let list = SafeList::bundled();
            let mut requested = Vec::new();
            for id in KERNEL_CRITICAL_SERVICES {
                // Services have no path component; variants are case +
                // whitespace only.
                requested.push((*id).to_string());
                requested.push(id.to_ascii_uppercase());
                requested.push(format!("  {id}  "));
                // Title-case first letter for "Capitalized" variant.
                let mut chars = id.chars();
                if let Some(first) = chars.next() {
                    let capped: String = first.to_ascii_uppercase().to_string() + chars.as_str();
                    requested.push(capped);
                }
            }
            let (allowed, rejected) = list.partition_services(&requested);
            assert!(
                allowed.is_empty(),
                "kernel-critical service IDs must never be Allowed; got {} entries",
                allowed.len()
            );
            for r in &rejected {
                assert_eq!(
                    r.kind,
                    RejectionKind::Denied,
                    "{}: expected RejectionKind::Denied, got {:?}",
                    r.id,
                    r.kind
                );
            }
        }

        // ── (4) Engine-side symmetry tests ──────────────────────────────

        /// Build the denied-process set the way `engine/src/lib.rs:462`
        /// does at runtime: iterate `denied_process_names()`, lowercase,
        /// collect into a `HashSet<String>`. The engine's
        /// `safe_list_denied_exes` is an `Arc<HashSet<String>>` of
        /// exactly this shape, so the test set is bit-for-bit
        /// equivalent.
        fn engine_style_denied_set() -> HashSet<String> {
            SafeList::bundled()
                .denied_process_names()
                .map(|n| n.to_ascii_lowercase())
                .collect()
        }

        #[test]
        fn engine_style_denied_set_contains_every_kernel_critical() {
            let engine_set = engine_style_denied_set();
            for name in KERNEL_CRITICAL_PROCESSES {
                // Engine receives bare NTQSI filenames (no paths).
                // Variants: case × with/without `.exe`.
                let stem = strip_dot_exe(name).unwrap_or(name);
                let variants = [
                    (*name).to_string(),
                    stem.to_string(),
                    name.to_ascii_uppercase(),
                    stem.to_ascii_uppercase(),
                ];
                for v in &variants {
                    assert!(
                        engine_set.contains(&v.to_ascii_lowercase()),
                        "engine-style set is missing variant {v:?} for {name} \
                         — engine ProBalance restraint would touch this \
                         kernel-critical process"
                    );
                }
            }
        }

        #[test]
        fn engine_set_membership_matches_check_process_verdict() {
            let list = SafeList::bundled();
            let engine_set = engine_style_denied_set();
            for name in KERNEL_CRITICAL_PROCESSES {
                let stem = strip_dot_exe(name).unwrap_or(name);
                for v in [
                    (*name).to_string(),
                    stem.to_string(),
                    name.to_ascii_uppercase(),
                    stem.to_ascii_uppercase(),
                ] {
                    let in_engine = engine_set.contains(&v.to_ascii_lowercase());
                    let check_denied = list.check_process(&v).is_denied();
                    assert_eq!(
                        in_engine, check_denied,
                        "denylist-path asymmetry for {v:?}: engine_set says \
                         {in_engine}, check_process says denied={check_denied}. \
                         The two code paths that gate dangerous actions MUST \
                         agree."
                    );
                }
            }
        }

        // ── (5) Negative direction: normalization must not over-match ───

        #[test]
        fn normalization_never_makes_an_allowed_name_denied() {
            // For every allow-listed process entry, every variant spelling
            // must report something OTHER than Denied. (May be Allowed
            // for exact-form variants, Unlisted for path-prefixed
            // variants if the allowlist isn't path-normalised; the
            // load-bearing invariant is "not Denied" — normalization may
            // only make matching more aggressive on the DENYLIST, never
            // less safe for legitimately allowed names.)
            let list = SafeList::bundled();
            for entry in list.processes() {
                let name = &entry.exe;
                let stem = strip_dot_exe(name).unwrap_or(name);
                let variants = [
                    name.clone(),
                    stem.to_string(),
                    name.to_ascii_uppercase(),
                    stem.to_ascii_uppercase(),
                    format!("C:\\Some\\Path\\{name}"),
                    format!("  {name}  "),
                ];
                for v in &variants {
                    assert!(
                        !list.check_process(v).is_denied(),
                        "allow-listed entry {name} variant {v:?} reported \
                         Denied — normalization made matching unsafe"
                    );
                }
            }
        }

        #[test]
        fn empty_or_extension_only_input_not_denied_via_phantom_key() {
            // Pairs with `strip_dot_exe_rejects_exe_only_input` (the
            // unit test). That one asserts `strip_dot_exe` refuses the
            // boundary; this one asserts the full lookup chain
            // doesn't produce a Denied verdict for inputs that
            // *would* match a phantom empty key if one ever slipped
            // into `process_denied`. Defense-in-depth: even if a
            // future change re-introduces the boundary bug, this
            // test fires before a real denial can mis-fire.
            let list = SafeList::bundled();
            for input in ["", "   ", ".exe", ".EXE", " \t.exe "] {
                assert!(
                    !list.check_process(input).is_denied(),
                    "empty/whitespace/extension-only input {input:?} \
                     reported Denied — phantom empty key in process_denied"
                );
            }
        }
    }
}
