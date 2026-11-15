//! Hint-only pack admission. The graph contains only files spanning multiple
//! packs; single-pack files cannot leave cross-pack hash/handle pressure.
//!
//! Admission updates sparse incidences once, not every file for every candidate.
//! A file contributes to at most two queued packs: its next manifest-order pack
//! and its last undispatched pack. Running packs are never waited on. Actual
//! assembly hints strengthen these credits for still-inline partials.
use std::cmp::Reverse;
use std::collections::BTreeSet;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc, Arc,
};
use std::time::Instant;

use super::{elapsed_us, PackJob, PipelineIoMetrics, RestorePlan, RestorePriority};

// Included in the pipeline's aggregate memory admission, even if the graph is
// smaller or falls back. The build's temporary hash lookup is charged too.
pub(super) const WORKING_BYTES: u64 = 16 * 1024 * 1024;
const PREFLIGHT_BYTES: u64 = 2 * 1024 * 1024;
pub(super) const GRAPH_BYTES: u64 = WORKING_BYTES - PREFLIGHT_BYTES;
const MAILBOX_BYTES: u64 = 64 * 1024;
const HINT_CAPACITY: usize = 512;
const FAIR_INTERVAL: u64 = 8;
const STARTED: u64 = 1;
const READBACK: u64 = 2;
const COMPLETED: u64 = 4;
const DIRTY: u64 = 1 << 63;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    Baseline,
    FileAware,
}

/// Preflight membership is also bounded, including when all 186k files need
/// work. A bitmap avoids retaining another usize/file snapshot beside the
/// graph. At an unrepresentably large manifest we retain the original static
/// scheduler's needed-hash-only filtering instead, never a partial graph.
pub(super) struct Eligibility {
    words: Option<Vec<u64>>,
    build_us: u64,
}

impl Eligibility {
    pub fn new(files: usize, pending: impl Iterator<Item = usize>) -> Self {
        let started = Instant::now();
        let words = files.div_ceil(64);
        if words as u64 > PREFLIGHT_BYTES / 8 {
            return Self {
                words: None,
                build_us: elapsed_us(started),
            };
        }
        let mut words = vec![0u64; words];
        for index in pending {
            words[index / 64] |= 1 << (index % 64);
        }
        Self {
            words: Some(words),
            build_us: elapsed_us(started),
        }
    }

    pub fn contains(&self, index: usize) -> bool {
        self.words.as_ref().is_none_or(|words| {
            words
                .get(index / 64)
                .is_some_and(|word| word & (1 << (index % 64)) != 0)
        })
    }

    pub fn precise(&self) -> bool {
        self.words.is_some()
    }

    pub fn bytes(&self) -> u64 {
        self.words
            .as_ref()
            .map_or(0, |words| words.len() as u64 * 8)
    }

    pub fn iter(&self) -> impl Iterator<Item = usize> + Clone + '_ {
        self.words
            .as_deref()
            .unwrap_or_default()
            .iter()
            .copied()
            .enumerate()
            .flat_map(|(word_index, mut word)| {
                std::iter::from_fn(move || {
                    if word == 0 {
                        return None;
                    }
                    let bit = word.trailing_zeros() as usize;
                    word &= word - 1;
                    Some(word_index * 64 + bit)
                })
            })
    }
}

#[derive(Default)]
pub(super) struct Stats {
    mode: AtomicU64,
    build_us: AtomicU64,
    decision_ns: AtomicU64,
    feedback_ns: AtomicU64,
    metadata_bytes: AtomicU64,
    build_peak_bytes: AtomicU64,
    files: AtomicU64,
    edges: AtomicU64,
    decisions: AtomicU64,
    reordered: AtomicU64,
    fairness: AtomicU64,
    hints: AtomicU64,
    coalesced: AtomicU64,
    dropped: AtomicU64,
    incidences: AtomicU64,
    score_updates: AtomicU64,
    preflight_bytes: AtomicU64,
}

impl Stats {
    pub fn preflight(&self, eligibility: &Eligibility) {
        self.preflight_bytes
            .store(eligibility.bytes(), Ordering::Relaxed);
        self.build_us
            .fetch_add(eligibility.build_us, Ordering::Relaxed);
    }

