# Audit 07 — Configurability

Scope: Are the right knobs exposed? Are dangerous knobs gated? Can a power
user actually configure this thing, and can a novice avoid bricking it?

## What's already done well

- **`Profile` is genuinely comprehensive** (`crates/core/src/profile.rs:144-198`):
  every Windows knob worth touching is here — CPU Sets, hard affinity, power
  throttling, priority class, IO/memory priority, working-set trim, persistence,
  and a full nested `GameModeActions`. Every field `Option<T>`-gated, so unset
  means "leave the OS default alone." Right shape.
- **`Realtime` is deliberately excluded** from `PriorityClass`
  (`crates/core/src/profile.rs:93-103`). Comment explicitly calls out the
  desktop-freeze risk. Major footgun closed at the type level — you literally
  cannot serialise a `Realtime` choice from this UI.
- **`AffinityRule` is the lightweight Process-Lasso-style flow**
  (`crates/core/src/policy.rs:77-89`) — exe-name keyed, persistent, with an
  explicit precedence rule over `Profile.cpu_sets`. Lets users pin Diablo IV
  to the X3D CCD without authoring a whole profile.
- **Hot-reload works and is debounced**
  (`crates/service/src/runtime.rs:130-195`). Atomic temp-file + rename in
  `Policy::save` (`crates/core/src/policy.rs:271-297`) means external editors
  can never see a torn file. UTF-8 BOM is tolerated
  (`crates/core/src/policy.rs:240`) — PowerShell-5.1 `Set-Content -Encoding UTF8`
  produces BOMs, which serde_json would otherwise reject; this is a real fix
  for the silent-fall-back-to-defaults footgun.
- **Curated safe-lists with denylists**
  (`crates/gamemode/src/safe_lists/services.json`,
  `crates/gamemode/src/safe_lists/processes.json`). Denylist covers
  Defender / Vanguard / EAC / BattlEye / DCOM / RPC / csrss / lsass /
  winlogon / dwm / explorer — the canonical "you'd brick the box" set. Each
  entry carries a rationale. Unknown ids are logged + skipped at apply time
  (`crates/tray/src/main.rs:5200, 5207`).
- **Terminate is gated by a confirm modal** with red "This is a hard kill"
  warning (`crates/tray/src/main.rs:898-952`).
- **Custom-affinity-picker rejects empty masks**
  (`crates/tray/src/main.rs:1121-1133`): the Apply button is disabled and a
  red "Pick at least one CPU" hint shows. Closes the 0x0-mask footgun for
  the live-process flow.
- **Profile delete is guarded against breaking the engine**
  (`crates/tray/src/main.rs:2631-2669`): can't delete default, background,
  active, manual, or rule-referenced profiles. Per-condition hover-text
  explains why.
- **Right-click → make rule exists and is discoverable**
  (`crates/tray/src/main.rs:3209-3232`): Processes tab → right-click row →
  "Create rule for this exe" → submenu of every profile. Plus the
  "Add rule for foreground" shortcut on the Rules toolbar
  (`crates/tray/src/main.rs:1931-1961`). Twin-flow covers both Lasso-style
  (from process list) and rule-tab-up-front discovery.
- **Focus Assist correctly disabled** with a "no documented Windows API"
  explanation in the Game Mode editor (`crates/tray/src/main.rs:5179-5194`)
  instead of a checkbox that silently does nothing.

## Footguns and gaps

### 1. Default game-x3d profile is aggressive — and BF6/Valorant/Fortnite are pre-seeded — Severity HIGH

`crates/core/src/policy.rs:454-470` ships rules pointing `bf6.exe`,
`VALORANT-Win64-Shipping.exe`, `FortniteClient-Win64-Shipping.exe` at the
30-service-stop, 24-process-suspend `game-x3d` profile by default. Two real
collateral-damage cases hiding in `policy.rs:355-391`:

- **`WSearch` stop breaks Outlook search.** Outlook (classic, not new) hosts
  its mail index in the Windows Search service. Stopping WSearch while a
  user has Outlook open and is alt-tabbing makes mail search return empty
  results until WSearch is restarted. Same applies to File Explorer's
  search-as-you-type. Service is auto-restored on profile exit, but
  mid-session searches will silently fail.
- **`BITS` + `DoSvc` + `WaaSMedicSvc` + `UsoSvc` stopped together**
  blocks Defender signature updates, Microsoft Store downloads, and Windows
  Update entirely for the session. Fine for a 1-hour game; a streamer who
  leaves a "game" running for an 8-hour shift hasn't received a signature
  update all day.
- **`ClickToRunSvc` stop** halts Office 365 background tasks; an Outlook send
  is queued, not sent, while the service is paused.
