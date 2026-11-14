use super::*;
use std::future::{poll_fn, Future};
use std::task::Poll;

const PACK_HEADER_BYTES: u64 = 24;
pub(super) const CHUNK_RECORD_BYTES: u64 = 52;
pub(super) const MAX_CHUNK_PACK_BYTES: u64 =
    PACK_HEADER_BYTES + constants::FASTCDC_MAX + CHUNK_RECORD_BYTES;
const SMALL_GROUP_BYTES: u64 = 64 * 1024 * 1024;
const PARTIAL_BUILDER_MAX_AGE: Duration = Duration::from_secs(1);

/// EOF flushes nonempty trailing builders immediately. During capture, retain
/// the existing one-second maximum age measured from the builder's first chunk.
pub(super) fn partial_builder_due(
    nonempty: bool,
    capture_closed: bool,
    age: Option<Duration>,
) -> bool {
    nonempty && (capture_closed || age.is_some_and(|age| age >= PARTIAL_BUILDER_MAX_AGE))
}

/// A target is not a reservation: an indivisible maximum-size chunk can exceed
/// it. Keep that worst-case next codec input available even for small targets.
pub(super) struct Limits {
    pub slots: usize,
    pub capacity: usize,
    pub bytes: u64,
    pub pack_max: u64,
    pub target: u64,
    pub lookahead_bytes: u64,
}
impl Limits {
    pub fn new(slots: usize, bytes: u64, requested: u64) -> Self {
        let pack_max = constants::PACK_MAX.min(bytes);
        let target = requested
            .min(pack_max)
            .min((bytes / (slots as u64 + 1)).max(1));
        // Below two maximum chunks, overlap cannot be guaranteed. Do not hold
        // an impossible headroom reservation and deadlock an otherwise valid
        // small transfer. Frozen packs larger than this new target also retain
        // their identity and can drain alone.
        let lookahead_bytes = if bytes >= MAX_CHUNK_PACK_BYTES.saturating_mul(2) {
            target.max(MAX_CHUNK_PACK_BYTES)
        } else {
            0
        };
        Self {
            slots,
            capacity: slots.saturating_mul(2),
            bytes,
            pack_max,
            target,
            lookahead_bytes,
        }
    }

    pub fn can_upload<T>(
        &self,
        uploads: &noland_storage::RollingUploadWindow<'_, T>,
        count: usize,
        bytes: u64,
        prepare_more: bool,
    ) -> bool {
        uploads.can_push_count(count, bytes)
            && (!prepare_more
                || uploads.bytes().saturating_add(bytes)
                    <= self.bytes.saturating_sub(self.lookahead_bytes)
                || (uploads.is_empty() && count == 1))
    }

    pub fn group_limit(&self, provider: &dyn SharedStorageProvider) -> usize {
        if provider.supports_parallel_upload_groups() {
            (self.slots / 2).clamp(1, 8)
        } else {
            1
        }
    }
}

pub(super) struct Ready {
    pub index: usize,
    pub pack: BuiltPack,
    pub since: Instant,
}
impl Ready {
    pub fn new(index: usize, pack: BuiltPack) -> Self {
        Self {
            index,
            pack,
            since: Instant::now(),
        }
    }
}

/// Inspect only the local ready prefix. Never await a codec to fill a group.
pub(super) fn ready_group_len<T>(
    ready: &VecDeque<Ready>,
    uploads: &noland_storage::RollingUploadWindow<'_, T>,
    limits: &Limits,
    group_limit: usize,
    prepare_more: bool,
) -> usize {
    let mut count = 0;
    let mut bytes = 0u64;
    for item in ready.iter().take(group_limit) {
        let total = bytes.saturating_add(item.pack.bytes);
        if (count > 0 && total > SMALL_GROUP_BYTES)
            || !limits.can_upload(uploads, count + 1, total, prepare_more)
        {
            break;
        }
        count += 1;
        bytes = total;
        if bytes >= SMALL_GROUP_BYTES {
            break;
        }
    }
    count
}

/// Sealing/ready packs and rolling uploads share one logical reservation.
/// Transfer ownership, rather than keeping a second map charged until a whole
/// group exits: durable member receipts immediately reduce uploads.bytes().
pub(super) fn reservation_snapshot<T>(
    local: &HashMap<usize, u64>,
    uploads: &noland_storage::RollingUploadWindow<'_, T>,
) -> (usize, u64) {
    (
        local.len() + uploads.len(),
        local
            .values()
            .copied()
            .sum::<u64>()
            .saturating_add(uploads.bytes()),
    )
}

pub(super) enum UploadEvent<T> {
    Completed(Result<T>),
    CapacityReleased,
}

/// A partial group receipt need not finish the group's child. Surface its
/// reservation change to the scheduler in the same poll, without a timer or a
/// detached task, while the window retains the child's last process slot.
pub(super) async fn upload_event<T>(
    uploads: &mut noland_storage::RollingUploadWindow<'_, T>,
) -> UploadEvent<T> {
    let before = (uploads.len(), uploads.bytes());
    poll_fn(|cx| {
        let result = {
            let mut next = std::pin::pin!(uploads.next());
            next.as_mut().poll(cx)
        };
        match result {
            Poll::Ready(Some(result)) => Poll::Ready(UploadEvent::Completed(result)),
            Poll::Ready(None) => Poll::Ready(UploadEvent::Completed(Err(StateError::Integrity(
                "empty streaming upload window".into(),
            )))),
            Poll::Pending if before != (uploads.len(), uploads.bytes()) => {
                Poll::Ready(UploadEvent::CapacityReleased)
            }
            Poll::Pending => Poll::Pending,
        }
    })
    .await
}

#[derive(Default)]
pub(super) struct WaitTime {
    since: Option<Instant>,
    elapsed_us: u64,
}
impl WaitTime {
    pub fn set(&mut self, waiting: bool) {
        if waiting {
            self.since.get_or_insert_with(Instant::now);
        } else if let Some(since) = self.since.take() {
            self.elapsed_us = self
                .elapsed_us
                .saturating_add(since.elapsed().as_micros() as u64);
        }
    }

    pub fn micros(&self) -> u64 {
        self.elapsed_us.saturating_add(
            self.since
                .map(|since| since.elapsed().as_micros() as u64)
                .unwrap_or(0),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_builder_flushes_at_one_second_or_eof_but_never_when_empty() {
        assert!(!partial_builder_due(true, false, None));
        assert!(!partial_builder_due(true, false, Some(Duration::ZERO)));
        assert!(!partial_builder_due(
            true,
            false,
            Some(Duration::from_secs(1) - Duration::from_nanos(1))
        ));
        assert!(partial_builder_due(
            true,
            false,
            Some(Duration::from_secs(1))
        ));
        assert!(partial_builder_due(
            true,
            false,
            Some(Duration::from_secs(1) + Duration::from_nanos(1))
        ));
        assert!(partial_builder_due(true, true, None));
        assert!(partial_builder_due(true, true, Some(Duration::ZERO)));
        for capture_closed in [false, true] {
            assert!(!partial_builder_due(false, capture_closed, None));
            assert!(!partial_builder_due(
                false,
                capture_closed,
                Some(Duration::from_secs(60))
            ));
        }
    }
}
