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

    /// Infer which CCD carries 3D V-Cache from the CPPC rank distribution and
    /// retag its cores as `CoreKind::Cache`.
    ///
    /// Heuristic: on dual-CCD AMD parts (7950X3D, 9950X3D, …) the X3D CCD has
    /// a measurably lower top frequency than the non-X3D CCD — the cache stack
    /// costs ~500 MHz of headroom. When we see exactly two CCDs whose top
    /// ranks differ by ≥ `CACHE_CCD_RANK_MARGIN_PCT` percent, the slower CCD
    /// is the X3D one.
    ///
    /// We deliberately leave alone:
    /// * Single-CCD topologies — can't distinguish 7800X3D from 7700X here;
    ///   user can configure manually.
    /// * 3+ CCD topologies (Threadripper) — not a desktop X3D scenario, and a
    ///   pairwise heuristic would be fragile.
    /// * Cores already tagged as `Efficiency` — those came from a different
    ///   signal (Intel hybrid) and outrank this heuristic.
    pub fn retag_ccds_from_ranks(&mut self) {
        const CACHE_CCD_RANK_MARGIN_PCT: u64 = 5;

        let ccds: Vec<u8> = self.ccds().collect();
        if ccds.len() != 2 {
            return;
        }

        let r_a = self.cpus_on_ccd(ccds[0]).filter_map(|c| c.cppc_rank).max();
        let r_b = self.cpus_on_ccd(ccds[1]).filter_map(|c| c.cppc_rank).max();
        let (Some(r_a), Some(r_b)) = (r_a, r_b) else {
            return;
        };
        if r_a == r_b {
            return;
        }
        let (cache_ccd, perf_rank, cache_rank) = if r_a > r_b {
            (ccds[1], r_a, r_b)
        } else {
            (ccds[0], r_b, r_a)
        };

        // Require a meaningful gap: perf > cache * (1 + margin/100). Done in
        // u64 to dodge any chance of overflow with implausibly large MaxMhz.
        if (perf_rank as u64) * 100 < (cache_rank as u64) * (100 + CACHE_CCD_RANK_MARGIN_PCT) {
            return;
        }

        for cpu in &mut self.cpus {
            if cpu.ccd == cache_ccd && cpu.kind == CoreKind::Performance {
                cpu.kind = CoreKind::Cache;
            }
        }
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
                // Primary physical cores first (all of them), then SMT siblings,
                // each group ordered by CPPC rank descending. Critical for games:
                // 4 distinct cores beat 2 cores' SMT siblings even when ranks
                // overlap.
                ranked.sort_by(|a, b| {
                    a.is_smt_sibling
                        .cmp(&b.is_smt_sibling)
                        .then(b.cppc_rank.cmp(&a.cppc_rank))
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
///
/// JSON wire format uses adjacent tagging (`{"type": "...", "value": ...}`) so
/// newtype variants serialise cleanly. Internally tagged would force struct
/// variants, which we don't need.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum CpuSelector {
    /// All logical processors.
    #[default]
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 8-core / 16-thread dual-CCD synthetic chip: CCD 0 is the X3D / Cache
    /// side (4 cores, lower top CPPC rank); CCD 1 is the non-X3D Performance
    /// side (4 cores, higher top rank). SMT siblings interleaved per physical
    /// core. Used by tests below — chosen to look like a Ryzen 7950X3D.
    fn x3d_dual_ccd() -> CpuTopology {
        let mut cpus = Vec::new();
        for core in 0..8u32 {
            let ccd: u8 = if core < 4 { 0 } else { 1 };
            let kind = if ccd == 0 {
                CoreKind::Cache
            } else {
                CoreKind::Performance
            };
            // Cache CCD has lower top rank than Performance CCD — that's the
            // signal a real CPPC readout would give us.
            let base_rank = if ccd == 0 { 80 } else { 120 };
            for smt in 0..2u32 {
                cpus.push(LogicalCpu {
                    index: core * 2 + smt,
                    physical_core: core,
                    ccd,
                    kind,
                    cppc_rank: Some(base_rank - (core % 4)),
                    is_smt_sibling: smt == 1,
                });
            }
        }
        CpuTopology { cpus }
    }

    #[test]
    fn resolve_all_returns_every_thread() {
        let topo = x3d_dual_ccd();
        let resolved = topo.resolve(&CpuSelector::All);
        assert_eq!(resolved.len(), 16);
        assert_eq!(resolved.first().copied(), Some(0));
        assert_eq!(resolved.last().copied(), Some(15));
    }

    #[test]
    fn resolve_cache_picks_only_x3d_ccd() {
        let topo = x3d_dual_ccd();
        let resolved = topo.resolve(&CpuSelector::Kind(CoreKind::Cache));
        // 4 cores * 2 SMT = 8 threads on CCD 0.
        assert_eq!(resolved, vec![0, 1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn resolve_ccd_zero_and_ccd_not_zero_partition_the_machine() {
        let topo = x3d_dual_ccd();
        let on = topo.resolve(&CpuSelector::Ccd(0));
        let off = topo.resolve(&CpuSelector::CcdNot(0));
        assert_eq!(on.len() + off.len(), 16);
        assert!(on.iter().all(|i| !off.contains(i)));
    }

    #[test]
    fn resolve_top_ranked_prefers_physical_cores_over_smt_siblings() {
        let topo = x3d_dual_ccd();
        // Top 4 ranked threads: the Performance CCD has higher rank than the
        // Cache CCD; within each physical core, the non-SMT-sibling wins ties.
        let top4 = topo.resolve(&CpuSelector::TopRanked(4));
        assert_eq!(top4.len(), 4);
        // All 4 should be on the Performance CCD (cores 4..=7 → indices 8..=15).
        for idx in &top4 {
            let cpu = topo.cpus.iter().find(|c| c.index == *idx).unwrap();
            assert_eq!(cpu.kind, CoreKind::Performance);
        }
        // And none should be SMT siblings (the tie-breaker we documented).
        for idx in &top4 {
            let cpu = topo.cpus.iter().find(|c| c.index == *idx).unwrap();
            assert!(
                !cpu.is_smt_sibling,
                "TopRanked picked an SMT sibling over a physical core"
            );
        }
    }

    #[test]
    fn resolve_mask_picks_only_set_bits() {
        let topo = x3d_dual_ccd();
        // Bits 0, 2, 4, 6 → first four even logical indices.
        let mask: u128 = 0b0101_0101;
        let resolved = topo.resolve(&CpuSelector::Mask(mask));
        assert_eq!(resolved, vec![0, 2, 4, 6]);
    }

    #[test]
    fn ccds_iterator_dedups_and_orders() {
        let topo = x3d_dual_ccd();
        let ccds: Vec<u8> = topo.ccds().collect();
        assert_eq!(ccds, vec![0, 1]);
    }

    /// Two CCDs of 4 cores * 2 SMT each, both initially tagged as
    /// `Performance`. CCD `slow` gets a lower top rank; `fast` gets a higher
    /// top rank. Mirrors what `framesage-sys::topology::detect()` produces
    /// before `retag_ccds_from_ranks` runs.
    fn two_ccds(fast_max_mhz: u32, slow_max_mhz: u32) -> CpuTopology {
        let mut cpus = Vec::new();
        let mut idx = 0u32;
        for ccd in 0u8..2 {
            let top = if ccd == 0 { slow_max_mhz } else { fast_max_mhz };
            for core in 0..4u32 {
                for smt in 0..2u32 {
                    cpus.push(LogicalCpu {
                        index: idx,
                        physical_core: (ccd as u32) * 4 + core,
                        ccd,
                        kind: CoreKind::Performance,
                        cppc_rank: Some(top.saturating_sub(core * 25)),
                        is_smt_sibling: smt == 1,
                    });
                    idx += 1;
                }
            }
        }
        CpuTopology { cpus }
    }

    #[test]
    fn retag_marks_slower_ccd_as_cache_when_gap_is_large_enough() {
        // 5.7 GHz vs 5.0 GHz — the classic 7950X3D distribution.
        let mut topo = two_ccds(5700, 5000);
        topo.retag_ccds_from_ranks();

        let ccd0: Vec<CoreKind> = topo.cpus_on_ccd(0).map(|c| c.kind).collect();
        let ccd1: Vec<CoreKind> = topo.cpus_on_ccd(1).map(|c| c.kind).collect();
        assert!(
            ccd0.iter().all(|k| *k == CoreKind::Cache),
            "slower CCD 0 should be retagged as Cache"
        );
        assert!(
            ccd1.iter().all(|k| *k == CoreKind::Performance),
            "faster CCD 1 should remain Performance"
        );
    }

    #[test]
    fn retag_leaves_topology_alone_when_ccds_are_close() {
        // Within ~2% — could be the same silicon binned slightly differently.
        let mut topo = two_ccds(5700, 5600);
        topo.retag_ccds_from_ranks();
        assert!(
            topo.cpus.iter().all(|c| c.kind == CoreKind::Performance),
            "no retag when the rank gap is below the X3D threshold"
        );
    }

    #[test]
    fn retag_is_a_noop_on_single_ccd_topology() {
        // 7800X3D-like: one CCD, all Cache cores. Even with ranks present, we
        // can't distinguish X3D from non-X3D with a single CCD, so the
        // heuristic leaves the existing tags alone.
        let mut topo = CpuTopology {
            cpus: (0..8)
                .map(|i| LogicalCpu {
                    index: i,
                    physical_core: i / 2,
                    ccd: 0,
                    kind: CoreKind::Cache,
                    cppc_rank: Some(5000),
                    is_smt_sibling: i % 2 == 1,
                })
                .collect(),
        };
        topo.retag_ccds_from_ranks();
        assert!(topo.cpus.iter().all(|c| c.kind == CoreKind::Cache));
    }

    #[test]
    fn retag_is_a_noop_when_ranks_are_missing() {
        let mut topo = two_ccds(5700, 5000);
        // Strip ranks — simulates a host where CallNtPowerInformation failed.
        for cpu in &mut topo.cpus {
            cpu.cppc_rank = None;
        }
        topo.retag_ccds_from_ranks();
        assert!(topo.cpus.iter().all(|c| c.kind == CoreKind::Performance));
    }

    #[test]
    fn cpu_selector_roundtrips_through_json() {
        let cases = vec![
            CpuSelector::All,
            CpuSelector::Kind(CoreKind::Cache),
            CpuSelector::Ccd(1),
            CpuSelector::CcdNot(0),
            CpuSelector::TopRanked(4),
            CpuSelector::Mask(0xCAFE),
        ];
        for sel in cases {
            let json = serde_json::to_string(&sel).expect("serialize");
            let parsed: CpuSelector = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(sel, parsed, "round-trip mismatch via {json}");
        }
    }
}