    pub fn snapshot(&self, io: &mut PipelineIoMetrics) {
        io.pack_scheduling = match self.mode.load(Ordering::Relaxed) {
            1 => "file_aware",
            2 => "baseline_requested",
            3 => "baseline_metadata_budget",
            4 => "baseline_no_cross_pack_files",
            _ => "initializing",
        }
        .into();
        io.scheduling_build_us = self.build_us.load(Ordering::Relaxed);
        io.scheduling_decision_us = self.decision_ns.load(Ordering::Relaxed) / 1000;
        io.scheduling_feedback_us = self.feedback_ns.load(Ordering::Relaxed) / 1000;
        io.scheduling_metadata_bytes = self.metadata_bytes.load(Ordering::Relaxed);
        io.scheduling_build_peak_bytes = self.build_peak_bytes.load(Ordering::Relaxed);
        io.scheduling_files = self.files.load(Ordering::Relaxed);
        io.scheduling_edges = self.edges.load(Ordering::Relaxed);
        io.scheduling_decisions = self.decisions.load(Ordering::Relaxed);
        io.scheduling_reordered_decisions = self.reordered.load(Ordering::Relaxed);
        io.scheduling_fairness_decisions = self.fairness.load(Ordering::Relaxed);
        io.scheduling_hints = self.hints.load(Ordering::Relaxed);
        io.scheduling_coalesced_hints = self.coalesced.load(Ordering::Relaxed);
        io.scheduling_dropped_hints = self.dropped.load(Ordering::Relaxed);
        io.scheduling_incidence_visits = self.incidences.load(Ordering::Relaxed);
        io.scheduling_score_updates = self.score_updates.load(Ordering::Relaxed);
        io.scheduling_preflight_bytes = self.preflight_bytes.load(Ordering::Relaxed);
    }
}

/// One sticky atomic state per tracked file, plus a bounded queue of dirty slot
/// IDs. No plaintext, paths, handles, hashers, per-chunk messages or blocking
/// sends. Overflow loses an optimization hint, never reconstruction state.
pub(super) struct Feedback {
    indices: Vec<usize>,
    states: Vec<AtomicU64>,
    dirty: mpsc::SyncSender<u32>,
    stats: Arc<Stats>,
}

