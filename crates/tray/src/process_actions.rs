//! Item 3.6 (fifth slice — final) — process-row action surface lifted
//! out of main.rs.
//!
//! What lives here:
//!
//! * `ProcessAction` enum — pending context-menu / detail-button
//!   click captured during the render pass; dispatched after the
//!   render closure releases its borrow on `FramesageApp`. Every
//!   right-click action and every detail-card button funnels
//!   through this enum so the dispatch path is a single match
//!   statement back in main.rs.
//! * `PRIORITY_CHOICES` — labeled priority levels used by the
//!   per-row "Set priority" submenu. Order matches Task Manager
//!   (high → low).
//! * `render_process_detail` — the selected-process detail panel
//!   rendered below the Processes table. Mirrors the right-click
//!   context menu so users who never right-click still discover the
//!   actions.

use eframe::egui;

use framesage_core::PriorityClass;
use framesage_ipc::ProcessSnapshot;

use crate::formatters::{format_bytes, priority_class_label};
use crate::theme;
use crate::widgets::{detail_kv, draw_single_sparkline};

/// Pending context-menu click captured during the render pass; dispatched
/// after the render closure releases its borrow on `self`.
pub(crate) enum ProcessAction {
    SetPriority {
        pid: u32,
        class: PriorityClass,
    },
    ApplyProfileForeground {
        profile: String,
    },
    CreateRule {
        exe_name: String,
        profile: String,
    },
    Suspend {
        pid: u32,
    },
    Resume {
        pid: u32,
    },
    TrimWorkingSet {
        pid: u32,
    },
    /// Opens the Terminate confirmation modal — DOES NOT directly send the
    /// IPC. The modal's "Confirm" button is what fires the actual request.
    /// Captured separately from `Suspend` / `Resume` because we never want
    /// a misclick to nuke a process.
    RequestTerminate {
        pid: u32,
        exe_name: String,
    },
    /// One-shot affinity pin using a topology-aware selector (Kind(Cache)
    /// for X3D, Kind(Performance) for non-X3D, All for reset, etc.).
    /// Engine resolves against live `CpuTopology`.
    ///
    /// When `save_as_rule_for` is `Some(exe)`, the live pin is followed by
    /// a `SetAffinityRule` so the same selector is re-applied next time a
    /// process with that exe spawns. This is how the "Remember as rule"
    /// submenu checkbox upgrades a one-shot pick into a persistent rule.
    SetAffinity {
        pid: u32,
        selector: framesage_core::CpuSelector,
        save_as_rule_for: Option<String>,
    },
    /// Delete the persistent affinity rule for `exe_name`. Dispatched from
    /// the per-row affinity badge's context menu and from the Affinity
    /// Rules manager view's Remove button. Doesn't touch any currently-
    /// running matching processes — the live pin persists until the
    /// process exits (matches Process Lasso).
    DeleteAffinityRule {
        exe_name: String,
    },
    /// Add / remove `exe_name` from the ProBalance user-ignore list so the
    /// engine never restrains it. `exclude` picks the direction (the
    /// context menu shows whichever the current state calls for). Persists
    /// via the service so it survives a restart.
    SetProBalanceExclusion {
        exe_name: String,
        exclude: bool,
    },
    /// Opens the custom-mask affinity picker modal for `pid`. The modal's
    /// Apply button is what fires the actual `SetAffinity` IPC with the
    /// user-built mask. `existing_rule_selector` pre-loads the picker mask
    /// from the persistent rule (if one exists) so editing an existing
    /// rule starts from its current state, not the live process's mask.
    RequestAffinityPicker {
        pid: u32,
        exe_name: String,
    },
    /// Reveal the process's exe in Explorer (selects the file in its folder).
    /// Pure shell-out via `explorer.exe /select,<path>`; nothing to round-
    /// trip through the service.
    ShowInExplorer {
        path: String,
    },
    /// Copy a string to the clipboard. The egui `Context::copy_text` helper
    /// owns the clipboard plumbing; we just package the string here so the
    /// dispatch loop can call it after the render closure releases its borrow.
    CopyToClipboard {
        text: String,
    },
    /// Suspend an entire subtree: the parent PID + every descendant
    /// reachable via parent-PID edges in the current snapshot. Captured
    /// here as a single action so the dispatch loop expands it once
    /// against the live snapshot — a stale tree from a prior frame would
    /// be wrong by the time we send.
    SuspendTree {
        root_pid: u32,
    },
}

