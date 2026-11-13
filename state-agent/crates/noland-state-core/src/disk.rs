//! Per-filesystem admission for storage work; reservations never delete user data.
//!
//! Aggregate plans use [`check_admission`], which does not hold capacity. Active
//! writers use [`DiskWriteBudget`] and consume their reservation only after bytes
//! have been allocated on disk. Do not also hold an aggregate reservation for the
//! same writes: an independent writer budget would charge that capacity twice.
use crate::{Result, StateError};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

type ReservationRegistry = Arc<Mutex<BTreeMap<u64, u64>>>;
type DeviceRequests<'a> = BTreeMap<u64, (u64, u64, &'a Path)>;

static RESERVED: OnceLock<ReservationRegistry> = OnceLock::new();

fn registry() -> ReservationRegistry {
    Arc::clone(RESERVED.get_or_init(Default::default))
}

pub fn safety_margin() -> u64 {
    std::env::var("NOLAND_STORAGE_MIN_FREE_BYTES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(64 * 1024 * 1024)
}

#[cfg(unix)]
pub fn available(path: &Path) -> Result<(u64, u64)> {
    use std::{
        ffi::CString,
        os::unix::{ffi::OsStrExt, fs::MetadataExt},
    };
    let ancestor = path.ancestors().find(|path| path.exists()).ok_or_else(|| {
        StateError::Storage(format!("no existing filesystem for {}", path.display()))
    })?;
    let name = CString::new(ancestor.as_os_str().as_bytes())
        .map_err(|_| StateError::UnsafePath("NUL in storage path".into()))?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: name is NUL-terminated and stat points to writable statvfs storage.
    if unsafe { libc::statvfs(name.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let stat = unsafe { stat.assume_init() };
    // statvfs field widths differ across the supported Unix targets.
    #[allow(clippy::unnecessary_cast)]
    let free = (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64);
    Ok((std::fs::metadata(ancestor)?.dev(), free))
}

#[cfg(not(unix))]
pub fn available(_path: &Path) -> Result<(u64, u64)> {
    Err(StateError::Storage(
        "storage disk admission requires a Unix agent".into(),
    ))
}

pub fn ensure_space(path: &Path, bytes: u64) -> Result<()> {
    let (_, free) = available(path)?;
    require(free, bytes, safety_margin(), path)
}

/// Check the aggregate peak-space estimate without holding a reservation for it.
/// Existing active reservations are included, and paths on the same filesystem
/// are summed. Successful preflight is not a promise of future capacity: each
/// writer must still reserve its bounded, not-yet-allocated output before writing.
pub fn check_admission(requests: &[(PathBuf, u64)]) -> Result<()> {
    check_admission_using(&registry(), requests, safety_margin(), available)
}

fn require(free: u64, required: u64, margin: u64, path: &Path) -> Result<()> {
    if required
        .checked_add(margin)
        .is_none_or(|total| free < total)
    {
        return Err(StateError::Storage(format!(
            "insufficient storage at {}: {free} bytes available, {required} required plus {margin} safety margin", path.display()
        )));
    }
    Ok(())
}

fn device_requests<'a>(
    requests: &'a [(PathBuf, u64)],
    mut capacity: impl FnMut(&Path) -> Result<(u64, u64)>,
) -> Result<DeviceRequests<'a>> {
    let mut devices = BTreeMap::<u64, (u64, u64, &Path)>::new();
    for (path, bytes) in requests {
        let (device, free) = capacity(path)?;
        let entry = devices.entry(device).or_insert((free, 0, path));
        entry.0 = entry.0.min(free);
        entry.1 = entry
            .1
            .checked_add(*bytes)
            .ok_or_else(|| StateError::Storage("storage reservation overflow".into()))?;
    }
    Ok(devices)
}

fn check_devices(
    reserved: &BTreeMap<u64, u64>,
    devices: &DeviceRequests<'_>,
    margin: u64,
) -> Result<()> {
    for (device, (free, bytes, path)) in devices {
        let required = bytes
            .checked_add(*reserved.get(device).unwrap_or(&0))
            .ok_or_else(|| StateError::Storage("storage reservation overflow".into()))?;
        require(*free, required, margin, path)?;
    }
    Ok(())
}

fn check_admission_using(
    registry: &ReservationRegistry,
    requests: &[(PathBuf, u64)],
    margin: u64,
    capacity: impl FnMut(&Path) -> Result<(u64, u64)>,
) -> Result<()> {
    let reserved = registry
        .lock()
        .map_err(|_| StateError::Storage("storage reservation lock poisoned".into()))?;
    // Sample free space under the accounting lock. Otherwise a concurrent writer
    // could allocate and consume its reservation between the sample and check,
    // making the older free-space sample overstate unreserved capacity.
    check_devices(&reserved, &device_requests(requests, capacity)?, margin)
}

/// Fixed held capacity, released on drop. For output that is gradually allocated,
/// use [`DiskWriteBudget`] instead so allocated bytes stop counting as reserved.
pub struct DiskReservation {
    registry: ReservationRegistry,
    owned: Vec<(u64, u64)>,
}

impl DiskReservation {
    pub fn reserve(requests: &[(PathBuf, u64)]) -> Result<Self> {
        Self::reserve_using(registry(), requests, safety_margin(), available)
    }

    fn reserve_using(
        registry: ReservationRegistry,
        requests: &[(PathBuf, u64)],
        margin: u64,
        capacity: impl FnMut(&Path) -> Result<(u64, u64)>,
    ) -> Result<Self> {
        let mut reserved = registry
            .lock()
            .map_err(|_| StateError::Storage("storage reservation lock poisoned".into()))?;
        let devices = device_requests(requests, capacity)?;
        check_devices(&reserved, &devices, margin)?;
        let mut owned = Vec::new();
        for (device, (_, bytes, _)) in devices {
            *reserved.entry(device).or_default() += bytes;
            owned.push((device, bytes));
        }
        drop(reserved);
        Ok(Self { registry, owned })
    }
}

fn release(reserved: &mut BTreeMap<u64, u64>, device: u64, bytes: u64) {
    if let Some(total) = reserved.get_mut(&device) {
        *total = total.saturating_sub(bytes);
        if *total == 0 {
            reserved.remove(&device);
        }
    }
}

impl Drop for DiskReservation {
    fn drop(&mut self) {
        if let Ok(mut reserved) = self.registry.lock() {
            for (device, bytes) in &self.owned {
                release(&mut reserved, *device, *bytes);
            }
        }
    }
}

/// A bounded writer's unallocated capacity on one filesystem. Independent
/// writers share admission accounting, but own and consume separate budgets.
pub struct DiskWriteBudget {
    reservation: DiskReservation,
}

impl DiskWriteBudget {
    /// Reserve output capacity before writing. Avoid enclosing held reservations
    /// for the same output; use [`check_admission`] for aggregate plans instead.
    pub fn reserve(path: &Path, bytes: u64) -> Result<Self> {
        Ok(Self {
            reservation: DiskReservation::reserve(&[(path.to_path_buf(), bytes)])?,
        })
    }

    pub fn remaining(&self) -> u64 {
        self.reservation.owned[0].1
    }

    /// Stop reserving bytes now accounted for by the filesystem's free-space
    /// measurement. Call only after successful allocation/flush, not when bytes
    /// merely enter a userspace buffer. Unflushed bytes must remain reserved.
    ///
    /// Consumption is irreversible: deleting output does not replenish this
    /// budget. A later writer must obtain a fresh reservation. Drop releases only
    /// the unused remainder, including on cancellation or write failure.
    pub fn consume_allocated(&mut self, bytes: u64) -> Result<()> {
        let (device, remaining) = &mut self.reservation.owned[0];
        if bytes > *remaining {
            return Err(StateError::Storage(
                "allocated bytes exceed writer disk budget".into(),
            ));
        }
        let mut reserved = self
            .reservation
            .registry
            .lock()
            .map_err(|_| StateError::Storage("storage reservation lock poisoned".into()))?;
        release(&mut reserved, *device, bytes);
        *remaining -= bytes;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writer(registry: &ReservationRegistry, free: u64, bytes: u64) -> Result<DiskWriteBudget> {
        Ok(DiskWriteBudget {
            reservation: DiskReservation::reserve_using(
                Arc::clone(registry),
                &[(PathBuf::from("capture"), bytes)],
                20,
                |_| Ok((1, free)),
            )?,
        })
    }

    #[test]
    fn admission_includes_margin_and_overflow_fails_closed() {
        assert!(require(100, 80, 20, Path::new("fixture")).is_ok());
        assert!(require(99, 80, 20, Path::new("fixture")).is_err());
        assert!(require(100, u64::MAX, 20, Path::new("fixture")).is_err());
        assert!(require(u64::MAX, u64::MAX, 1, Path::new("fixture")).is_err());
    }

    #[test]
    fn aggregate_preflight_does_not_double_charge_active_writers() {
        let registry = ReservationRegistry::default();
        let requests = [(PathBuf::from("capture"), 40), (PathBuf::from("packs"), 40)];
        check_admission_using(&registry, &requests, 20, |_| Ok((1, 100))).unwrap();
        assert!(registry.lock().unwrap().is_empty());
        let capture = writer(&registry, 100, 40).unwrap();
        let packs = writer(&registry, 100, 40).unwrap();
        assert!(writer(&registry, 100, 1).is_err());
        // Preflight must still account for capacity held by other active writers.
        assert!(check_admission_using(&registry, &requests, 20, |_| Ok((1, 100))).is_err());
        drop(capture);
        drop(packs);
        assert!(registry.lock().unwrap().is_empty());
    }

    #[test]
    fn concurrent_writers_cannot_admit_the_same_capacity() {
        let registry = ReservationRegistry::default();
        let start = std::sync::Barrier::new(3);
        let outcomes = std::thread::scope(|scope| {
            let writers: Vec<_> = (0..2)
                .map(|_| {
                    scope.spawn(|| {
                        start.wait();
                        writer(&registry, 100, 80)
                    })
                })
                .collect();
            start.wait();
            // Keep the successful reservation alive until both have attempted
            // admission; either contender may win, but they cannot both win.
            writers
                .into_iter()
                .map(|writer| writer.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(registry.lock().unwrap().get(&1), Some(&80));
        drop(outcomes);
        assert!(registry.lock().unwrap().is_empty());
        assert!(writer(&registry, 100, 80).is_ok());
    }

    #[test]
    fn allocated_bytes_are_not_charged_again_and_drop_releases_only_unused_capacity() {
        let registry = ReservationRegistry::default();
        let mut capture = writer(&registry, 100, 60).unwrap();
        // Forty bytes have reached the filesystem; the free-space sample is now
        // 60. Until the writer acknowledges its allocation, admission is conservative.
        assert!(writer(&registry, 60, 20).is_err());
        capture.consume_allocated(40).unwrap();
        assert_eq!(capture.remaining(), 20);
        let packs = writer(&registry, 60, 20).unwrap();
        assert!(writer(&registry, 60, 1).is_err());
        drop(capture);
        assert_eq!(registry.lock().unwrap().get(&1), Some(&20));
        drop(packs);
        assert!(registry.lock().unwrap().is_empty());
        // Previously allocated output still occupies disk after its budget dies.
        assert!(writer(&registry, 60, 41).is_err());
        assert!(writer(&registry, 60, 40).is_ok());
    }

    #[test]
    fn excess_consumption_cannot_release_another_writers_capacity() {
        let registry = ReservationRegistry::default();
        let mut capture = writer(&registry, 100, 30).unwrap();
        let packs = writer(&registry, 100, 50).unwrap();
        assert!(capture.consume_allocated(31).is_err());
        assert_eq!(capture.remaining(), 30);
        assert_eq!(registry.lock().unwrap().get(&1), Some(&80));
        capture.consume_allocated(30).unwrap();
        capture.consume_allocated(0).unwrap();
        drop(capture);
        assert_eq!(registry.lock().unwrap().get(&1), Some(&50));
        drop(packs);
        assert!(registry.lock().unwrap().is_empty());
    }

    #[test]
    fn filesystem_admission_is_aggregated_and_atomic() {
        let registry = ReservationRegistry::default();
        let requests = [
            (PathBuf::from("capture"), 40),
            (PathBuf::from("packs"), 30),
            (PathBuf::from("snapshots"), 35),
        ];
        let reserve = |second_free| {
            DiskReservation::reserve_using(Arc::clone(&registry), &requests, 20, |path| {
                Ok(if path == Path::new("snapshots") {
                    (2, second_free)
                } else {
                    (1, 100)
                })
            })
        };
        assert!(reserve(54).is_err());
        assert!(
            registry.lock().unwrap().is_empty(),
            "failed admission must be atomic"
        );
        let budget = reserve(55).unwrap();
        assert_eq!(
            *registry.lock().unwrap(),
            BTreeMap::from([(1, 70), (2, 35)])
        );
        drop(budget);
        assert!(registry.lock().unwrap().is_empty());
    }

    #[test]
    fn capacity_is_sampled_under_accounting_lock_and_overflows_do_not_mutate_it() {
        let registry = ReservationRegistry::default();
        let budget = DiskReservation::reserve_using(
            Arc::clone(&registry),
            &[(PathBuf::from("capture"), u64::MAX)],
            0,
            |_| {
                assert!(registry.try_lock().is_err());
                Ok((1, u64::MAX))
            },
        )
        .unwrap();
        assert!(DiskReservation::reserve_using(
            Arc::clone(&registry),
            &[(PathBuf::from("packs"), 1)],
            0,
            |_| Ok((1, u64::MAX)),
        )
        .is_err());
        assert_eq!(registry.lock().unwrap().get(&1), Some(&u64::MAX));
        drop(budget);
        assert!(DiskReservation::reserve_using(
            Arc::clone(&registry),
            &[
                (PathBuf::from("capture"), u64::MAX),
                (PathBuf::from("packs"), 1)
            ],
            0,
            |_| Ok((1, u64::MAX)),
        )
        .is_err());
        assert!(registry.lock().unwrap().is_empty());
    }
}
