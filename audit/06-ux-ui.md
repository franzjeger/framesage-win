# FrameSage Tray UX/UI Audit

Scope: `crates/tray/src/{main.rs, tree.rs, formatters.rs, theme.rs, icons.rs}`.
Comparators: Process Lasso (information density, sticky affinity, sparkline-per-process is absent there too), Process Explorer / System Informer (tree, columns, per-process history), Task Manager (sparseness baseline).

Severity: [HIGH] = blocking competitive parity, [MED] = visible polish gap, [LOW] = nice-to-have.

---

## What is genuinely well-done

- **Shell icons per row** (`icons.rs`, `main.rs:3039-3059`) with a per-frame extraction budget (4) and negative caching. Most egui apps skip this; the row instantly reads as "a process viewer," not "a debug overlay."
- **Tree view with cycle-safe DFS** (`tree.rs:203-270`, plus inline tests). Roots fall back to "orphan" when the parent PID is missing — handles short-lived shells correctly.
- **Row-state classification** with a leading 3px coloured gutter (`main.rs:3019-3032`, `tree.rs:35-56`): foreground=accent, ProBalance-restrained=warning, managed=success. Reads at a glance without painting whole rows, which would be screamy.
- **X3D-aware affinity submenu** (`main.rs:3233-3327`) plus session-sticky "Remember as rule" checkbox — the bridge between one-shot and persistent pin is the cleanest such affordance I've seen. Process Lasso forces a separate dialog for the same flow.
- **Multi-select with Ctrl/Shift** (`main.rs:3765-3806`), evaluated at end-of-frame so modifier state is captured at click time, with bulk actions in the context menu (`main.rs:3417-3497`). Matches Task Manager / Process Explorer convention exactly.
- **Persistent affinity rule indicator** — 📌 prefix on the affinity cell when a rule exists for the exe (`main.rs:3600-3625`), tooltip explains state, single-click opens picker.
- **Single-instance show-window signal** + cross-session foreground reporter — non-UX but explains why a second launch raises the existing window instead of failing silently.
- **Working-set hover tooltip** showing peak + private bytes (`main.rs:3577-3583`) — the leak-detection signal Task Manager hides.
- **Tray tooltip is live** (`main.rs:714-721`, `formatters.rs:147-162`) — two-line "FrameSage — state / Active profile · Foreground" updated only when the formatted string changes (gated to avoid a Win32 round-trip per frame).

---

## Findings

### 1. Density & scannability of the process list [MED]

`main.rs:2965-3010`, `theme.rs:104-118`.

- Row height **18px**, header **20px** (`main.rs:3011, 2983`). Process Lasso ships ~16px rows; Task Manager ~22px. FrameSage sits closer to Lasso — good.
- Body font is **13.5pt Proportional** (`theme.rs:110`). Lasso uses ~9pt Segoe UI in its grid; Process Explorer ~8pt Tahoma. FrameSage at 13.5pt is **noticeably airy** — visually closer to Task Manager than to a pro tool. On a 1440p monitor this trades 30-40% of vertical row count for legibility. PID/CPU/Memory/Threads cells render `ui.monospace()` at the default monospace 12.5pt (`main.rs:3559-3587`), which helps numeric alignment, but the proportional Description/Company columns still dominate the line height.
- Default column widths (`main.rs:2969-2982`): Process **220 + at_least 120**, Description **200**, Company **140**, User **140**, PID **60**, CPU% **60**, Memory **85**, Threads **55**, Priority **85**, Affinity **110**, Profile **100**, Status remainder. Sum (sans marker+icon) = ~1255px before remainder. Combined with the global `MAX_CONTENT_WIDTH = 980.0` cap on the central panel (`main.rs:878`), **the table will overflow horizontally on any first run** until the user drags column widths or stretches the window. This is a hard-felt papercut.
- `ui.set_max_width(MAX_CONTENT_WIDTH)` at `main.rs:878-879` applies to ALL tabs including Processes — for a process viewer this is the wrong choice. A 3440-wide ultrawide gets ~980 of useful columns and a sea of empty space on the right, while the data the user wants is hidden behind a horizontal scroll.
- Colour usage is restrained and well-tiered: muted/text/warning/error gradient on CPU% (`formatters.rs:39-46`), accent only on foreground/managed, warning only on restraint. This is correct — Lasso over-paints; FrameSage gets it right.
- **No alternating-row contrast tweak** — `striped(true)` is enabled (`main.rs:2966`) but on the dark theme the stripe is barely visible. Worth a custom stripe colour.

### 2. Sorting / filtering / grouping [MED]

`main.rs:2766-2833`, `main.rs:4013-4038`.

