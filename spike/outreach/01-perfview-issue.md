# Draft: PerfView issue / discussion

**Target:** https://github.com/microsoft/perfview/issues
**Label:** `discussion` (or `question` — pick whatever the repo's issue templates use)
**CC if no response in 48h:** Vance Morrison via the maintainer list

---

## Title

EDR detection of SystemTraceProvider consumers — current data on Defender ATP / CrowdStrike / SentinelOne?

## Body

Hi PerfView maintainers,

I'm working on an open-source Windows tool ([FrameSage](https://github.com/franzjeger/framesage-win)) that consumes ETW kernel events via the same `SystemTraceProvider` API PerfView uses — `StartTraceW` with a private session GUID, `EVENT_TRACE_FLAG_CSWITCH | DPC | INTERRUPT | DISK_IO | MEMORY_HARD_FAULTS`, `OpenTraceW` in `PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD`, the works. The spike binary and the architecture write-up are linked at the bottom.

We're at the point in the v0.7 release cycle where we need to validate the consumer behaves cleanly under modern EDR products, specifically:

1. Microsoft Defender for Endpoint (the EDR product, not consumer Defender)
2. CrowdStrike Falcon (any tier)
3. SentinelOne Singularity

PerfView has been exercising this API since long before any of these products existed, and you've collectively had every EDR conversation under the sun. So I'd love your current take on:

1. **Which of the three (if any) currently flag a plain SystemTraceProvider consumer** with default policy? We have anecdata that CrowdStrike's behavioral engine sometimes flags ETW consumers, but no current evidence-level data. PerfView's release history would suggest you've seen reports of this when users in EDR-managed shops try to use PerfView.
2. **For products that do flag**, has the standard remediation been Authenticode signing + vendor-allow-list submission, or is there a behavioral pattern (e.g., specific provider GUIDs, event flags) that gets flagged regardless of signature?
3. **Is there a hunting-query pattern** any of these products use to detect "non-PerfView non-xperf SystemTraceProvider consumers" that we should be aware of? (Our validation criteria require evidence-level results — screenshots/logs/exports — not "seemed fine.")

If anyone on the maintainer side runs a multi-EDR home lab and would be willing to run our spike binary (60 s + 5 min `--duration` runs, plus one run with a real game in the foreground) and capture EDR console output, we'd be deeply grateful. The validation criteria we're held to (and the env-1 gap that motivates this outreach) are documented in [spike/etw-edr-report.md §6.1](https://github.com/franzjeger/framesage-win/blob/main/spike/etw-edr-report.md) so you can see exactly what we'd need.

Happy to provide:
- The unsigned spike binary directly, or
- Compile instructions for anyone reasonably uncomfortable running an unsigned binary, or
- The full schema-research doc (`spike/etw-schemas.md`) so technical reviewers can validate "this is just a normal ETW consumer" before lending a test box.

Hard cutoff on my side: if I don't have evidence-level data by **day 5**, I escalate to paid in-house testing for the un-validated product. So fast "no, can't help" responses are also genuinely valuable — they let me reallocate budget.

Thanks for reading.

—Frank
FrameSage maintainer
https://github.com/franzjeger/framesage-win

### Reference links

- Repo: https://github.com/franzjeger/framesage-win
- Phase 1 ETW spike report: https://github.com/franzjeger/framesage-win/blob/main/spike/etw-report.md
- ETW kernel-event schema research (citations per opcode): https://github.com/franzjeger/framesage-win/blob/main/spike/etw-schemas.md
- EDR validation scope + criteria: https://github.com/franzjeger/framesage-win/blob/main/spike/etw-edr-report.md
- v0.7 architecture: https://github.com/franzjeger/framesage-win/blob/main/audit/v0.7-architecture.md
