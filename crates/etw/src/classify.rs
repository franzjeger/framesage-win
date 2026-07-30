//! Kernel-event classification — Group A drain, first slice.
//!
//! Maps (provider GUID, opcode) from an `EVENT_RECORD` header to the
//! five signal families the §2.3 `kernel_signal` schema tracks. Every
//! entry is backed by `spike/etw-schemas.md` (MSDN authority +
//! empirical confirmation on Win11 26200); this table is the
//! `win11_24h2_26200` opcode table referenced by
//! `session_start.opcode_table`.
//!
//! Platform-free on purpose: the GUID is our own POD struct so the
//! classification (and its tests) run on any host; the `cfg(windows)`
//! callback converts `EVENT_RECORD.EventHeader.ProviderId` into it.

/// POD mirror of a Windows GUID, host-independent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderGuid {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

/// Thread kernel provider `{3D6FA8D1-FE05-11D0-9DDA-00C04FD7BA7C}`
/// (MSDN Thread_V2; spike §"Provider GUID summary").
pub const THREAD_PROVIDER: ProviderGuid = ProviderGuid {
    data1: 0x3D6F_A8D1,
    data2: 0xFE05,
    data3: 0x11D0,
    data4: [0x9D, 0xDA, 0x00, 0xC0, 0x4F, 0xD7, 0xBA, 0x7C],
};

/// PerfInfo provider `{CE1DBFB4-137E-4DA6-87B0-3F59AA102CBC}`
/// (MSDN PerfInfo).
pub const PERFINFO_PROVIDER: ProviderGuid = ProviderGuid {
    data1: 0xCE1D_BFB4,
    data2: 0x137E,
    data3: 0x4DA6,
    data4: [0x87, 0xB0, 0x3F, 0x59, 0xAA, 0x10, 0x2C, 0xBC],
};

/// PageFault provider `{3D6FA8D3-FE05-11D0-9DDA-00C04FD7BA7C}`
/// (MSDN PageFault_V2).
pub const PAGEFAULT_PROVIDER: ProviderGuid = ProviderGuid {
    data1: 0x3D6F_A8D3,
    data2: 0xFE05,
    data3: 0x11D0,
    data4: [0x9D, 0xDA, 0x00, 0xC0, 0x4F, 0xD7, 0xBA, 0x7C],
};

/// DiskIo provider `{3D6FA8D4-FE05-11D0-9DDA-00C04FD7BA7C}`
/// (MSDN DiskIo_TypeGroup1).
pub const DISKIO_PROVIDER: ProviderGuid = ProviderGuid {
    data1: 0x3D6F_A8D4,
    data2: 0xFE05,
    data3: 0x11D0,
    data4: [0x9D, 0xDA, 0x00, 0xC0, 0x4F, 0xD7, 0xBA, 0x7C],
};

/// The five §2.3 `kernel_signal` families. `as usize` indexes the
/// per-kind counters in `ConsumerState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum KernelEventKind {
    /// PerfInfo 0x42 ThreadDPC / 0x44 DPC / 0x45 TimerDPC.
    Dpc = 0,
    /// PerfInfo 0x43 ISR.
    Isr = 1,
    /// PageFault 0x20 HardFault.
    HardFault = 2,
    /// Thread 0x24 CSwitch.
    ContextSwitch = 3,
    /// DiskIo 0x0A Read / 0x0B Write.
    DiskIo = 4,
}

/// Number of kinds — sizes the counter arrays.
pub const KERNEL_EVENT_KINDS: usize = 5;

impl KernelEventKind {
    /// §2.3 `kernel_signal.signal` discriminant for this family.
    pub fn signal_name(self) -> &'static str {
        match self {
            KernelEventKind::Dpc => "dpc_spike",
            KernelEventKind::Isr => "isr_spike",
            KernelEventKind::HardFault => "hard_fault_burst",
            KernelEventKind::ContextSwitch => "context_switch_storm",
            KernelEventKind::DiskIo => "diskio_spike",
        }
    }

    pub fn all() -> [KernelEventKind; KERNEL_EVENT_KINDS] {
        [
            KernelEventKind::Dpc,
            KernelEventKind::Isr,
            KernelEventKind::HardFault,
            KernelEventKind::ContextSwitch,
            KernelEventKind::DiskIo,
        ]
    }
}

/// The `win11_24h2_26200` classification table. Returns `None` for
/// events outside the five tracked families (the callback just counts
/// those in `events_seen` as before).
pub fn classify(provider: &ProviderGuid, opcode: u8) -> Option<KernelEventKind> {
    match *provider {
        PERFINFO_PROVIDER => match opcode {
            // 0x42 ThreadDPC / 0x44 DPC / 0x45 TimerDPC (MSDN DPC class).
            0x42 | 0x44 | 0x45 => Some(KernelEventKind::Dpc),
            // 0x43 ISR (MSDN ISR class).
            0x43 => Some(KernelEventKind::Isr),
            // Opcode 0x50 observed on 26200 is NOT in MSDN's table —
            // deliberately unclassified per the spike's cite-or-defer
            // rule.
            _ => None,
        },
        THREAD_PROVIDER => (opcode == 0x24).then_some(KernelEventKind::ContextSwitch),
        PAGEFAULT_PROVIDER => (opcode == 0x20).then_some(KernelEventKind::HardFault),
        DISKIO_PROVIDER => matches!(opcode, 0x0A | 0x0B).then_some(KernelEventKind::DiskIo),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_matches_the_spike_schema_doc() {
        assert_eq!(
            classify(&PERFINFO_PROVIDER, 0x44),
            Some(KernelEventKind::Dpc)
        );
        assert_eq!(
            classify(&PERFINFO_PROVIDER, 0x42),
            Some(KernelEventKind::Dpc)
        );
        assert_eq!(
            classify(&PERFINFO_PROVIDER, 0x45),
            Some(KernelEventKind::Dpc)
        );
        assert_eq!(
            classify(&PERFINFO_PROVIDER, 0x43),
            Some(KernelEventKind::Isr)
        );
        assert_eq!(
            classify(&THREAD_PROVIDER, 0x24),
            Some(KernelEventKind::ContextSwitch)
        );
        assert_eq!(
            classify(&PAGEFAULT_PROVIDER, 0x20),
            Some(KernelEventKind::HardFault)
        );
        assert_eq!(
            classify(&DISKIO_PROVIDER, 0x0A),
            Some(KernelEventKind::DiskIo)
        );
        assert_eq!(
            classify(&DISKIO_PROVIDER, 0x0B),
            Some(KernelEventKind::DiskIo)
        );
    }

    #[test]
    fn undocumented_and_foreign_events_stay_unclassified() {
        // PerfInfo 0x50: observed on 26200 but absent from MSDN's
        // table — cite-or-defer says leave it out.
        assert_eq!(classify(&PERFINFO_PROVIDER, 0x50), None);
        // DiskIo init/flush opcodes are not the Read/Write completions
        // §2.3 tracks.
        assert_eq!(classify(&DISKIO_PROVIDER, 0x0C), None);
        let unknown = ProviderGuid {
            data1: 1,
            data2: 2,
            data3: 3,
            data4: [0; 8],
        };
        assert_eq!(classify(&unknown, 0x24), None);
    }

    #[test]
    fn signal_names_match_the_section_2_3_discriminants() {
        let names: Vec<&str> = KernelEventKind::all()
            .iter()
            .map(|k| k.signal_name())
            .collect();
        assert_eq!(
            names,
            [
                "dpc_spike",
                "isr_spike",
                "hard_fault_burst",
                "context_switch_storm",
                "diskio_spike"
            ]
        );
    }
}