- **Sort by header**: yes, all columns except Affinity and Status (`main.rs:2987-3006`). `sortable_header()` handles toggling and uses ASCII `^`/`v` arrows — pragmatic given the default font has no triangle glyph coverage, but the result reads as code rather than UI. Worth shipping a font with arrow glyphs (Process Lasso uses Wingdings tricks for the same reason).
- **Default sort = CPU desc** (`main.rs:239`). Correct — every comparable tool does this.
- **Filter** is a single substring textbox over `exe_name` only (`main.rs:2852-2854`). Process Explorer / System Informer let you search across description, command line, owning user, PID. FrameSage's filter ignores Description/Company/User even though those columns exist. Typing "microsoft" finds nothing useful. [MED]
- **Group by** (user, parent class, status): not present. The tree view substitutes for "group by parent." No group-by-user view, which would be a legitimate feature for separating SYSTEM rows from your stuff.
- Filter forces flat mode (`main.rs:2780-2787`), which is the right call and clearly communicated via disabled-checkbox hover. Good.

### 3. Search [MED]

`main.rs:2769`.

- Substring only, case-insensitive (via `to_ascii_lowercase`). No regex, no fuzzy, no per-column. The textbox is 200px wide and lives in the toolbar — adequate for "find chrome" but useless for "find anything launched by steam."
- The Activity tab uses identical substring search (`main.rs:1821-1826`).
- No keyboard shortcut to focus the filter (Ctrl-F). On a process viewer this is conspicuous — Task Manager binds it.

### 4. Per-process history [HIGH]

Searched the entire tray crate: no `per_pid_history`, no `cpu_history` map, no sparkline-per-process state. Only the **system-wide** 60-sample ring buffer (`main.rs:65-71, 5966-5969`).

- Process Explorer keeps per-PID CPU history and shows it in the "Performance Graph" tab of the per-process properties dialog. Process Lasso ships a column-level mini-graph in the History view. FrameSage's process detail panel (`main.rs:4143-4341`) shows current values only — no sparkline, no min/max over the session.
- The detail panel has the real estate (210px default height, draggable splitter). Adding a 60-sample CPU sparkline per selected PID would be cheap; the per-process polling already happens every 1 s.
- This is the largest single feature gap vs. Process Explorer.

### 5. WHY did the app do something? — audit log [MED]

`main.rs:1758-1874` (Activity tab), `main.rs:5842-5896` (event ingestion).

- Activity tab has Time / Kind / Event columns; filter chips per kind (Foreground / Engine / ProBalance demote / ProBalance restore / Other); substring search; "Clear log" button (`main.rs:1802-1804`).
- Event labels are decent — `"<exe> -> <profile> (pid N)"` for foreground, `"probalance restrained <exe> (pid N) from_class -> to_class"` for demotes (`main.rs:5849-5879`).
- But: **the event does not record which rule matched**. The foreground event says "bf6.exe -> game-x3d" but never "...because rule #3 (path contains 'Battlefield 6')". The data exists upstream (`matched_rule_note` is on `ProcessSnapshot`), but the IPC event stream doesn't carry it. So the "why" remains implicit. [MED]
- No log persistence — 1000-entry in-memory cap (`main.rs:5892-5896`), evicted oldest-first, wiped on tray restart. Lasso writes its log to disk. A user who reboots loses the trail of why their machine slowed down before bed.
- Clicking an event row does nothing — no jump-to-PID, no jump-to-rule. Should at minimum offer "jump to rule that triggered this." [LOW]

### 6. Undo [HIGH]

Searched for `undo`, `revert`, `history`, `rollback` — nothing in tray. The only revert is `Request::GameModeOff` (the panic button at `main.rs:1249, 1373`) which is GameMode-specific.

- If FrameSage suspended OneDrive as part of `game_mode.suspend_processes`, there is **no per-action undo**. You either:
  1. Click "Game Mode off (panic)" — reverts the whole session.
  2. Right-click the suspended PID → Resume (`main.rs:3461-3466`).

  Option 1 is heavy; option 2 requires the user to know WHICH processes were touched. There is no diff view of "Game Mode is currently holding: [list, with per-row Resume]."
- A per-process priority set has no undo at all — once you click "Set priority → High," there is no "revert to prior class" memory.
- Manual override has an explicit Clear (`main.rs:1262-1264`), so that one is fine.

This is **the biggest trust gap**. Users won't experiment with a tool they can't unwind.

### 7. Rule editor clarity [MED]

`main.rs:1878-2203` (rules tab), `main.rs:3209-3232` (right-click → Create rule), `main.rs:4270-4280` (detail panel → Create rule).

