//! Process-tree data structures + sort logic for the Processes tab.
//!
//! Pulls the depth-first tree builder, the per-row state classifier, the
//! sort-key enum, and the comparator out of `main.rs` so the heavy UI
//! rendering paths in `main.rs` stay readable. Everything in here is
//! `(plain inputs) → plain output` — no egui state, no FramesageApp
//! coupling — which is what lets the tests at the bottom of this file
//! stand on their own.

use eframe::egui;

use framesage_ipc::ProcessSnapshot;

use crate::theme;

// ─── Row classification ──────────────────────────────────────────────────────

/// Visual classification for a Processes-tab row. Drives the colored leading
/// marker column, the exe-name color, and (indirectly) several glyph
/// prefixes. Order matters in `classify_row` — the cases are checked in
/// priority sequence so the most "interesting" state wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowState {
    /// The currently-focused application. Wins over every other state — the
    /// user wants the foreground row to be obvious at a glance.
    Foreground,
    /// ProBalance has demoted this process for contention relief.
    Restrained,
    /// A profile is currently applied (a rule matched, or manual mode).
    Managed,
    /// Plain background process — no special framesage involvement.
    Default,
}

pub fn classify_row(p: &ProcessSnapshot, foreground_pid: Option<u32>) -> RowState {
    if foreground_pid == Some(p.pid) {
        RowState::Foreground
    } else if p.restrained_by_probalance {
        RowState::Restrained
    } else if p.managed_profile.is_some() {
        RowState::Managed
    } else {
        RowState::Default
    }
}

/// Color for the leading vertical marker bar. `None` means "draw nothing"
/// (default rows stay clean so the colored rows pop visually).
pub fn row_marker_color(state: RowState) -> Option<egui::Color32> {
    match state {
        RowState::Foreground => Some(theme::ACCENT),
        RowState::Restrained => Some(theme::WARNING),
        RowState::Managed => Some(theme::SUCCESS),
        RowState::Default => None,
    }
}

/// Color for the exe-name column. Foreground gets the accent; ProBalance-
/// restrained gets warning. Managed keeps default text — the leading marker
/// and the Profile column already say "managed" loud enough; over-coloring
/// the name on every managed row makes the list feel screamy.
pub fn row_exe_color(state: RowState) -> egui::Color32 {
    match state {
        RowState::Foreground => theme::ACCENT,
        RowState::Restrained => theme::WARNING,
        RowState::Managed | RowState::Default => theme::TEXT,
    }
}

// ─── Sort key + comparator ───────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessSortKey {
    ExeName,
    Pid,
    Cpu,
    Memory,
    Threads,
    Priority,
    Profile,
    Description,
    Company,
    User,
}

/// Long-form hover text for each Processes-tab column header. Same
/// "explain what this is" affordance every Process Lasso column has.
/// Returned by-value (not `&'static str`) so callers can compose dynamic
/// hints in the future without churning every call site.
pub fn column_hover_text(key: ProcessSortKey) -> &'static str {
    match key {
        ProcessSortKey::ExeName => {
            "Executable filename. Click to sort alphabetically; click again to flip direction."
        }
        ProcessSortKey::Pid => {
            "Process ID. Stable within a process's lifetime, reused after exit."
        }
        ProcessSortKey::Cpu => {
            "Live CPU usage as % of one logical processor. Sort descending to find the noisy one fast."
        }
        ProcessSortKey::Memory => {
            "Working-set size — resident RAM the process is using right now. Hover the cell for peak + private bytes."
        }
        ProcessSortKey::Threads => "Live thread count.",
        ProcessSortKey::Priority => {
            "Windows priority class. Game Mode + manual overrides can change this from its default."
        }
        ProcessSortKey::Profile => {
            "Profile FrameSage has applied to this PID. Star ★ marks profiles that came from a Rule (vs one-shot)."
        }
        ProcessSortKey::Description => {
            "Friendly name from the exe's version resource (\"Microsoft Edge\" beside msedge.exe)."
        }
        ProcessSortKey::Company => {
            "Publisher string from the version resource. Useful for spotting unfamiliar binaries."
        }
        ProcessSortKey::User => {
            "User account that owns the process. SYSTEM / NT SERVICE rows render muted so user-owned code stands out."
        }
    }
}

