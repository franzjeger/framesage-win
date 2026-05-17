# Draft: Sysinternals outreach

> **INTERNAL SEND-PREP — NOT FOR THE RECIPIENT.**
> Everything in this section is guidance for the sender. Only the content under
> `## RECIPIENT-FACING BODY` below the divider goes to Sysinternals.

**Target:** Sysinternals team (Mark Russinovich and colleagues at Microsoft).

**Status:** Body APPROVED by user (2026-05-17) with three tightenings applied (generic salutation kept; "currently" replaces "2025-2026"; link target redirected to `spike/etw-edr-report.md` §§1-3 for context, not §6.1).

**Send schedule (per user instruction, 2026-05-17):**
1. **Hold for 48h** — wait for `microsoft/perfview#2422` to potentially surface a personal contact route into Sysinternals. If a personal contact materializes through the PerfView thread, use that channel instead of cold-channel outreach.
2. **Tuesday 2026-05-19 EOD fallback:** if no contact route has surfaced from PerfView by then, post the body below in the Sysinternals techcommunity forum (https://techcommunity.microsoft.com/category/windows/sysinternals — verify the current URL before posting; Microsoft has been migrating from learn.microsoft.com/answers to techcommunity).
3. Log the send timestamp + channel in `spike/etw-edr-report.md` §10.1 once it goes out.

**Reach options, in order of viability:**
1. **Personal contact via PerfView issue** (preferred if it surfaces in the 48h window) — direct email/DM to a Sysinternals maintainer referred by a PerfView responder.
2. **Sysinternals techcommunity forum** — fallback if (1) doesn't materialize by Tuesday EOD.
3. **Twitter/X** at [@markrussinovich](https://twitter.com/markrussinovich) for a public-question variant of the below — last resort only, and only after the forum has had 48h to surface a reply.

This contact is opportunistic. There's no guaranteed channel; treat as "if a personal contact materializes, send it there; otherwise techcommunity by Tuesday EOD."

**Personalization before send:**
- Default salutation below is "Hi Sysinternals team," which is the safe generic. If a specific contact materializes via PerfView, personalize the salutation to that person's first name before sending.
- If the Twitter/X route ends up being the channel (last resort), the Body section below needs trimming for character count.

---

## RECIPIENT-FACING BODY

### Subject

EDR detection of native ETW kernel consumers — current data?

### Body

Hi Sysinternals team,

I'm working on an open-source Windows tool (FrameSage) that consumes ETW kernel events via SystemTraceProvider — same API surface as Process Explorer's kernel-event tooling, Process Monitor, and the bits of Sysinternals Suite that read ETW for live process telemetry. Architecture and code are at https://github.com/franzjeger/framesage-win for context.

We're at the v0.7 release point where we need to validate the consumer behaves cleanly under modern EDR products (Defender ATP, CrowdStrike Falcon, SentinelOne Singularity). Sysinternals tools face this problem at much larger scale than we ever will, so I wanted to ask — entirely off the record if that's easier — whether the team has current data on:

1. Which EDR products currently flag SystemTraceProvider consumers as suspicious, and under what default policies?
2. Whether the standard remediation pattern is Authenticode signing + vendor-allow-list submission, or whether some products require deeper engagement.
3. If you've ever published guidance for third-party tool developers in this space that I've missed (PerfView's docs have some, Sysinternals' I haven't found explicit guidance on).

Even a "yes Defender ATP flags us routinely / no it doesn't" answer would unblock a v0.7.1 ship decision on our side. The gap report at https://github.com/franzjeger/framesage-win/blob/main/spike/etw-edr-report.md §§1-3 explains the env we have, the gap we're trying to close, and the access blockers — that's the context for why I'm reaching out at all.

Day-5 hard cutoff on our side — if I don't have data by then, I escalate to paid in-house validation. So a fast "can't help right now" is also useful — it lets us reallocate.

Thanks for your time, and for everything Sysinternals has shipped over the years.

—Frank
FrameSage maintainer
https://github.com/franzjeger/framesage-win