/// (display label, enum value) pairs used by the per-row priority submenu.
/// Order matches Task Manager's "Set priority" — high to low — which is
/// what users expect.
pub(crate) const PRIORITY_CHOICES: &[(&str, PriorityClass)] = &[
    ("High", PriorityClass::High),
    ("Above Normal", PriorityClass::AboveNormal),
    ("Normal", PriorityClass::Normal),
    ("Below Normal", PriorityClass::BelowNormal),
    ("Idle (lowest)", PriorityClass::Idle),
];

/// Selected-process detail panel rendered below the Processes table.
///
/// Layout: title bar with exe + pid + close button, then a two-column field
/// grid (key on the left in mono-muted, value on the right in plain text),
/// then a row of action buttons that mirror the right-click context menu.
/// The detail card is the discoverability surface for users who never
/// right-click — Process Lasso ships the same set of actions both ways for
/// exactly this reason.
///
/// `cpu_history` (item 3.4): per-PID CPU% sample history (newest at
/// the back), capped at `state::SYSTEM_HISTORY_LEN`. Empty when we
/// haven't seen this PID in any prior tick. Rendered as a sparkline
/// next to the CPU detail row.
pub(crate) fn render_process_detail(
    ui: &mut egui::Ui,
    pid: u32,
    rows: &[ProcessSnapshot],
    profile_ids: &[String],
    cpu_history: &[u8],
    action_queue: &mut Vec<ProcessAction>,
    close_flag: &mut bool,
) {
    let Some(p) = rows.iter().find(|p| p.pid == pid) else {
        // PID disappeared between snapshots — auto-close the panel rather
        // than render a misleading "unknown process" card.
        *close_flag = true;
        return;
    };

    theme::card().show(ui, |ui| {
        // Title row: exe name + PID badge + close.
        ui.horizontal(|ui| {
            ui.heading(&p.exe_name);
            theme::status_badge(theme::p().text_muted).show(ui, |ui| {
                ui.colored_label(theme::p().text_muted, format!("pid {}", p.pid));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("✕")
                    .on_hover_text("Close detail panel")
                    .clicked()
                {
                    *close_flag = true;
                }
            });
        });
        // Subtitle: the version-resource description, when the engine has
        // it. Sits directly under the heading so the relationship between
        // exe name and friendly name reads at a glance.
        if let Some(desc) = &p.description {
            ui.colored_label(theme::p().text_muted, desc);
        }
        ui.add_space(4.0);

        egui::ScrollArea::vertical()
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    // Left column: metrics.
                    ui.vertical(|ui| {
                        detail_kv(ui, "CPU", format!("{} %", p.cpu_percent));
                        // Item 3.4 — 60 s of CPU% history rendered as
                        // an inline sparkline. Tucked under the CPU
                        // row in the detail panel so the user can
                        // see whether the current % is a spike or a
                        // sustained load without leaving the panel.
                        // Skipped on first-tick (history.len() < 2)
                        // — draw_single_sparkline handles that by
                        // drawing just the background tray.
                        if !cpu_history.is_empty() {
                            let desired = egui::vec2(180.0, 22.0);
                            let (rect, _) = ui.allocate_exact_size(desired, egui::Sense::hover());
                            draw_single_sparkline(ui.painter(), rect, cpu_history);
                            ui.add_space(4.0);
                        }
                        // Working set + the supporting peak / private values.
                        // The "growth gap" between current working set and
                        // peak working set is the classic memory-leak signal.
                        let memory_summary = if p.peak_working_set_bytes > 0 || p.private_bytes > 0
                        {
                            format!(
                                "{}  (peak {} · private {})",
                                format_bytes(p.memory_bytes),
                                format_bytes(p.peak_working_set_bytes),
                                format_bytes(p.private_bytes),
                            )
                        } else {
                            format_bytes(p.memory_bytes)
                        };
                        detail_kv(ui, "Memory", memory_summary);
                        detail_kv(ui, "Threads", p.threads.to_string());
                        detail_kv(
                            ui,
                            "Priority",
                            priority_class_label(p.priority_class_raw).to_string(),
                        );
                        detail_kv(ui, "Affinity", format!("{:#018x}", p.affinity_mask));
                    });
                    ui.separator();
                    // Right column: framesage state.
                    ui.vertical(|ui| {
                        let profile_text = match &p.managed_profile {
                            Some(id) => {
                                if p.matched_rule_note.is_some() {
                                    format!("★ {id}  (Rule)")
                                } else {
                                    id.clone()
                                }
                            }
                            None => "—".to_string(),
                        };
                        detail_kv(ui, "Profile", profile_text);
                        detail_kv(ui, "User", p.user.as_deref().unwrap_or("—").to_string());
                        let rule_note = p
                            .matched_rule_note
                            .as_deref()
                            .filter(|n| !n.is_empty())
                            .unwrap_or("—");
                        detail_kv(ui, "Rule note", rule_note.to_string());
                        let probal = if p.restrained_by_probalance {
                            "● restrained"
                        } else {
                            "—"
                        };
                        detail_kv(ui, "ProBalance", probal.to_string());
                    });
                });
            });

        ui.add_space(6.0);
        ui.separator();
        ui.add_space(6.0);

        // Action row — same submenus as the table's right-click context menu.
        let exe = p.exe_name.clone();
        let pid = p.pid;
        ui.horizontal(|ui| {
            ui.menu_button("Set priority", |ui| {
                for (label, class) in PRIORITY_CHOICES.iter() {
                    if ui.button(*label).clicked() {
                        action_queue.push(ProcessAction::SetPriority { pid, class: *class });
                        ui.close_menu();
                    }
                }
            });
            ui.menu_button("Apply profile now", |ui| {
                for pid_name in profile_ids {
                    if ui.button(pid_name).clicked() {
                        action_queue.push(ProcessAction::ApplyProfileForeground {
                            profile: pid_name.clone(),
                        });
                        ui.close_menu();
                    }
                }
            });
            ui.menu_button("Create rule for this exe", |ui| {
                for pid_name in profile_ids {
                    if ui.button(pid_name).clicked() {
                        action_queue.push(ProcessAction::CreateRule {
                            exe_name: exe.clone(),
                            profile: pid_name.clone(),
                        });
                        ui.close_menu();
                    }
                }
            });
            ui.menu_button("Set affinity", |ui| {
                // Detail-panel affinity submenu mirrors the table's right-
                // click options but always dispatches one-shot pins
                // (`save_as_rule_for: None`). The persistent-rule flow
                // lives on the table's right-click submenu (with the
                // Remember toggle) and the picker (with the checkbox);
                // duplicating it here would clutter the action row
                // without adding capability.
                if ui.button("X3D CCD").clicked() {
                    action_queue.push(ProcessAction::SetAffinity {
                        pid,
                        selector: framesage_core::CpuSelector::Kind(
                            framesage_core::CoreKind::Cache,
                        ),
                        save_as_rule_for: None,
                    });
                }
                if ui.button("Non-X3D CCD").clicked() {
                    action_queue.push(ProcessAction::SetAffinity {
                        pid,
                        selector: framesage_core::CpuSelector::Kind(
                            framesage_core::CoreKind::Performance,
                        ),
                        save_as_rule_for: None,
                    });
                }
                if ui.button("All cores").clicked() {
                    action_queue.push(ProcessAction::SetAffinity {
                        pid,
                        selector: framesage_core::CpuSelector::All,
                        save_as_rule_for: None,
                    });
                }
                if ui.button("Custom…").clicked() {
                    action_queue.push(ProcessAction::RequestAffinityPicker {
                        pid,
                        exe_name: exe.clone(),
                    });
                }
            });
            ui.separator();
            if ui.button("Suspend").clicked() {
                action_queue.push(ProcessAction::Suspend { pid });
            }
            if ui.button("Resume").clicked() {
                action_queue.push(ProcessAction::Resume { pid });
            }
            if ui
                .add(egui::Button::new(
                    egui::RichText::new("Terminate…").color(theme::p().error),
                ))
                .clicked()
            {
                action_queue.push(ProcessAction::RequestTerminate {
                    pid,
                    exe_name: exe.clone(),
                });
            }
        });
    });
}