impl Feedback {
    pub fn observe(&self, manifest_index: usize, readback: bool, completed: bool) {
        let started = Instant::now();
        if let Ok(slot) = self.indices.binary_search(&manifest_index) {
            let flags = STARTED
                | if readback { READBACK } else { 0 }
                | if completed { COMPLETED } else { 0 };
            let previous = self.states[slot].fetch_or(flags, Ordering::AcqRel);
            if previous & flags == flags {
                self.stats.coalesced.fetch_add(1, Ordering::Relaxed);
            } else {
                let previous = self.states[slot].fetch_or(DIRTY, Ordering::AcqRel);
                if previous & DIRTY != 0 {
                    self.stats.coalesced.fetch_add(1, Ordering::Relaxed);
                } else if self.dirty.try_send(slot as u32).is_err() {
                    self.states[slot].fetch_and(!DIRTY, Ordering::AcqRel);
                    self.stats.dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        self.stats.feedback_ns.fetch_add(
            started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Score {
    inline_closes: u64,
    inline_next: u64,
    closes: u64,
    next: u64,
}

type Key = (RestorePriority, Reverse<Score>, usize);

struct Queue {
    jobs: Vec<Option<PackJob>>,
    baseline: usize,
    ranked: BTreeSet<Key>,
    keys: Vec<Key>,
    scores: Vec<Score>,
    changed: Vec<bool>,
    dirty: Vec<usize>,
}

impl Queue {
    fn new(jobs: Vec<PackJob>, dynamic: bool) -> Self {
        let count = jobs.len();
        let keys = if dynamic {
            jobs.iter()
                .enumerate()
                .map(|(index, job)| (job.priority, Reverse(Score::default()), index))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let scored = if dynamic { count } else { 0 };
        Self {
            ranked: keys.iter().copied().collect(),
            keys,
            scores: vec![Score::default(); scored],
            changed: vec![false; scored],
            dirty: Vec::with_capacity(scored),
            baseline: 0,
            jobs: jobs.into_iter().map(Some).collect(),
        }
    }

    fn change(&mut self, pack: u32, inline: bool, closing: bool, add: bool) {
        let pack = pack as usize;
        if self.jobs[pack].is_none() {
            return;
        }
        let score = &mut self.scores[pack];
        let counter = match (inline, closing) {
            (true, true) => &mut score.inline_closes,
            (true, false) => &mut score.inline_next,
            (false, true) => &mut score.closes,
            (false, false) => &mut score.next,
        };
        if add {
            *counter += 1;
        } else {
            *counter -= 1;
        }
        if !self.changed[pack] {
            self.changed[pack] = true;
            self.dirty.push(pack);
        }
    }

    fn refresh(&mut self, stats: &Stats) {
        // Coalesce a large shared-chunk fanout into one tree update per pack.
        for pack in self.dirty.drain(..) {
            self.changed[pack] = false;
            self.ranked.remove(&self.keys[pack]);
            self.keys[pack].1 = Reverse(self.scores[pack]);
            if self.jobs[pack].is_some() {
                self.ranked.insert(self.keys[pack]);
            }
            stats.score_updates.fetch_add(1, Ordering::Relaxed);
        }
    }
}

struct File {
    begin: u32,
    end: u32,
    frontier: u32,
    remaining: u32,
    remaining_xor: u32,
    started: bool,
    inline: bool,
    completed: bool,
}

impl File {
    fn credits(&self, packs: &[u32], queue: &mut Queue, add: bool) {
        if !self.started || self.completed {
            return;
        }
        if self.frontier < self.end {
            let next = packs[self.frontier as usize];
            queue.change(next, false, false, add);
            if self.inline {
                queue.change(next, true, false, add);
            }
        }
        if self.remaining == 1 {
            queue.change(self.remaining_xor, false, true, add);
            if self.inline {
                queue.change(self.remaining_xor, true, true, add);
            }
        }
    }
}

struct Graph {
    files: Vec<File>,
    packs: Vec<u32>,
    // CSR reverse incidence, avoiding an allocation per file or pack.
    offsets: Vec<usize>,
    pack_files: Vec<u32>,
    feedback: Arc<Feedback>,
    hints: mpsc::Receiver<u32>,
}

impl Graph {
    fn build(
        jobs: &[PackJob],
        plan: &RestorePlan,
        eligible: impl Iterator<Item = usize> + Clone,
        budget: u64,
        stats: &Arc<Stats>,
    ) -> Option<Self> {
        // Conservative charges include Vec construction, tree nodes, mailbox
        // storage and the temporary borrowed-hash lookup. No allocation below
        // grows without first passing this bound.
        let base = MAILBOX_BYTES.checked_add((jobs.len() as u64).checked_mul(1024)?)?;
        let hashes = jobs
            .iter()
            .try_fold(0u64, |sum, job| sum.checked_add(job.entries.len() as u64))?;
        let lookup_bytes = hashes.checked_mul(32)?;
        let initial_peak = base.checked_add(lookup_bytes)?;
        if jobs.len() > u32::MAX as usize || initial_peak > budget {
            return None;
        }
        stats
            .build_peak_bytes
            .store(initial_peak, Ordering::Relaxed);
        let mut lookup = Vec::with_capacity(hashes as usize);
        for (pack, job) in jobs.iter().enumerate() {
            for entry in &job.entries {
                lookup.push((entry.chunk_hash.as_str(), pack as u32));
            }
        }
        lookup.sort_unstable_by_key(|(hash, _)| *hash);
        let pack_for = |hash: &str| {
            lookup
                .binary_search_by_key(&hash, |(hash, _)| *hash)
                .ok()
                .map(|index| lookup[index].1)
        };
        let mut seen = vec![usize::MAX; jobs.len()];
        let mut file_count = 0usize;
        let mut edge_count = 0usize;
        for index in eligible.clone() {
            let mut degree = 0usize;
            for chunk in &plan.manifest.files[index].chunks {
                let pack = pack_for(&chunk.hash)? as usize;
                if seen[pack] != index {
                    seen[pack] = index;
                    degree += 1;
                }
            }
            if degree > 1 {
                file_count = file_count.checked_add(1)?;
                edge_count = edge_count.checked_add(degree)?;
            }
        }
        if file_count == 0 {
            stats.mode.store(4, Ordering::Relaxed);
            return None;
        }
        // Node, manifest ID, atomic state and construction/alignment slack.
        // Both CSR directions together occupy 8 bytes per sparse incidence.
        let file_bytes = (std::mem::size_of::<File>()
            + std::mem::size_of::<usize>()
            + std::mem::size_of::<AtomicU64>()
            + 8) as u64;
        let metadata = base
            .checked_add((file_count as u64).checked_mul(file_bytes)?)?
            .checked_add((edge_count as u64).checked_mul(8)?)?;
        let peak = metadata.checked_add(lookup_bytes)?;
        if file_count > u32::MAX as usize || edge_count > u32::MAX as usize || peak > budget {
            return None;
        }
        stats.build_peak_bytes.store(peak, Ordering::Relaxed);
        let mut files = Vec::with_capacity(file_count);
        let mut indices = Vec::with_capacity(file_count);
        let mut packs = Vec::with_capacity(edge_count);
        let mut offsets = vec![0usize; jobs.len() + 1];
        seen.fill(usize::MAX);
        // A bounded per-file scratch vector is unnecessary: the first pass's
        // marks count degree, then this pass appends once in first-use order.
        for index in eligible {
            let begin = packs.len();
            let mut xor = 0;
            let mut first = None;
            for chunk in &plan.manifest.files[index].chunks {
                let pack = pack_for(&chunk.hash)?;
                if seen[pack as usize] != index {
                    seen[pack as usize] = index;
                    xor ^= pack;
                    if let Some(first) = first {
                        if packs.len() == begin {
                            packs.push(first);
                        }
                        packs.push(pack);
                    } else {
                        first = Some(pack);
                    }
                }
            }
            if packs.len() == begin {
                continue;
            }
            for &pack in &packs[begin..] {
                offsets[pack as usize + 1] += 1;
            }
            files.push(File {
                begin: begin as u32,
                end: packs.len() as u32,
                frontier: begin as u32,
                remaining: (packs.len() - begin) as u32,
                remaining_xor: xor,
                started: false,
                inline: false,
                completed: false,
            });
            indices.push(index);
        }
        for pack in 0..jobs.len() {
            offsets[pack + 1] += offsets[pack];
        }
        let mut positions = offsets[..jobs.len()].to_vec();
        let mut pack_files = vec![0u32; edge_count];
        for (index, file) in files.iter().enumerate() {
            for &pack in &packs[file.begin as usize..file.end as usize] {
                pack_files[positions[pack as usize]] = index as u32;
                positions[pack as usize] += 1;
            }
        }
        let (dirty, hints) = mpsc::sync_channel(HINT_CAPACITY);
        let feedback = Arc::new(Feedback {
            states: (0..files.len()).map(|_| AtomicU64::new(0)).collect(),
            indices,
            dirty,
            stats: Arc::clone(stats),
        });
        stats.files.store(files.len() as u64, Ordering::Relaxed);
        stats.edges.store(packs.len() as u64, Ordering::Relaxed);
        stats.metadata_bytes.store(metadata, Ordering::Relaxed);
        Some(Self {
            files,
            packs,
            offsets,
            pack_files,
            feedback,
            hints,
        })
    }

    fn drain_hints(&mut self, queue: &mut Queue, stats: &Stats) {
        // Producers cannot keep this loop running indefinitely.
        for _ in 0..HINT_CAPACITY {
            let Ok(index) = self.hints.try_recv() else {
                break;
            };
            let flags = self.feedback.states[index as usize].fetch_and(!DIRTY, Ordering::AcqRel);
            let file = &mut self.files[index as usize];
            file.credits(&self.packs, queue, false);
            file.started |= flags & STARTED != 0;
            file.inline = flags & (STARTED | READBACK | COMPLETED) == STARTED;
            file.completed |= flags & COMPLETED != 0;
            file.credits(&self.packs, queue, true);
            stats.hints.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn admitted(&mut self, pack: usize, queue: &mut Queue, stats: &Stats) {
        for &index in &self.pack_files[self.offsets[pack]..self.offsets[pack + 1]] {
            stats.incidences.fetch_add(1, Ordering::Relaxed);
            let file = &mut self.files[index as usize];
            file.credits(&self.packs, queue, false);
            file.started = true;
            file.remaining -= 1;
            file.remaining_xor ^= pack as u32;
            // This cursor only moves forward over this file's sparse edges:
            // total traversal across the entire restore is O(edges).
            while file.frontier < file.end
                && queue.jobs[self.packs[file.frontier as usize] as usize].is_none()
            {
                file.frontier += 1;
            }
            file.credits(&self.packs, queue, true);
        }
    }
}

pub(super) struct Scheduler {
    queue: Queue,
    graph: Option<Graph>,
    stats: Arc<Stats>,
    decisions: u64,
}

impl Scheduler {
    pub fn new(
        jobs: Vec<PackJob>,
        plan: &RestorePlan,
        eligible: impl Iterator<Item = usize> + Clone,
        mode: Mode,
        budget: u64,
        stats: Arc<Stats>,
    ) -> Self {
        let started = Instant::now();
        let graph = if mode == Mode::FileAware {
            stats.mode.store(3, Ordering::Relaxed);
            Graph::build(&jobs, plan, eligible, budget.min(GRAPH_BYTES), &stats)
        } else {
            stats.mode.store(2, Ordering::Relaxed);
            None
        };
        if graph.is_some() {
            stats.mode.store(1, Ordering::Relaxed);
        }
        let effective_mode = if graph.is_some() {
            Mode::FileAware
        } else {
            Mode::Baseline
        };
        let queue = Queue::new(jobs, effective_mode == Mode::FileAware);
        stats
            .build_us
            .fetch_add(elapsed_us(started), Ordering::Relaxed);
        Self {
            queue,
            graph,
            stats,
            decisions: 0,
        }
    }

    pub fn feedback(&self) -> Option<Arc<Feedback>> {
        self.graph.as_ref().map(|graph| Arc::clone(&graph.feedback))
    }

    pub fn next(&mut self) -> Option<PackJob> {
        let started = Instant::now();
        while self.queue.baseline < self.queue.jobs.len()
            && self.queue.jobs[self.queue.baseline].is_none()
        {
            self.queue.baseline += 1;
        }
        let baseline = self.queue.baseline;
        self.queue.jobs.get(baseline)?.as_ref()?;
        let selected = if let Some(graph) = &mut self.graph {
            graph.drain_hints(&mut self.queue, &self.stats);
            self.queue.refresh(&self.stats);
            // Highest remaining priority is always the leading key. A fair
            // baseline dispatch every eighth admission bounds bypass within a
            // tier; lower tiers never overtake queued higher-priority work.
            if self.decisions % FAIR_INTERVAL == FAIR_INTERVAL - 1 {
                self.stats.fairness.fetch_add(1, Ordering::Relaxed);
                baseline
            } else {
                self.queue.ranked.first().expect("pending candidate").2
            }
        } else {
            baseline
        };
        if self.graph.is_some() {
            self.queue.ranked.remove(&self.queue.keys[selected]);
        }
        let job = self.queue.jobs[selected].take().expect("pending pack");
        if let Some(graph) = &mut self.graph {
            graph.admitted(selected, &mut self.queue, &self.stats);
        }
        self.decisions += 1;
        self.stats.decisions.fetch_add(1, Ordering::Relaxed);
        if selected != baseline {
            self.stats.reordered.fetch_add(1, Ordering::Relaxed);
        }
        self.stats.decision_ns.fetch_add(
            started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
        Some(job)
    }
}