- Right-click → "Create rule for this exe" → submenu of profiles → click → rule appears (`main.rs:3844-3880`). Persisted immediately. Good.
- **"Add rule for foreground"** button on the Rules tab (`main.rs:1933-1961`) pre-fills the form from the current foreground app — directly addresses the "what exe name do I even type" pain. Excellent.
- **Per-match-kind "Use foreground X" shortcut** inside the rule form (`main.rs:2041-2059`) is the cherry on top.
- **Two parallel rule systems** (`AppRule` for profiles, `AffinityRule` for affinity) are co-located in the Rules tab but visually disjoint (`main.rs:2205-2321`). The affinity rules section sits below a `ui.separator()` and uses a different visual treatment (TableBuilder vs `ui.horizontal` rows). At a glance they look like two unrelated features. Worth unifying.
- Affinity rule creation only happens from the Processes tab — the Rules tab is read-only for affinity (`main.rs:2238-2244` empty-state CTA literally says "open the Processes tab"). Discoverable but split across tabs.
- The rule list itself (`main.rs:2142-2178`) renders as `"exe  bf6.exe  ->  game-x3d  (note)"` in a plain `ui.label()`. Process Lasso uses a real grid. The current treatment is hard to scan with >10 rules.

### 8. Tray behavior [MED]

`main.rs:5632-5776`.

- Left-click toggles window (`main.rs:5753-5773`). Single-click, not double — correct convention. Right-click opens menu (handled by `tray-icon` crate).
- Menu contents (`main.rs:5635-5672`): Open/Hide · Pause/Resume/GameModeOff · View → (Processes/Status/Rules/Profiles) · Open config / Edit policy · Exit. **This is excellent.** Most utilities ship "Open / Quit"; FrameSage's tray menu has every common one-click action plus tab-jump.
- Tray tooltip is live and informative (`main.rs:714-721`).
- No Activity tab in the View submenu (`main.rs:5650-5654`) — minor omission. [LOW]

### 9. Notifications [MED]

Grepped for `notify`, `Notification`, `toast`, `winrt` — **none in tray**. No earned toasts for "we stopped a service your foreground app needs," no noisy ones for "applied profile X."

- The Activity strip at the bottom (`main.rs:4832-4863`) is the only in-app surfacing.
- Lasso uses Windows balloon tips for ProBalance demotes; some users hate them, some rely on them. The absence here is a defensible choice (no spam) but a power user might want opt-in for the one toast that matters: "Game Mode failed to start service X back up — click to retry." [LOW]

### 10. Dark mode [LOW]

`theme.rs:40` starts with `Visuals::dark()` and overrides every interactive role. **There is no light mode.** No toggle, no system-theme follow.

- For a power-user dark-by-default utility this is fine and arguably correct — Process Lasso ships a light default that almost everyone hates.
- But the absence of even a follow-system toggle will read as missing to reviewers. [LOW]

### 11. DPI scaling [LOW]

egui handles DPI via `ctx.pixels_per_point()`. No explicit handling in this crate — relies on egui defaults. Mixed-DPI multi-monitor: egui handles per-viewport scale changes since 0.27, but the fixed `MAX_CONTENT_WIDTH = 980.0` (`main.rs:878`) is logical pixels, so it scales correctly. Per-core matrix bar width is `PER_CORE_BAR_W = 5.0` (`main.rs:4661`) — also logical, fine.

No reason to expect breakage. Untested in-source, though.

### 12. Accessibility [HIGH for screen-readers, MED for keyboard]

- **Keyboard nav on the table is effectively absent.** No arrow keys to move between rows; clicked_pid is set only on `Response::clicked()` from a mouse event (`main.rs:3144-3147`). No `Sense::focusable()` plumbing on row labels.
- **Tab order** falls back to egui's default focus ring — for a multi-panel layout with menubar + toolbar + tab strip + table + detail panel, that order is unpredictable.
- **Enter/Space to activate** a focused row: not wired.
- **Esc to close the modal**: `render_terminate_confirm_modal` (`main.rs:898-952`) and `render_affinity_picker_modal` (`main.rs:959-`) don't handle Esc explicitly. egui's `Window` with `collapsible(false).resizable(false)` doesn't auto-bind Esc.
- **Screen reader**: egui has accessibility-kit support (off by default in eframe 0.28). Not enabled here. NVDA / JAWS see the window as one big canvas. This is a known egui limitation — worth a comment in the README rather than fixing here.

### 13. First-run experience [HIGH]

No onboarding code path. No `first_run`, no welcome dialog, no empty-state CTA on the Status tab beyond "Waiting for the service to respond…" (`main.rs:1479`).

- A fresh install lands on the Processes tab (`Tab::default() = Processes`, `main.rs:158-165`). The user sees a table they didn't ask for and no explanation of what FrameSage does.
- Empty Rules tab shows "(no rules — add one to map a foreground app to a profile)" (`main.rs:2137`). One line. No CTA chip, no "click here to create your first rule," no example.
- Empty affinity rules shows a longer explanation (`main.rs:2239-2244`) — but it's a wall of text. No button.
- The Status tab IS the right landing page for first run: hero strip, profile summary, foreground card, ProBalance card, quick actions. Default tab should be Status, not Processes. [MED — easy fix]

