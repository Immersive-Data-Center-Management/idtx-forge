//! Shared queue reservations and aggregate dispatch accounting.
//!
//! Reservations travel with deferred items, including partially delivered
//! broadcasts and async HTTP work. Draining, cancellation, and drop release the
//! exact credits; switching channels or receiving a new frame does not refill a
//! still-occupied queue. No queue lock is held while running guest code.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use idtx_behavior_abi::ErrorCode;

use crate::limits::{Limits, WorkLimits, epoch_ticks};

#[derive(Debug)]
struct Usage {
    items: usize,
    bytes: usize,
    max_items: usize,
    max_bytes: usize,
    closed: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct QueueBudget {
    node: Arc<Mutex<Usage>>,
    parent: Option<Box<Self>>,
}

#[derive(Debug, Clone)]
pub(crate) struct QueueOwner(Weak<Mutex<Usage>>);

impl QueueOwner {
    pub fn is_active(&self) -> bool {
        self.0.upgrade().is_some_and(|n| n.lock().is_ok_and(|u| !u.closed))
    }

    pub fn same(&self, other: &Self) -> bool {
        self.0.ptr_eq(&other.0)
    }
}

impl QueueBudget {
    pub fn new(max_items: usize, max_bytes: usize) -> Self {
        Self {
            node: Arc::new(Mutex::new(Usage { items: 0, bytes: 0, max_items, max_bytes, closed: false })),
            parent: None,
        }
    }

    pub fn child(&self, max_items: usize, max_bytes: usize) -> Self {
        let mut child = Self::new(max_items, max_bytes);
        child.parent = Some(Box::new(self.clone()));
        child
    }

    pub fn owner(&self) -> QueueOwner {
        QueueOwner(Arc::downgrade(&self.node))
    }

    pub fn close(&self) {
        if let Ok(mut usage) = self.node.lock() {
            usage.closed = true;
        }
    }

    pub fn reserve(&self, bytes: usize) -> Result<Reservation, ErrorCode> {
        // Acquire/release ancestor locks before touching the child. On local
        // rejection the ancestor reservation is automatically returned.
        let parent = self.parent.as_ref().map(|p| p.reserve(bytes)).transpose()?;
        {
            let mut usage = self.node.lock().map_err(|_| ErrorCode::InvalidArgument)?;
            if usage.closed || usage.items >= usage.max_items
                || bytes > usage.max_bytes.saturating_sub(usage.bytes)
            {
                return Err(ErrorCode::InvalidArgument);
            }
            usage.items += 1;
            usage.bytes += bytes;
        }
        Ok(Reservation { budget: self.clone(), parent: parent.map(Box::new), bytes })
    }
}

#[derive(Debug)]
pub(crate) struct Reservation {
    budget: QueueBudget,
    parent: Option<Box<Self>>,
    bytes: usize,
}

impl Reservation {
    pub fn cancelled(&self) -> bool {
        !self.budget.owner().is_active()
            || self.parent.as_ref().is_some_and(|p| p.cancelled())
    }

    #[cfg(any(feature = "net", test))]
    pub fn shrink_to(&mut self, bytes: usize) {
        assert!(bytes <= self.bytes, "reservation can only shrink");
        if let Ok(mut usage) = self.budget.node.lock() {
            usage.bytes -= self.bytes - bytes;
        }
        if let Some(parent) = &mut self.parent {
            parent.shrink_to(bytes);
        }
        self.bytes = bytes;
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if let Ok(mut usage) = self.budget.node.lock() {
            usage.items -= 1;
            usage.bytes -= self.bytes;
        }
    }
}

pub(crate) trait RetainedSize {
    fn retained_size(&self) -> usize;
}

#[derive(Debug)]
pub(crate) struct Queued<T> {
    pub value: T,
    pub reservation: Reservation,
}

impl<T> Queued<T> {
    pub fn cancelled(&self) -> bool {
        self.reservation.cancelled()
    }

    pub fn into_value(self) -> T {
        self.value
    }
}

#[derive(Debug)]
pub(crate) struct BoundedQueue<T> {
    pending: VecDeque<Queued<T>>,
    budget: QueueBudget,
    max_item_bytes: usize,
}

impl<T: RetainedSize> Default for BoundedQueue<T> {
    fn default() -> Self {
        let limits = WorkLimits::default();
        Self::new(QueueBudget::new(limits.queue_items, limits.queue_bytes), limits.argument_bytes)
    }
}

impl<T: RetainedSize> BoundedQueue<T> {
    pub fn new(budget: QueueBudget, max_payload_bytes: usize) -> Self {
        Self { pending: VecDeque::new(), budget,
            max_item_bytes: max_payload_bytes.saturating_add(std::mem::size_of::<T>()) }
    }

