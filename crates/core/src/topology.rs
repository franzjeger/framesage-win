//! CPU topology: how the engine reasons about the machine it's running on.
//!
//! On dual-CCD AMD parts (5800X3D-family, 7950X3D, 9950X3D…) the difference
//! between the X3D CCD and the non-X3D CCD is the single most important fact
//! the engine needs to know. On Intel hybrid parts (12th gen+), P-cores vs
//! E-cores serve the same role. We represent both with `CoreKind` so policies
//! can target "the favored cores" without caring about the vendor.
//!
//! CPPC perf rank ("CPPC tag" on AMD, "ITD class" on Intel) gives a per-silicon
//! ordering of cores by frequency headroom. We capture it so a policy can pin a
//! latency-sensitive thread to *the* fastest physical core on this exact chip.

use serde::{Deserialize, Serialize};

/// How a logical processor relates to physical silicon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoreKind {
    /// Performance core (Intel P-core, AMD non-X3D CCD core, or single-CCD part).
    Performance,
    /// Efficiency core (Intel E-core).
    Efficiency,
    /// AMD X3D / 3D V-Cache core — the gaming-favored CCD on dual-CCD parts.
    Cache,
}

/// One logical processor as seen by Windows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogicalCpu {
    /// Windows logical processor index (0-based, matches `GROUP_AFFINITY` index
    /// within group 0; multi-group machines are not yet supported).
    pub index: u32,
    /// Physical core this thread belongs to. SMT/HT siblings share this.
    pub physical_core: u32,
    /// CCD / cluster index. For dual-CCD AMD parts the X3D CCD is usually CCD 0
    /// but we don't rely on that — see `CoreKind::Cache`.
    pub ccd: u8,
    /// What kind of core this is.
    pub kind: CoreKind,
    /// CPPC perf rank (higher = faster silicon). `None` if not exposed by the
    /// platform. Values are platform-defined, only the relative order matters.
    pub cppc_rank: Option<u32>,
    /// Is the second SMT thread on this physical core?
    pub is_smt_sibling: bool,
}

/// Snapshot of the machine's CPU topology.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CpuTopology {
    pub cpus: Vec<LogicalCpu>,
}

impl CpuTopology {
    pub fn count(&self) -> usize {
        self.cpus.len()
    }

    pub fn ccds(&self) -> impl Iterator<Item = u8> + '_ {
        let mut seen = [false; 16];
        self.cpus.iter().filter_map(move |c| {
            let idx = c.ccd as usize;
            if idx < seen.len() && !seen[idx] {
                seen[idx] = true;
                Some(c.ccd)
            } else {
                None
            }
        })
    }

    pub fn cpus_on_ccd(&self, ccd: u8) -> impl Iterator<Item = &LogicalCpu> {
        self.cpus.iter().filter(move |c| c.ccd == ccd)
    }

    pub fn cpus_of_kind(&self, kind: CoreKind) -> impl Iterator<Item = &LogicalCpu> {
        self.cpus.iter().filter(move |c| c.kind == kind)
    }

    /// Resolve a `CpuSelector` against this topology into the concrete set of
    /// logical-processor indices the policy targets.
    pub fn resolve(&self, sel: &CpuSelector) -> Vec<u32> {
        match sel {
            CpuSelector::All => self.cpus.iter().map(|c| c.index).collect(),
            CpuSelector::Kind(kind) => self.cpus_of_kind(*kind).map(|c| c.index).collect(),
            CpuSelector::Ccd(ccd) => self.cpus_on_ccd(*ccd).map(|c| c.index).collect(),
            CpuSelector::CcdNot(ccd) => self
                .cpus
                .iter()
                .filter(|c| c.ccd != *ccd)
                .map(|c| c.index)
                .collect(),
            CpuSelector::TopRanked(n) => {
                let mut ranked: Vec<&LogicalCpu> =
                    self.cpus.iter().filter(|c| c.cppc_rank.is_some()).collect();
                // Higher rank first; SMT siblings deprioritised so we prefer physical cores.
                ranked.sort_by(|a, b| {
                    b.cppc_rank
                        .cmp(&a.cppc_rank)
                        .then(a.is_smt_sibling.cmp(&b.is_smt_sibling))
                });
                ranked
                    .into_iter()
                    .take(*n as usize)
                    .map(|c| c.index)
                    .collect()
            }
            CpuSelector::Mask(mask) => (0..self.count() as u32)
                .filter(|i| mask & (1u128 << i) != 0)
                .collect(),
        }
    }
}

/// High-level "which CPUs?" expression resolved at apply time.
///
/// Stored in profiles instead of raw masks so a profile authored on one machine
/// (e.g. "use the X3D CCD") still does the right thing on another machine with
/// a different topology.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CpuSelector {
    /// All logical processors.
    All,
    /// Cores of a given kind (Performance, Efficiency, Cache).
    Kind(CoreKind),
    /// Specific CCD by index.
    Ccd(u8),
    /// Everything *except* the given CCD.
    CcdNot(u8),
    /// Top N cores by CPPC perf rank (with SMT siblings de-prioritised).
    TopRanked(u32),
    /// Explicit bitmask over logical processor indices. Last resort / legacy.
    Mask(u128),
}

impl Default for CpuSelector {
    fn default() -> Self {
        Self::All
    }
}