### 14. Admin-prompt clarity [Good with one snag]

`main.rs:1675-1714`, `win32.rs:241-264`.

- Unelevated: a yellow warning banner at the top of the Status tab "Read-only mode — Pause, Resume, and Game Mode controls need admin" with an "Enable controls (UAC)…" button (`main.rs:1689-1707`).
- Click → `relaunch_as_admin` → ShellExecute("runas") → on UAC accept, the unelevated instance exits, the elevated child takes over. On UAC decline, error message echoes via `last_action`. Solid.
- The Rules and Profiles tabs show the same banner via `render_readonly_banner` (`main.rs:1884-1890`, `main.rs:4537-4549`). Consistent — good.
- One snag: the relaunched elevated instance loses your in-progress edits. The `policy_draft` is per-process, not persisted. If the user opened the Rules tab unelevated, started drafting a rule, then clicked the elevation banner — the draft is gone after the relaunch. [LOW, but surprising]

### 15. Performance band [Good]

`main.rs:4579-4761`.

- Permanent strip above every tab: `CPU %` (colour-graded), `MEM %` (graded) + bytes, **per-core matrix** (`main.rs:4670-4714`, 5px bars, hover-tooltip per core), **60s sparkline** with CPU+memory overlay.
- Aggregate CPU% has a hover-tooltip showing top-5 hottest cores via `format_top_cores` (`main.rs:4603`) — solves the X3D "did the load land on the right CCD" question in one glance.
- This band is meaningfully better than the equivalents in Lasso (no sparkline) and Task Manager (no per-core bar matrix in this footprint). Genuinely useful, not eye-candy. ~28px of vertical, restrained.

### 16. Process detail panel [MED]

`main.rs:4143-4341`.

- Card with title + PID badge + close (✕). Two columns: Metrics (CPU/Memory/Threads/Priority/Affinity) on the left; FrameSage state (Profile/User/Rule note/ProBalance) on the right. Action row below: Set priority / Apply profile / Create rule / Set affinity / Suspend / Resume / Terminate.
- Splitter bar between table and detail panel (`main.rs:3705-3734`) with proper cursor change and accent-tint when dragged. Persists for the session in `detail_height`. Good ergonomics.
- **Missing**: command line, parent exe name (just parent PID is on the snapshot), thread list, open handles, .NET / WinUI module info — i.e. all the Process Explorer reasons-to-open-the-properties-dialog. The current panel is a slightly-richer hover tooltip.
- **No per-PID sparkline** in this panel — see Finding #4.
- The action row duplicates the right-click menu (`main.rs:4252-4339`) but **without** the "Show in Explorer / Copy / Suspend tree / Trim working set" actions. Asymmetric.

### 17. Multi-select [Good]

`main.rs:3765-3806`, `main.rs:3154-3164`.

- Ctrl-click toggles; Shift-click range-extends from the last anchor; plain click clears and toggles single. Range select operates in the current visual sort order. Right-click on a multi-selected row dispatches to every selected PID; right-click outside the selection acts only on the clicked PID. Matches Task Manager / Process Explorer exactly.
- Bulk context-menu labels are pluralised ("Suspend 4 processes", "Terminate 4 processes…") (`main.rs:3444-3473`).
- Multi-select highlight is a translucent fill (`main.rs:3127-3135`); selected-for-detail row gets an accent stroke (`main.rs:3136-3143`). Two distinct visual states, no ambiguity.
- Caveat: "Apply profile now" in bulk falls back to ApplyProfileForeground (single-shot to whichever process is foreground) (`main.rs:3193-3207`) — there's a code comment acknowledging the IPC for per-PID apply isn't built. Functional gap, not a UX gap, but the menu lies about what it does.

---

## Top-priority backlog (if I were the PM)

1. **Per-PID CPU sparkline in the detail panel** — closes the largest competitive gap vs. Process Explorer. Polling already happens; just store a ring buffer per selected PID.
2. **Undo / revert log** — at minimum: "Show what Game Mode is currently holding" with per-row revert, plus per-priority-change undo. Without this the app is scary.
3. **Default landing tab = Status, not Processes.** One-line change. Massively better first-run.
4. **Filter searches description / company / user**, not just exe name. Bind Ctrl-F to focus it.
5. **Drop the 980px content-width cap on the Processes tab** — it actively hurts the most data-dense view.
6. **Smaller default body font on the table only** — drop to ~11.5pt in the table, keep 13.5pt elsewhere. Gets ~25% more rows on screen without hurting other tabs.
7. **Record matched-rule index in the IPC event** so the Activity log says "...because rule #3 matched" — closes the "why did it do that" loop.
8. **Persist the activity log to disk** — even just a JSONL append.

Word count: ~1730.
