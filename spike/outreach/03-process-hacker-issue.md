# Draft: Process Hacker / System Informer issue

**Target:** https://github.com/winsiderss/systeminformer/issues
**Label:** `question` (or whatever the repo uses for non-bug discussion)

---

## Title

Current EDR detection landscape for ETW kernel-event consumers — looking for evidence-level data

## Body

Hi System Informer maintainers,

I'm writing as the maintainer of [FrameSage](https://github.com/franzjeger/framesage-win), an open-source Windows tool that's in the same category as System Informer — native UI, native event consumption, deep enough into kernel ETW that EDR products notice us. We use `StartTraceW` with a private session GUID and consume CSWITCH/DPC/ISR/DISK_IO/MEMORY_HARD_FAULTS via `OpenTraceW` in real-time mode.

System Informer / Process Hacker has historically been on the front line of "EDR products misclassify legitimate kernel-event consumers." Your release notes and issues over the years have referenced specific cases (signing certificate revocations, Defender heuristics, etc.). We're trying to validate our consumer against Defender ATP / CrowdStrike Falcon / SentinelOne Singularity before flipping a closed-loop measurement feature default-on in v0.7.1, and would love your current take on:

1. **Which of those three products currently flag a clean SystemTraceProvider consumer** with default policy? You have far more current data on this than we do.
2. **Has signing been sufficient remediation in your experience**, or have any of the three required architectural changes / vendor coordination?
3. **Is there a "you'll definitely get flagged for this" pattern** in System Informer's history we should be checking our spike binary against? Things like specific provider GUIDs that draw heat, specific event flag combinations, specific function call sequences that are heuristic-triggers.

If anyone on the maintainer side currently runs a multi-EDR test rig and would be willing to run our spike binary through it (the time ask is ~30 min: a 60-second run, a 5-minute run, and one run with a real game in the foreground), we'd be deeply grateful — and we'd return the favor in any way that's useful to System Informer. Validation criteria + the env-1 gap that motivated this outreach are at https://github.com/franzjeger/framesage-win/blob/main/spike/etw-edr-report.md.

Happy to send:
- The unsigned spike binary, or
- Compile instructions if you'd rather not run an unsigned third-party binary (entirely fair), or
- Just the technical scope (schema-research doc, architecture doc) so you can tell us "we already know what happens, here's the answer."

Hard cutoff on our side: day 5 from this issue's posting, after which un-validated products escalate to paid in-house testing.

Thanks for everything System Informer has shipped — and for keeping the lights on for the kind of low-level Windows tooling that's increasingly hard to ship under EDR-default-deny.

—Frank
FrameSage maintainer
https://github.com/franzjeger/framesage-win
