# Draft: Sysinternals outreach

> **INTERNAL SEND-PREP — NOT FOR THE RECIPIENT.**
> Everything in this section is guidance for the sender. Only the content under
> `## RECIPIENT-FACING BODY` below the divider goes to Sysinternals.

**Target:** Sysinternals team (Mark Russinovich and colleagues at Microsoft).

**Reach options, in order of viability:**
1. **Direct email** to a Sysinternals team member if any contact exists from prior correspondence (FrameSage history: none currently).
2. **Sysinternals forum** at https://learn.microsoft.com/en-us/answers/tags/255/sysinternals (lower expected response rate; the forum is more user-support than maintainer-engagement).
3. **Twitter/X** at [@markrussinovich](https://twitter.com/markrussinovich) for a public-question variant of the below — only as a last resort and only after the forum has had 48h to surface a reply.

This contact is opportunistic. There's no guaranteed channel; treat as "if any direct contact exists, send it; otherwise skip and rely on PerfView + Process Hacker channels."

**Personalization before send:**
- Default salutation below is "Hi Sysinternals team," which is the safe generic. If you have a specific contact in mind (a previous correspondent, a named maintainer you've talked to before), personalize the salutation to that person's first name before sending.
- If you go the public Twitter/X route, the Body section below needs trimming for character count.

---

## RECIPIENT-FACING BODY

### Subject

EDR detection of native ETW kernel consumers — current data?

### Body

Hi Sysinternals team,

I'm working on an open-source Windows tool (FrameSage) that consumes ETW kernel events via SystemTraceProvider — same API surface as Process Explorer's kernel-event tooling, Process Monitor, and the bits of Sysinternals Suite that read ETW for live process telemetry. Architecture and code are at https://github.com/franzjeger/framesage-win for context.

We're at the v0.7 release point where we need to validate the consumer behaves cleanly under modern EDR products (Defender ATP, CrowdStrike Falcon, SentinelOne Singularity). Sysinternals tools face this problem at much larger scale than we ever will, so I wanted to ask — entirely off the record if that's easier — whether the team has current data on:

1. Which EDR products in 2025-2026 flag SystemTraceProvider consumers as suspicious, and under what default policies?
2. Whether the standard remediation pattern is Authenticode signing + vendor-allow-list submission, or whether some products require deeper engagement.
3. If you've ever published guidance for third-party tool developers in this space that I've missed (PerfView's docs have some, Sysinternals' I haven't found explicit guidance on).

Even a "yes Defender ATP flags us routinely / no it doesn't" answer would unblock a v0.7.1 ship decision on our side. Our validation criteria are written down at https://github.com/franzjeger/framesage-win/blob/main/spike/etw-edr-report.md if you want to see exactly what evidence we'd be using.

Day-5 hard cutoff on our side — if I don't have data by then, I escalate to paid in-house validation. So a fast "can't help right now" is also useful — it lets us reallocate.

Thanks for your time, and for everything Sysinternals has shipped over the years.

—Frank
FrameSage maintainer
https://github.com/franzjeger/framesage-win
