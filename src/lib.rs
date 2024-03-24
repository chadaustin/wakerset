#![no_std]

use core::cell::UnsafeCell;
use core::mem;
use core::pin::Pin;
use core::mem::MaybeUninit;
use core::ptr;
use core::sync::atomic::AtomicU8;
use core::task::Waker;

#[repr(C)]
#[derive(Copy, Clone, Debug)]
struct Pointers {
    next: *mut Pointers,
    prev: *mut Pointers,
}

impl Default for Pointers {
    fn default() -> Self {
        Self {
            next: ptr::null_mut(),
            prev: ptr::null_mut(),
        }
    }
}

impl Pointers {
    fn knot(mut self: Pin<&mut Self>) {
        if self.next.is_null() {
            assert!(self.prev.is_null(), "either both are null or neither are");
            self.next = Pin::into_inner(self.as_mut()) as *mut Pointers;
            self.prev = Pin::into_inner(self.as_mut()) as *mut Pointers;
        }
    }
}

#[derive(Debug, Default)]
pub struct WakerSet {
    pointers: Pointers,
}

// TODO: impl Drop for WakerSet

impl WakerSet {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn link(self: Pin<&mut Self>, mut slot: Pin<&mut WakerSlot>, waker: Waker) {
        self.pointers().knot();
        // do state transition on slot.state
        slot.as_mut().pointers().knot();
        slot.get_mut().slot.waker.write(waker);
    }

    pub fn extract_wakers(mut self: Pin<&mut Self>) -> WakerList {
        // If WakerSet is protected by a lock, it is held. Mark every
        // slot as PENDING_WAKE.
        // TODO: actually mark slots as reserved
        WakerList { pointers: mem::take(&mut self.pointers) }
    }

    fn pointers(mut self: Pin<&mut Self>) -> Pin<&mut Pointers> {
        // SAFETY: pointers is pinned when self is
        unsafe { self.map_unchecked_mut(|s| &mut s.pointers) }
    }
}

#[derive(Debug)]
pub struct WakerList {
    pointers: Pointers,
}

// TODO: impl Drop for WakerList

impl WakerList {
    // TODO: must release the lock before invoking wakers
    fn notify_all(&mut self) {
        let root = mem::take(&mut self.pointers);
        let mut p = root.next;
        while !p.is_null() {
            let next = unsafe { (*p).next };
            unsafe {
                (*p).next = ptr::null_mut();
                (*p).prev = ptr::null_mut();
            }
            // extract waker
            p = next;
        }
    }
}

const SLOT_EMPTY: u8 = 0;
const SLOT_FULL: u8 = 1;
const SLOT_PENDING_WAKE: u8 = 2;

#[repr(C)]
#[derive(Debug)]
struct Slot {
    pointers: Pointers,
    state: AtomicU8,
    waker: MaybeUninit<Waker>,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            pointers: Pointers::default(),
            state: AtomicU8::new(SLOT_EMPTY),
            waker: MaybeUninit::uninit(),
        }
    }
}

#[derive(Debug, Default)]
pub struct WakerSlot {
    slot: Slot,
}

// TODO: impl Drop for WakerSlot

impl WakerSlot {
    fn new() -> WakerSlot {
        Default::default()
    }

    fn pointers(mut self: Pin<&mut Self>) -> Pin<&mut Pointers> {
        // SAFETY: pointers is pinned when self is
        unsafe { self.map_unchecked_mut(|s| &mut s.slot.pointers) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::pin::pin;
    use core::sync::atomic::AtomicU64;
    use core::sync::atomic::Ordering;
    extern crate std;
    use std::sync::Arc;

    struct Task {
        wake_count: AtomicU64,
    }

    impl Task {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                wake_count: AtomicU64::new(0),
            })
        }

        fn waker(self: &Arc<Self>) -> Waker {
            self.clone().into()
        }

        fn wake_count(&self) -> u64 {
            self.wake_count.load(Ordering::Acquire)
        }
    }

    impl std::task::Wake for Task {
        fn wake(self: Arc<Self>) {
            self.wake_count.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[test]
    fn new_and_drop() {
        let set = WakerSet::new();
        drop(set);
    }

    #[test]
    fn count_task_wakes() {
        let task = Task::new();
        let waker = task.waker();
        assert_eq!(0, task.wake_count());
        waker.wake();
        assert_eq!(1, task.wake_count());
    }

    #[test]
    fn link_and_notify_all() {
        let task = Task::new();

        let mut set = pin!(WakerSet::new());
        let slot = pin!(WakerSlot::new());
        set.as_mut().link(slot, task.waker());
        set.extract_wakers().notify_all();

        assert_eq!(1, task.wake_count());
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests64 {
    use super::*;
    use core::mem;

    #[test]
    fn test_sizes() {
        assert_eq!(16, mem::size_of::<WakerSet>());
        // TODO: Can we make this fit in 32?
        assert_eq!(40, mem::size_of::<WakerSlot>());
    }
}