    pub fn push(&mut self, value: T) -> Result<(), ErrorCode> {
        self.push_from(value, &self.budget.clone())
    }

    pub fn push_from(&mut self, value: T, budget: &QueueBudget) -> Result<(), ErrorCode> {
        let bytes = value.retained_size();
        if bytes > self.max_item_bytes {
            return Err(ErrorCode::InvalidArgument);
        }
        let reservation = budget.reserve(bytes)?;
        self.pending.push_back(Queued { value, reservation });
        Ok(())
    }

    /// A reserved HTTP completion cannot lose its slot to later posts.
    #[cfg(any(feature = "net", test))]
    pub fn push_reserved(&mut self, value: T, mut reservation: Reservation) -> Result<(), ErrorCode> {
        let bytes = value.retained_size();
        if bytes > self.max_item_bytes || bytes > reservation.bytes || reservation.cancelled() {
            return Err(ErrorCode::InvalidArgument);
        }
        reservation.shrink_to(bytes);
        self.pending.push_back(Queued { value, reservation });
        Ok(())
    }

    pub fn pop(&mut self) -> Option<Queued<T>> {
        while let Some(value) = self.pending.pop_front() {
            if !value.cancelled() {
                return Some(value);
            }
        }
        None
    }

    pub fn return_front(&mut self, value: Queued<T>) {
        self.pending.push_front(value);
    }

    pub fn take_all(&mut self) -> Vec<Queued<T>> {
        self.pending.drain(..).filter(|v| !v.cancelled()).collect()
    }

    pub fn drain(&mut self) -> Vec<T> {
        self.take_all().into_iter().map(Queued::into_value).collect()
    }

    pub fn len(&self) -> usize { self.pending.len() }
    pub fn is_empty(&self) -> bool { self.pending.is_empty() }
    pub fn clear(&mut self) { self.pending.clear(); }
    pub fn retain(&mut self, mut keep: impl FnMut(&T) -> bool) {
        self.pending.retain(|v| !v.cancelled() && keep(&v.value));
    }
}

pub(crate) struct DispatchBudget {
    started: Instant,
    duration: Duration,
    #[cfg(feature = "metrics")]
    limits: WorkLimits,
    hooks: usize,
    visits: usize,
    fuel: u64,
}

#[derive(Clone, Copy)]
pub(crate) struct Grant {
    fuel: u64,
    aggregate_fuel: bool,
    aggregate_time: bool,
}

impl DispatchBudget {
    pub fn new(limits: WorkLimits) -> Self {
        Self { started: Instant::now(), duration: limits.dispatch_budget,
            hooks: limits.dispatch_hooks, visits: limits.dispatch_visits,
            fuel: limits.dispatch_fuel,
            #[cfg(feature = "metrics")]
            limits }
    }

    pub fn exhausted(&self) -> bool {
        self.hooks == 0 || self.visits == 0 || self.fuel == 0 || self.started.elapsed() >= self.duration
    }

    /// Capture runs before input ownership is committed, but may use at most
    /// half the remaining aggregate resources. Unused credits return to the
    /// ordinary phases; both slices share the original frame's wall deadline.
    pub fn capture_slice(&mut self) -> Self {
        let elapsed = self.started.elapsed();
        let slice = Self {
            started: self.started,
            duration: elapsed + self.duration.saturating_sub(elapsed) / 2,
            hooks: self.hooks.div_ceil(2), visits: self.visits.div_ceil(2),
            fuel: self.fuel.div_ceil(2),
            #[cfg(feature = "metrics")]
            limits: self.limits,
        };
        self.hooks -= slice.hooks;
        self.visits -= slice.visits;
        self.fuel -= slice.fuel;
        slice
    }

    pub fn return_unused(&mut self, slice: Self) {
        self.hooks += slice.hooks;
        self.visits += slice.visits;
        self.fuel += slice.fuel;
    }

    pub fn visit(&mut self) -> bool {
        if self.exhausted() { return false; }
        self.visits -= 1;
        true
    }