- **`OneDrive.exe` + `FileCoAuth.exe` suspend** while a user has a Word doc
  open via cloud-sync means saves don't replicate. The user thinks "saved"
  means saved-to-cloud; it doesn't until the game closes.

These are aggressive defaults, gated only by "did the user accidentally name
their unrelated work exe `bf6.exe`?" — which is unlikely, but the broader
miss is that the default rules ship pre-armed. A user who installs the tool
"just to look" and then launches Valorant gets the full sledgehammer applied
with no warning.

### 2. Hard `affinity_mask` accepts arbitrary u64 — no per-machine validation — Severity MEDIUM

`crates/tray/src/main.rs:5472-5484` (the profile-editor mask field) accepts
any hex `u64`. There is no check that the mask fits the machine's CPU count
or that at least one bit is set. The picker-modal blocks empty masks
(`main.rs:1121`) — but the **profile editor does not.** A power user editing
the `affinity_mask` field on a profile can save `Mask(0x0)`, push to the
service, and find every matching process pinned to no cores. The engine
warns and falls back (`crates/engine/src/lib.rs:398`), but that's a runtime
log line, not a UI-side guard.

Additionally, `Mask(0xFFFFFFFF_FFFFFFFF)` on a 16-thread machine just sets
bits the kernel ignores — harmless but misleading.

### 3. `Profile.persistent` is not exposed in the editor — Severity MEDIUM

`crates/core/src/profile.rs:196` carries `persistent: bool`. The default
`game-x3d` ships with `persistent: true` (`policy.rs:352`) — meaning the
X3D pin survives alt-tab. The render path
(`crates/tray/src/main.rs:4917-4965, 5057-5128`) shows description, CPU
sets, affinity mask, power throttling, priority class, I/O priority, memory
priority, trim_working_set, game_mode. **`persistent` is neither displayed
in the read view nor editable.** A user authoring a new profile cannot opt
into stickiness without hand-editing `policy.json`. Hot-reload picks it up,
but it's an obscure flag with material behavior.

### 4. `tick_ms`, `background_profile`, and ProBalance thresholds are read-only — Severity MEDIUM

ProBalance card (`crates/tray/src/main.rs:1581-1661`) shows the system /
hog thresholds and dwell as **labels only** — the only UI control is a
single Enable/Disable toggle. To change `system_cpu_threshold_percent`,
`hog_cpu_threshold_percent`, `min_restrain_ms`, or `ignore_processes`
(`crates/core/src/policy.rs:142-160`), the user must hand-edit policy.json.
Same for `tick_ms` (`policy.rs:111`) and `background_profile` (`policy.rs:106`).

`ignore_processes` is the user's escape hatch when ProBalance demotes
something they want untouched — and it has no UI.

### 5. No "Reset to defaults" — Severity MEDIUM

Grepped: no "Reset defaults", "Restore defaults", `reset_defaults` anywhere.
If a user butchers their `policy.json` (saves a syntactically valid but
semantically broken policy — e.g. deletes the `eco` profile that
`background_profile` references), the service keeps loading the broken
policy. The recovery is: stop the service, delete `policy.json`, restart —
then `load_or_create_default` (`policy.rs:252-264`) re-seeds. This is fine
for someone who reads source; for a normal user it's "uninstall and
reinstall."

### 6. No Export / Import — Severity MEDIUM

No UI surface for "save my rules to a file" or "load this team's rules."
The CLI also has no `policy export` / `policy import` verbs
(`crates/cli/src/main.rs:32-66` — verbs are install/uninstall/start/stop/
status/pause/resume/apply/topology/game-mode). The user can find
`policy.json` (path printed nowhere in the UI as far as I can see) and copy
it manually, but this isn't a discoverable flow for migrating between
machines or sharing tuning configs.

### 7. CLI has no policy mutation verbs at all — Severity MEDIUM

`crates/cli/src/main.rs:32-66`: the CLI can install, start, stop, status,
pause, resume, `apply <profile>` once, dump topology, and inspect Game Mode
status / safe-list. **There is no way to add a rule, edit a profile, or
toggle ProBalance from the CLI.** GPO push, automated machine setup, and
scripted rollouts all require `policy.json` file munging — which works
because of hot-reload, but the CLI is the documented surface and it's
incomplete.

### 8. Safe-list is curated and not editable — Severity HIGH

`crates/gamemode/src/safe_lists/services.json` +
`crates/gamemode/src/safe_lists/processes.json` are baked into the
`framesage-gamemode` crate. The user **cannot** add their own
"I want my custom telemetry agent suspended" entries — the planner rejects
unknown ids (`crates/tray/src/main.rs:5200`). The CLI exposes them
read-only via `game-mode safe-list` (`cli/src/main.rs:171-192`), and the
profile editor lets you put any string in the text area, but the apply path
will silently drop unknowns. So the user sees their entry persisted in
`policy.json` but never executed — confusing silent failure.

