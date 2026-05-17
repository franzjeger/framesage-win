# Community outreach — EDR validation for FrameSage v0.7.1

This directory holds the draft outreach packages used to seek
evidence-level EDR validation per
`spike/etw-edr-report.md` §6 (Option D) and §7 (community
outreach scope).

**Goal:** validate that FrameSage's ETW kernel-event consumer
(same SystemTraceProvider API as PerfView, xperf, LatencyMon,
Process Hacker / System Informer) does not trip behavioral
detection or quarantine on Defender ATP, CrowdStrike Falcon, or
SentinelOne Singularity. Specifically, satisfy the four
criteria in `spike/etw-edr-report.md` §6.1.

## Files in this directory

| File | Purpose |
|---|---|
| `01-perfview-issue.md` | Draft GitHub issue body for `microsoft/perfview` |
| `02-sysinternals.md` | Draft contact note for Sysinternals team (forum or direct, if a contact exists) |
| `03-process-hacker-issue.md` | Draft GitHub issue body for `winsiderss/systeminformer` |
| `04-rsysadmin-post.md` | Draft r/sysadmin help-request post (read subreddit rules first) |
| `evidence-template.md` | Template the respondent fills out to provide evidence-level results |

## Day-5 cutoff

Outreach starts on the day PR #68 merges. For each product
(Defender ATP, CrowdStrike Falcon, SentinelOne Singularity), if
no evidence-level response has landed by **day 5**, escalate
that product to Option B (paid in-house validation) per
`spike/etw-edr-report.md` §6.2.

## Result logging

All evidence-level responses land in
`spike/etw-edr-report.md` §10 "Results log". Anecdotal "seemed
fine" responses do NOT get logged there — they're either
escalated to a request for evidence or treated as "no
response."
