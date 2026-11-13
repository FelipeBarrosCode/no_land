//! One CPU/memory/IO budget shared by overlapping capture and compression.
use super::*;

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct Budget {
    pub hashing: usize,
    pub packing: usize,
}

impl Budget {
    pub fn detect(agent: &StateAgent, mode: BackupPerformanceMode) -> Result<Self> {
        let gameplay = agent.has_latency_sensitive_sessions()?;
        let cpus = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(2);
        // TransferTuning already reads host/container memory, including provider
        // buffers. Use a separate conservative per-preparation-worker estimate.
        let memory = noland_rclone_adapter::TransferTuning::preparation_memory_bytes();
        let pressured = std::fs::read_to_string("/proc/pressure/io")
            .ok()
            .is_some_and(|text| {
                text.lines()
                    .find(|line| line.starts_with("some "))
                    .is_some_and(|line| {
                        line.split_whitespace()
                            .find_map(|part| part.strip_prefix("avg10="))
                            .and_then(|value| value.parse::<f64>().ok())
                            .is_some_and(|value| value >= 10.0)
                    })
            });
        let mut budget = Self::for_resources(cpus, memory, mode, gameplay, pressured);
        // Overrides cannot defeat memory, disk-pressure or gameplay admission.
        for (name, value) in [
            ("NOLAND_HASH_WORKERS", &mut budget.hashing),
            ("NOLAND_PACK_WORKERS", &mut budget.packing),
        ] {
            if let Ok(requested) = std::env::var(name).unwrap_or_default().parse::<usize>() {
                *value = requested.clamp(1, *value);
            }
        }
        Ok(budget)
    }

    fn for_resources(
        cpus: usize,
        memory: Option<u64>,
        mode: BackupPerformanceMode,
        gameplay: bool,
        io_pressure: bool,
    ) -> Self {
        // Half of available memory is reserved for transfer processes and the
        // coordinator. Reader/codec workspaces never multiply upload budgets.
        let memory_slots =
            (memory.unwrap_or(512 * 1024 * 1024) / 2 / (96 * 1024 * 1024)).max(2) as usize;
        let cpu_slots = match mode {
            BackupPerformanceMode::Balanced => cpus.div_ceil(2),
            BackupPerformanceMode::Automatic
            | BackupPerformanceMode::Fast
            | BackupPerformanceMode::Full => cpus,
        };
        let total = cpu_slots.min(memory_slots).clamp(2, 12);
        let total = if gameplay {
            2
        } else if io_pressure {
            total.min(4)
        } else {
            total
        };
        Self {
            hashing: total.div_ceil(2).min(4),
            packing: (total - total.div_ceil(2).min(4)).max(1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overlapping_workers_share_resources_and_respect_gameplay_and_disk_pressure() {
        let large = Budget::for_resources(
            48,
            Some(16 << 30),
            BackupPerformanceMode::Fast,
            false,
            false,
        );
        assert_eq!((large.hashing, large.packing), (4, 8));
        let low = Budget::for_resources(
            48,
            Some(384 << 20),
            BackupPerformanceMode::Full,
            false,
            false,
        );
        assert_eq!(low.hashing + low.packing, 2);
        for (game, io, limit) in [(true, false, 2), (false, true, 4)] {
            let budget =
                Budget::for_resources(48, Some(16 << 30), BackupPerformanceMode::Full, game, io);
            assert_eq!(budget.hashing + budget.packing, limit);
        }
    }
}