/// Compare two `ProcessSnapshot`s by the chosen sort key + direction.
///
/// Single source of truth for both the flat-mode sort and the per-sibling
/// sort inside `build_tree_view`. `None` for `sort_by` means "preserve
/// input order" (= `Equal` for every pair) so callers can opt out without
/// branching at the call site.
pub fn compare_snapshots(
    a: &ProcessSnapshot,
    b: &ProcessSnapshot,
    sort_by: Option<ProcessSortKey>,
    desc: bool,
) -> std::cmp::Ordering {
    let Some(key) = sort_by else {
        return std::cmp::Ordering::Equal;
    };
    let ord = match key {
        ProcessSortKey::ExeName => a
            .exe_name
            .to_ascii_lowercase()
            .cmp(&b.exe_name.to_ascii_lowercase()),
        ProcessSortKey::Pid => a.pid.cmp(&b.pid),
        ProcessSortKey::Cpu => a.cpu_percent.cmp(&b.cpu_percent),
        ProcessSortKey::Memory => a.memory_bytes.cmp(&b.memory_bytes),
        ProcessSortKey::Threads => a.threads.cmp(&b.threads),
        ProcessSortKey::Priority => a.priority_class_raw.cmp(&b.priority_class_raw),
        ProcessSortKey::Profile => a.managed_profile.cmp(&b.managed_profile),
        ProcessSortKey::Description => {
            // Case-insensitive; None collates after Some so labelled rows
            // cluster together regardless of direction.
            let av = a.description.as_deref().unwrap_or("\u{ffff}");
            let bv = b.description.as_deref().unwrap_or("\u{ffff}");
            av.to_ascii_lowercase().cmp(&bv.to_ascii_lowercase())
        }
        ProcessSortKey::Company => {
            let av = a.company.as_deref().unwrap_or("\u{ffff}");
            let bv = b.company.as_deref().unwrap_or("\u{ffff}");
            av.to_ascii_lowercase().cmp(&bv.to_ascii_lowercase())
        }
        ProcessSortKey::User => {
            let av = a.user.as_deref().unwrap_or("\u{ffff}");
            let bv = b.user.as_deref().unwrap_or("\u{ffff}");
            av.to_ascii_lowercase().cmp(&bv.to_ascii_lowercase())
        }
    };
    if desc {
        ord.reverse()
    } else {
        ord
    }
}

// ─── Tree-view builder ───────────────────────────────────────────────────────

/// One visible row in tree mode. The flat table iterates these instead of
/// the raw `Vec<ProcessSnapshot>`; the depth controls indentation and the
/// `has_children` flag controls whether the ▶/▼ toggle renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeRow {
    pub pid: u32,
    /// Index into the unsorted `rows: &[ProcessSnapshot]` slice the builder
    /// was given. Lets the renderer fetch the underlying snapshot without
    /// a second hash lookup.
    pub row_index: usize,
    pub depth: u8,
    pub has_children: bool,
}

/// Build a depth-first flattened view of the process tree, respecting the
/// user's collapsed set.
///
/// Edges come from `ProcessSnapshot::parent_pid`. A process is a root when
/// its parent is `0` OR points at a PID not present in `rows` (the parent
/// has exited but the kernel hasn't reaped the orphan). Cycles — which
/// shouldn't be possible but a stale PID-reuse race could theoretically
/// produce — are broken via a DFS stack: if we'd revisit a PID already
/// on the path, we skip rather than loop.
///
/// Children of each parent (and the root list) are sorted by `cmp`, so
/// the chosen ProcessSortKey applies within siblings — matches the
/// Process Explorer / Process Lasso convention.
pub fn build_tree_view(
    rows: &[ProcessSnapshot],
    collapsed: &std::collections::HashSet<u32>,
    cmp: impl Fn(&ProcessSnapshot, &ProcessSnapshot) -> std::cmp::Ordering,
) -> Vec<TreeRow> {
    use std::collections::HashMap;
    let mut by_pid: HashMap<u32, usize> = HashMap::with_capacity(rows.len());
    for (i, r) in rows.iter().enumerate() {
        by_pid.insert(r.pid, i);
    }
    let mut children: HashMap<u32, Vec<usize>> = HashMap::new();
    let mut roots: Vec<usize> = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        let parent_alive = r.parent_pid != 0 && by_pid.contains_key(&r.parent_pid);
        if parent_alive {
            children.entry(r.parent_pid).or_default().push(i);
        } else {
            roots.push(i);
        }
    }
    // Sort within siblings and at the root.
    for v in children.values_mut() {
        v.sort_by(|&a, &b| cmp(&rows[a], &rows[b]));
    }
    roots.sort_by(|&a, &b| cmp(&rows[a], &rows[b]));

    let mut out = Vec::with_capacity(rows.len());
    let mut stack: Vec<u32> = Vec::new();
    for r in roots {
        visit_tree(r, 0, rows, &children, collapsed, &mut out, &mut stack);
    }
    out
}

