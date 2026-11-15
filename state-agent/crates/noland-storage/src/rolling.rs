use std::future::{poll_fn, Future};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::Poll;

use noland_state_core::{Result, StateError};

type TransferFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Completion receipts release objects while retaining one process slot until
/// the group's child has exited. The owner drops the entire reservation on error.
#[derive(Clone)]
pub struct UploadReservation(Arc<Mutex<(usize, u64)>>);
impl UploadReservation {
    pub fn completed(&self, count: usize, bytes: u64) {
        let mut remaining = self.0.lock().unwrap_or_else(|e| e.into_inner());
        remaining.0 = remaining.0.saturating_sub(count).max(1);
        remaining.1 = remaining.1.saturating_sub(bytes);
    }
    fn remaining(&self) -> (usize, u64) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A small, scoped set of transfers. Slots are refilled on *any* completion,
/// rather than waiting for a whole wave. No detached tasks survive cancellation:
/// dropping the window drops every child future before its borrowed owner.
pub struct RollingUploadWindow<'a, T> {
    tasks: Vec<(UploadReservation, TransferFuture<'a, T>)>,
    max_count: usize,
    max_bytes: u64,
}

impl<'a, T> RollingUploadWindow<'a, T> {
    pub fn new(max_count: usize, max_bytes: u64) -> Self {
        Self {
            tasks: Vec::new(),
            max_count: max_count.clamp(1, 32),
            max_bytes: max_bytes.max(1),
        }
    }

    pub fn len(&self) -> usize {
        self.tasks
            .iter()
            .map(|(reservation, _)| reservation.remaining().0)
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    pub fn bytes(&self) -> u64 {
        self.tasks
            .iter()
            .map(|(reservation, _)| reservation.remaining().1)
            .sum()
    }

    pub fn can_push(&self, bytes: u64) -> bool {
        self.can_push_count(1, bytes)
    }

    pub fn can_push_count(&self, count: usize, bytes: u64) -> bool {
        count > 0
            && self
                .len()
                .checked_add(count)
                .is_some_and(|count| count <= self.max_count)
            && self
                .bytes()
                .checked_add(bytes)
                .is_some_and(|sum| sum <= self.max_bytes)
    }

    pub fn push(
        &mut self,
        bytes: u64,
        transfer: impl Future<Output = Result<T>> + Send + 'a,
    ) -> Result<()> {
        self.push_count(1, bytes, transfer)
    }

    /// A grouped backend request still reserves one slot per object, not just
    /// one per process, so batching cannot multiply concurrency or memory.
    pub fn push_count(
        &mut self,
        count: usize,
        bytes: u64,
        transfer: impl Future<Output = Result<T>> + Send + 'a,
    ) -> Result<()> {
        if !self.can_push_count(count, bytes) {
            return Err(StateError::Invalid(
                "upload window count/byte budget exceeded".into(),
            ));
        }
        self.tasks.push((
            UploadReservation(Arc::new(Mutex::new((count, bytes)))),
            Box::pin(transfer),
        ));
        Ok(())
    }

    pub fn push_tracked<F: Future<Output = Result<T>> + Send + 'a>(
        &mut self,
        count: usize,
        bytes: u64,
        make: impl FnOnce(UploadReservation) -> F,
    ) -> Result<()> {
        if !self.can_push_count(count, bytes) {
            return Err(StateError::Invalid(
                "upload window count/byte budget exceeded".into(),
            ));
        }
        let reservation = UploadReservation(Arc::new(Mutex::new((count, bytes))));
        let future = make(reservation.clone());
        self.tasks.push((reservation, Box::pin(future)));
        Ok(())
    }

    pub async fn next(&mut self) -> Option<Result<T>> {
        poll_fn(|cx| {
            if self.tasks.is_empty() {
                return Poll::Ready(None);
            }
            for index in 0..self.tasks.len() {
                if let Poll::Ready(result) = self.tasks[index].1.as_mut().poll(cx) {
                    let (_, future) = self.tasks.swap_remove(index);
                    drop(future);
                    return Poll::Ready(Some(result));
                }
            }
            Poll::Pending
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use std::time::Duration;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn a_completed_group_member_releases_capacity_before_the_straggler_exits() {
        let mut window = RollingUploadWindow::new(3, 30);
        let (sent, received) = oneshot::channel();
        let (done, finish) = oneshot::channel();
        window
            .push_tracked(3, 30, |reservation| async move {
                reservation.completed(1, 10);
                sent.send(()).unwrap();
                finish.await.unwrap();
                Ok(3)
            })
            .unwrap();
        tokio::select! { _ = received => {}, _ = window.next() => panic!("group completed too soon") }
        assert_eq!((window.len(), window.bytes()), (2, 20));
        window.push(10, async { Ok(1) }).unwrap();
        assert_eq!(window.next().await.unwrap().unwrap(), 1);
        done.send(()).unwrap();
        assert_eq!(window.next().await.unwrap().unwrap(), 3);
        assert!(window.is_empty());
    }

    #[tokio::test]
    async fn refills_while_the_first_pack_is_still_slow_and_enforces_both_bounds() {
        let mut window = RollingUploadWindow::new(2, 10);
        let (release, slow) = oneshot::channel();
        window
            .push(6, async {
                slow.await.unwrap();
                Ok(1)
            })
            .unwrap();
        assert!(!window.can_push(5));
        window.push(4, async { Ok(2) }).unwrap();
        assert!(!window.can_push(0)); // Count is bounded even for empty objects.
        assert_eq!(window.next().await.unwrap().unwrap(), 2);
        assert_eq!(window.bytes(), 6);
        window.push(4, async { Ok(3) }).unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), window.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            3
        );
        assert_eq!(window.len(), 1);
        release.send(()).unwrap();
        assert_eq!(window.next().await.unwrap().unwrap(), 1);
        assert!(window.next().await.is_none());
        assert_eq!(window.bytes(), 0);
        assert!(window.push(11, async { Ok(4) }).is_err());
    }

    #[tokio::test]
    async fn grouping_reserves_each_object_and_releases_the_entire_batch() {
        let mut window = RollingUploadWindow::new(3, 100);
        window.push_count(2, 60, async { Ok(2) }).unwrap();
        assert_eq!(window.len(), 2);
        assert!(!window.can_push_count(2, 1));
        assert!(!window.can_push_count(1, 41));
        assert!(window.can_push_count(1, 40));
        assert!(!window.can_push_count(0, 0));
        assert_eq!(window.next().await.unwrap().unwrap(), 2);
        assert_eq!((window.len(), window.bytes()), (0, 0));
    }

    #[tokio::test]
    async fn cancellation_drops_every_started_transfer_before_returning() {
        struct Active(Arc<AtomicUsize>);
        impl Drop for Active {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let active = Arc::new(AtomicUsize::new(0));
        let mut window = RollingUploadWindow::<()>::new(3, 12);
        for _ in 0..3 {
            let active = active.clone();
            window
                .push(4, async move {
                    active.fetch_add(1, Ordering::SeqCst);
                    let _guard = Active(active);
                    std::future::pending::<Result<()>>().await
                })
                .unwrap();
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(10), window.next())
                .await
                .is_err()
        );
        assert_eq!(active.load(Ordering::SeqCst), 3);
        drop(window);
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }
}