Every shop has different OEM background apps. Without an "add to safe-list"
mechanism, the curated list will always be stale.

### 9. No validation when referencing missing profile ids or CCDs — Severity MEDIUM

`AppRule.profile` is a `ProfileId` (free string). If a user writes a rule
referencing `game-x3d-custom` but only has `game-x3d`, nothing in the save
flow flags it. The engine falls back to `default_profile` silently
(`policy.rs:181-188`). Should be a Save-time lint: "Rule for bf6.exe
references unknown profile `xyz`."

Same for `CpuSelector::Ccd(7)` on a single-CCD chip, or `Kind(Cache)` on an
Intel hybrid without P-vs-E vs cache split. The picker
(`crates/tray/src/main.rs:5466-5468`) allows DragValue 0–15 with no
correlation to detected topology (which IS known via `framesage topology`).

### 10. Game Mode entries have no tooltips on what they actually do — Severity LOW

The Game Mode editor (`crates/tray/src/main.rs:5165-5219`) renders
`stop_services` and `suspend_processes` as plain multi-line text areas with
a hint about "safe-list gate at apply time." It does NOT cross-reference
the rich rationale strings from `services.json` /`processes.json`.
A user who types `OneDrive.exe` doesn't see "Cloud-storage sync. Spikes
disk I/O and CPU on file change bursts" — that rationale is only visible
via `framesage game-mode safe-list` CLI command. The data exists; the UI
just doesn't surface it.

### 11. No first-run onboarding — Severity LOW

No wizard, no welcome screen, no "we ship with three games pre-armed — do
you want to disable them?" prompt. New install + first launch drops the
user into the Status tab with the default policy already loaded and Rules
tab already containing BF6/Valorant/Fortnite mapped to the aggressive
profile. If they happen to launch one of those games before exploring the
UI, the full Game Mode sledgehammer fires.

### 12. No dry-run / preview — Severity LOW

There's no "show me what Game Mode will do before it fires" view. The
service + journal is crash-safe-revertible
(`crates/core/src/game_mode.rs:24`), and "Game Mode Off" via CLI exists
for panic, but the *understanding* gap stays — a user picking
"Suspend OneDrive" doesn't see "this will pause cloud sync; saves to
shared docs won't replicate until the game closes." That's a tooltip
opportunity (see #10) but also a Plan-Preview-Apply opportunity.

## Per-app profile assignment

Works (`crates/tray/src/main.rs:3209-3232`): right-click → "Create rule for
this exe" → pick profile. The CreateRule action
(`crates/tray/src/main.rs:3844-...`) appends to the policy draft. Lasso
parity here is fine. The hover text could be punchier ("Auto-apply this
profile every time this exe launches") but the flow is correct.

## Summary table

| # | Gap                                       | Severity | File:line                         |
| - | ----------------------------------------- | -------- | --------------------------------- |
| 1 | Aggressive default game-x3d, pre-armed    | HIGH     | policy.rs:343-470                 |
| 2 | Profile-editor mask accepts 0x0 / oversize| MEDIUM   | tray/main.rs:5472-5484            |
| 3 | `persistent` field not in editor          | MEDIUM   | tray/main.rs:5057-5128            |
| 4 | tick_ms / ProBalance thresholds / bg only-read | MEDIUM | tray/main.rs:1581-1661         |
| 5 | No "Reset to defaults"                    | MEDIUM   | (absent)                          |
| 6 | No export / import                        | MEDIUM   | (absent)                          |
| 7 | CLI cannot mutate policy                  | MEDIUM   | cli/main.rs:32-66                 |
| 8 | Safe-list baked in, not user-extensible   | HIGH     | gamemode/safe_lists/*.json        |
| 9 | No validation of profile-ids / CCDs       | MEDIUM   | tray/main.rs:2097-2109, 5466-5468 |
| 10| No tooltips on Game Mode list entries     | LOW      | tray/main.rs:5196-5208            |
| 11| No first-run onboarding                   | LOW      | (absent)                          |
| 12| No Game Mode dry-run / preview            | LOW      | (absent)                          |

The pattern: power-user knobs exist on the wire but not in the UI (#3, #4),
and beginner safety rails are partial (#1 aggressive defaults, #11 no
onboarding) — but the *destructive* edges are mostly covered (Terminate
modal, empty-mask block in picker, denylists, hot-reload atomicity, Realtime
omission). The biggest two action items are: gate the default game rules
behind a first-run opt-in (#1, #11), and let the user extend the safe-list
or at least make the rationale visible in-app (#8, #10).