fn visit_tree(
    i: usize,
    depth: u8,
    rows: &[ProcessSnapshot],
    children: &std::collections::HashMap<u32, Vec<usize>>,
    collapsed: &std::collections::HashSet<u32>,
    out: &mut Vec<TreeRow>,
    stack: &mut Vec<u32>,
) {
    let pid = rows[i].pid;
    if stack.contains(&pid) {
        // Cycle: shouldn't happen in practice (the kernel's parent linkage
        // doesn't make loops), but a malformed snapshot could trip us up.
        // Drop silently rather than loop forever.
        return;
    }
    let has_children = children.get(&pid).map(|v| !v.is_empty()).unwrap_or(false);
    out.push(TreeRow {
        pid,
        row_index: i,
        depth,
        has_children,
    });
    if !has_children || collapsed.contains(&pid) {
        return;
    }
    stack.push(pid);
    if let Some(kids) = children.get(&pid) {
        for &c in kids {
            visit_tree(c, depth + 1, rows, children, collapsed, out, stack);
        }
    }
    stack.pop();
}

/// Collect `root_pid` and every PID transitively descended from it through
/// the current snapshot's parent-PID edges. DFS with a visited-set guards
/// against cycles (which shouldn't exist in real kernel state but a
/// PID-reuse race could theoretically produce). Returns the root first;
/// suspending in that order means a parent's signal-handling is paused
/// before its child's exit propagates back up the tree.
pub fn descendants_of(rows: &[ProcessSnapshot], root_pid: u32) -> Vec<u32> {
    use std::collections::{HashMap, HashSet, VecDeque};
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for r in rows {
        children.entry(r.parent_pid).or_default().push(r.pid);
    }
    let mut out = Vec::new();
    let mut seen: HashSet<u32> = HashSet::new();
    let mut q: VecDeque<u32> = VecDeque::new();
    q.push_back(root_pid);
    while let Some(pid) = q.pop_front() {
        if !seen.insert(pid) {
            continue;
        }
        out.push(pid);
        if let Some(kids) = children.get(&pid) {
            for &c in kids {
                q.push_back(c);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn make_proc(pid: u32) -> ProcessSnapshot {
        ProcessSnapshot {
            pid,
            parent_pid: 0,
            exe_name: "test.exe".into(),
            exe_path: String::new(),
            description: None,
            company: None,
            user: None,
            priority_class_raw: 0x20, // NORMAL_PRIORITY_CLASS
            affinity_mask: 0xFFFF,
            cpu_percent: 0,
            memory_bytes: 0,
            peak_working_set_bytes: 0,
            private_bytes: 0,
            threads: 1,
            matched_rule_note: None,
            managed_profile: None,
            restrained_by_probalance: false,
        }
    }

    #[test]
    fn classify_row_foreground_beats_everything() {
        // Even if a process is also restrained or managed, the foreground
        // designation wins — the user expects the focused row to read as
        // "this is what you're using right now."
        let mut p = make_proc(42);
        p.restrained_by_probalance = true;
        p.managed_profile = Some("game-x3d".into());
        assert_eq!(classify_row(&p, Some(42)), RowState::Foreground);
    }

    #[test]
    fn classify_row_restrained_beats_managed() {
        let mut p = make_proc(7);
        p.restrained_by_probalance = true;
        p.managed_profile = Some("perf".into());
        assert_eq!(classify_row(&p, Some(1)), RowState::Restrained);
    }

    #[test]
    fn classify_row_managed_when_only_profile() {
        let mut p = make_proc(7);
        p.managed_profile = Some("perf".into());
        assert_eq!(classify_row(&p, Some(1)), RowState::Managed);
    }

    #[test]
    fn classify_row_default_otherwise() {
        let p = make_proc(7);
        assert_eq!(classify_row(&p, Some(1)), RowState::Default);
        assert_eq!(classify_row(&p, None), RowState::Default);
    }

    #[test]
    fn row_marker_default_paints_nothing() {
        // Default rows leave the marker column empty so the colored ones
        // pop. Locking this in — accidental "always-on" would defeat the
        // gutter's purpose.
        assert!(row_marker_color(RowState::Default).is_none());
        assert!(row_marker_color(RowState::Foreground).is_some());
        assert!(row_marker_color(RowState::Restrained).is_some());
        assert!(row_marker_color(RowState::Managed).is_some());
    }

    fn proc_with_parent(pid: u32, parent_pid: u32) -> ProcessSnapshot {
        let mut p = make_proc(pid);
        p.parent_pid = parent_pid;
        p
    }

    #[test]
    fn build_tree_view_groups_children_under_parents() {
        // Tree:
        //   1 (root)
        //     └ 2
        //         └ 4
        //     └ 3
        //   5 (root, parent=99 missing → orphan = root)
        let rows = vec![
            proc_with_parent(1, 0),
            proc_with_parent(2, 1),
            proc_with_parent(3, 1),
            proc_with_parent(4, 2),
            proc_with_parent(5, 99),
        ];
        let collapsed = HashSet::new();
        let tree = build_tree_view(&rows, &collapsed, |a, b| a.pid.cmp(&b.pid));
        // With ascending-PID sort within siblings, expected DFS order is:
        // 1, 2, 4, 3, 5
        let pids: Vec<u32> = tree.iter().map(|t| t.pid).collect();
        assert_eq!(pids, vec![1, 2, 4, 3, 5]);
        let depths: Vec<u8> = tree.iter().map(|t| t.depth).collect();
        assert_eq!(depths, vec![0, 1, 2, 1, 0]);
    }

    #[test]
    fn build_tree_view_respects_collapsed() {
        // Collapsing pid 1 should hide 2, 3, 4 — they all sit under 1.
        let rows = vec![
            proc_with_parent(1, 0),
            proc_with_parent(2, 1),
            proc_with_parent(3, 1),
            proc_with_parent(4, 2),
            proc_with_parent(5, 0),
        ];
        let mut collapsed = HashSet::new();
        collapsed.insert(1);
        let tree = build_tree_view(&rows, &collapsed, |a, b| a.pid.cmp(&b.pid));
        let pids: Vec<u32> = tree.iter().map(|t| t.pid).collect();
        assert_eq!(pids, vec![1, 5]);
        // Pid 1 still has children even though they're hidden; the toggle
        // glyph needs to render.
        assert!(tree[0].has_children);
        assert!(!tree[1].has_children);
    }

    #[test]
    fn build_tree_view_treats_missing_parents_as_orphans() {
        // PID 7's parent (42) isn't in the snapshot → 7 should be a root.
        let rows = vec![proc_with_parent(1, 0), proc_with_parent(7, 42)];
        let tree = build_tree_view(&rows, &HashSet::new(), |a, b| a.pid.cmp(&b.pid));
        assert_eq!(
            tree.iter()
                .filter(|t| t.depth == 0)
                .map(|t| t.pid)
                .collect::<Vec<_>>(),
            vec![1, 7]
        );
    }

    #[test]
    fn build_tree_view_breaks_cycles() {
        // Pathological cycle (shouldn't happen in real kernel state but
        // defending against it): 1 → 2 → 1.
        let rows = vec![proc_with_parent(1, 2), proc_with_parent(2, 1)];
        let tree = build_tree_view(&rows, &HashSet::new(), |a, b| a.pid.cmp(&b.pid));
        // Both have a "parent" present so neither is a top-level root by
        // our normal rule — `roots` ends up empty. The output should be
        // empty rather than infinite-looping.
        assert!(tree.is_empty());
    }

    #[test]
    fn build_tree_view_sort_applies_to_siblings_only() {
        // Sort by reverse pid; expect siblings to flip but tree structure
        // intact.
        let rows = vec![
            proc_with_parent(1, 0),
            proc_with_parent(2, 1),
            proc_with_parent(3, 1),
        ];
        let tree = build_tree_view(&rows, &HashSet::new(), |a, b| b.pid.cmp(&a.pid));
        let pids: Vec<u32> = tree.iter().map(|t| t.pid).collect();
        // Root is unchanged (only one root); siblings of 1 emit in
        // reverse-PID order → 3, then 2.
        assert_eq!(pids, vec![1, 3, 2]);
    }

    #[test]
    fn descendants_of_includes_root_and_walks_subtree() {
        // Tree:
        //   1
        //     2
        //        4
        //     3
        //   5
        let rows = vec![
            proc_with_parent(1, 0),
            proc_with_parent(2, 1),
            proc_with_parent(3, 1),
            proc_with_parent(4, 2),
            proc_with_parent(5, 0),
        ];
        let mut got = descendants_of(&rows, 1);
        got.sort_unstable();
        assert_eq!(got, vec![1, 2, 3, 4]);
        // Calling on a leaf returns just the leaf.
        assert_eq!(descendants_of(&rows, 4), vec![4]);
    }

    #[test]
    fn descendants_of_handles_cycle_without_looping() {
        let rows = vec![proc_with_parent(1, 2), proc_with_parent(2, 1)];
        let got = descendants_of(&rows, 1);
        // Both PIDs visited exactly once.
        assert_eq!(got.len(), 2);
        assert!(got.contains(&1));
        assert!(got.contains(&2));
    }
}
