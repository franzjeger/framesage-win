# Evidence template for FrameSage EDR validation

**Purpose:** structured form for community-outreach respondents
(per `spike/etw-edr-report.md` §7) to submit evidence-level
results. A completed form satisfies one or more of the v0.7.1
default-on-flip criteria in `spike/etw-edr-report.md` §6.1.

The point is to make a "yes I can help" response into a guided
fill-in-the-blanks rather than a freeform writeup. Faster for
respondents and cleaner archival on our side.

Respondents send this back filled-in via the channel the
outreach was sent through (GitHub issue comment, DM, email,
whatever).

---

## Respondent

- **Handle / contact:** _(GitHub handle, Twitter handle, "anonymous", or email)_
- **OK to credit publicly in release notes?** Yes / No / Anonymous credit only
- **Org context (optional, helps weight the evidence):** _(e.g. "I'm a SOC analyst at [company]," "I run a home lab," "I maintain $project")_

## Test environment

- **OS build:** _(`PS> [System.Environment]::OSVersion.Version` literal output)_
  ```
  ```
- **EDR product + version:**
- **EDR policy / tier:** _(default policy / hardened / custom — note any modifications from out-of-the-box default)_
- **Binary tested:** Unsigned spike (provided) / Compiled from source / Signed v0.7 release / Other
  - **Binary version / git commit SHA if known:**
- **Concurrent foreground app during the test:** Idle / Active game (specify) / Other workload (specify)

## Test runs performed

For each run, fill the row. A complete submission per `spike/etw-edr-report.md` §6.1 criterion 1 has **both** the 60-second and 5-minute idle runs; criterion 2 adds at least one run with a real game running concurrently.

| Run # | Duration | Concurrent workload | EDR alerts fired? | Process terminated? | Console telemetry recorded? |
|---|---|---|---|---|---|
| 1 | 60 s | _(idle / game-name)_ | Y / N | Y / N | Y / N |
| 2 | 5 min (300 s) | _(idle / game-name)_ | Y / N | Y / N | Y / N |
| 3 (optional) | _(if you ran additional configurations)_ |  |  |  |  |

## Evidence

For each "Y" cell in the table above, attach or link the
underlying evidence. Acceptable forms:

- **Screenshot** of the EDR admin / user console for the run
- **Exported log entry** (CSV / JSON / EDR-native format) for the alert
- **Hunting-query output** (CrowdStrike CSPL, Defender ATP advanced hunting, SentinelOne Deep Visibility) showing what telemetry the EDR collected about the FrameSage process
- **Quarantine record** if the binary was quarantined

For "N" cells, the evidence is "I ran the test and the EDR did not generate the corresponding artifact" — fine, but please confirm explicitly by stating something like "Defender ATP advanced-hunting query `DeviceProcessEvents | where FileName == 'spike-etw.exe' | top 10 by Timestamp desc` returned the process-launch + process-exit events only; no alert pivot, no suspicious-behavior flag."

```
(paste the relevant query / log / screenshot caption here, or
attach as a file in the channel response)
```

## Vendor-allow-list path (if alerts fired)

If the EDR did flag the binary, please indicate:

- [ ] Vendor allow-list / reputation submission path exists for this product (please link the docs)
- [ ] You submitted the FrameSage cert / file hash to the vendor's allow-list (optional — only if you happen to have admin on the product and want to test the remediation)
- [ ] The allow-list submission cleared the flag (optional — same caveat)

This corresponds to `spike/etw-edr-report.md` §6.1 criterion 4.

## Free-form notes

_(Anything not captured above — surprising EDR behavior, prior
context relevant to FrameSage's design, suggestions, warnings.)_

---

**Thank you.** This response goes directly into
`spike/etw-edr-report.md` §10 "Results log" — anonymized to
your preference — and contributes to the v0.7.1 release-gate
decision.