    pub fn arm<T>(&mut self, store: &mut wasmtime::Store<T>, limits: Limits) -> Option<Grant> {
        if self.hooks == 0 || self.fuel == 0 { return None; }
        let remaining = self.duration.checked_sub(self.started.elapsed())?;
        if remaining.is_zero() { return None; }
        let fuel = self.fuel.min(limits.hook_fuel);
        store.set_fuel(fuel).expect("host engine enables fuel");
        store.set_epoch_deadline(epoch_ticks(remaining.min(limits.hook_budget)));
        self.hooks -= 1;
        Some(Grant { fuel, aggregate_fuel: fuel < limits.hook_fuel,
            aggregate_time: remaining < limits.hook_budget })
    }

    /// Returns true for an aggregate-budget trap, which yields the instance for
    /// this frame rather than blaming it for fuel/time used by other guests.
    pub fn finish<T>(&mut self, store: &wasmtime::Store<T>, grant: Grant, result: &Result<(), wasmtime::Error>) -> bool {
        self.fuel = self.fuel.saturating_sub(grant.fuel.saturating_sub(store.get_fuel().unwrap_or(0)));
        let trap = result.as_ref().err().and_then(|e| e.downcast_ref::<wasmtime::Trap>());
        matches!(trap, Some(wasmtime::Trap::OutOfFuel) if grant.aggregate_fuel)
            || matches!(trap, Some(wasmtime::Trap::Interrupt) if grant.aggregate_time)
    }

    #[cfg(feature = "metrics")]
    pub fn metrics(&self) -> DispatchMetrics {
        DispatchMetrics { hook_calls: self.limits.dispatch_hooks - self.hooks,
            visits: self.limits.dispatch_visits - self.visits,
            fuel_consumed: self.limits.dispatch_fuel - self.fuel,
            elapsed: self.started.elapsed(), exhausted: self.exhausted() }
    }
}

/// Last dispatch's actual work (timing/diagnostics only; control is always on).
#[cfg(feature = "metrics")]
#[derive(Debug, Clone, Copy, Default)]
pub struct DispatchMetrics {
    pub hook_calls: usize,
    pub visits: usize,
    pub fuel_consumed: u64,
    pub elapsed: Duration,
    pub exhausted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(budget: &QueueBudget) -> (usize, usize) {
        let usage = budget.node.lock().unwrap();
        (usage.items, usage.bytes)
    }

    #[test]
    fn shared_reservations_rollback_shrink_and_release() {
        let root = QueueBudget::new(2, 128);
        let a = root.child(1, 64);
        let b = root.child(1, 64);
        let first = a.reserve(64).unwrap();
        assert!(a.reserve(1).is_err());
        assert_eq!(usage(&root), (1, 64), "local rejection leaked ancestor credit");
        assert!(b.reserve(65).is_err());
        let mut second = b.reserve(64).unwrap();
        assert!(root.reserve(0).is_err(), "zero-byte entries still cost a slot");
        drop(first);
        second.shrink_to(32);
        assert_eq!(usage(&root), (1, 32));
        assert_eq!(usage(&b), (1, 32));
        drop(second);
        assert_eq!(usage(&root), (0, 0));
    }

    #[derive(Debug)]
    struct Bytes(Vec<u8>);
    impl RetainedSize for Bytes {
        fn retained_size(&self) -> usize { std::mem::size_of::<Self>() + self.0.capacity() }
    }

    #[test]
    fn capacity_and_inflight_items_remain_charged() {
        let root = QueueBudget::new(1, 256);
        let owner = root.child(1, 256);
        let mut queue = BoundedQueue::new(owner.clone(), 128);
        let mut large = Vec::with_capacity(129);
        large.push(1);
        assert!(queue.push(Bytes(large)).is_err(), "length alone misses retained capacity");
        queue.push(Bytes(vec![1, 2])).unwrap();
        let inflight = queue.pop().unwrap();
        assert!(queue.push(Bytes(vec![])).is_err(), "pop cannot free a delivery's reservation");
        owner.close();
        assert!(inflight.cancelled());
        drop(inflight);
        assert_eq!(usage(&root), (0, 0));
        assert!(owner.reserve(0).is_err(), "closed owner cannot publish again");
        root.reserve(256).unwrap();
    }

    #[test]
    fn reserved_completion_keeps_its_slot_and_releases_unused_bytes() {
        let root = QueueBudget::new(1, 256);
        let mut queue = BoundedQueue::new(root.clone(), 128);
        let reserved = root.reserve(256).unwrap();
        assert!(queue.push(Bytes(vec![9])).is_err());
        queue.push_reserved(Bytes(vec![1]), reserved).unwrap();
        assert_eq!(usage(&root), (1, std::mem::size_of::<Bytes>() + 1));
        assert_eq!(queue.drain()[0].0, [1]);
        assert_eq!(usage(&root), (0, 0));
    }
}
