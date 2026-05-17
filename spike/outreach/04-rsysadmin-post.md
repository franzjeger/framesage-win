# Draft: r/sysadmin post

**Target:** https://www.reddit.com/r/sysadmin/

**IMPORTANT — read the subreddit rules before posting.** r/sysadmin auto-removes posts that violate the wiki rules around vendor links, self-promotion, low-effort questions, and "shilling open-source projects." This draft is intentionally formatted as a sysadmin-to-sysadmin technical question, not a project announcement. If the post still gets auto-removed, the BloodHound Slack (#general, rules permitting) is the fallback channel for the same question.

**Tone:** matches r/sysadmin's "fellow practitioner asking a real question" register. No marketing language. No "check out our cool tool." The question is the value.

---

## Title

Currently running CrowdStrike / SentinelOne / Defender ATP — willing to test a 30-second ETW kernel consumer binary?

## Body

I'm building an open-source Windows tool that consumes ETW kernel events (same API surface as PerfView, xperf, LatencyMon, Process Hacker — kernel CSWITCH/DPC/ISR/DISK_IO/MEMORY_HARD_FAULTS via SystemTraceProvider, private session GUID, no instrumentation hooks, no driver). Before we flip a feature default-on, I need evidence-level data on whether modern EDR products treat us as suspicious.

If you run any of:
- **Microsoft Defender for Endpoint** (the EDR product — not consumer Defender)
- **CrowdStrike Falcon** (any tier)
- **SentinelOne Singularity**

…on a test box you're allowed to run third-party binaries on, I'd like to ask a ~30-minute favor. The test is:

1. Run an unsigned binary I provide with `--duration 60` (60-second ETW session).
2. Run it again with `--duration 300` (5-minute session).
3. Optionally run it one more time with a real game in the foreground (to test the EDR's handling of "ETW kernel session + concurrent process modification" — that's the v0.7.1 default-on flip gate on my side).
4. Send me whatever your EDR console / hunting queries say about the binary during/after the runs.

**I need evidence-level results** — screenshots of the EDR alert page (or its absence), exported logs, hunting-query output, whatever your product produces. "I ran it, seemed fine" is unfortunately not enough for the v0.7.1 release-gate I'm trying to clear; I need something archivable.

If you'd rather not run an unsigned binary (entirely fair), I can also send build instructions and you compile it yourself. The schema-research doc + architecture write-up are public on GitHub so you can validate it really is just an ETW consumer before lending the test box.

Hard cutoff on my side: day 5 from this post. After that, I escalate the un-tested products to paid in-house validation. So a fast "I can't help" is also super valuable — it means I reallocate budget to that product instead of waiting.

DM if you can help. Thanks.

---

### What's the project?

I'm not going to link it in the body to keep the question focused on the test (and because r/sysadmin reasonably auto-removes posts that look like project promo). Happy to share the repo + technical docs in DM.

### Reciprocal offer

If anyone helps with evidence-level testing, I'll happily reciprocate with:
- A formal acknowledgment in the release notes (or anonymous, your preference)
- Free engineering time on whatever Windows tooling problem you've got that's adjacent — I've shipped a fair amount of native Win32 / ETW / service-management code in the last six months and can swing 2-3h of "real" engineering for a real favor.
