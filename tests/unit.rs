use core::mem;
use core::panic::AssertUnwindSafe;
use core::pin::pin;
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering;
use core::task::Waker;
use std::panic::catch_unwind;
use std::sync::Arc;
use wakerset::WakerList;
use wakerset::WakerSlot;

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
fn marker_traits() {
    use static_assertions::assert_impl_all;
    use static_assertions::assert_not_impl_any;
    assert_impl_all!(WakerList: Send);
    assert_impl_all!(WakerSlot: Send);
    assert_not_impl_any!(WakerList: Sync, Unpin);
    assert_not_impl_any!(WakerSlot: Sync, Unpin);
}

#[test]
fn new_and_drop() {
    let list = WakerList::new();
    drop(list);
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

    let mut list = pin!(WakerList::new());
    let slot = pin!(WakerSlot::new());
    list.as_mut().link(slot, task.waker());
    list.extract_wakers().notify_all();

    assert_eq!(1, task.wake_count());
}

#[test]
fn link_and_notify_all_two_tasks() {
    let task = Task::new();

    let mut list = pin!(WakerList::new());
    let slot1 = pin!(WakerSlot::new());
    let slot2 = pin!(WakerSlot::new());
    list.as_mut().link(slot1, task.waker());
    list.as_mut().link(slot2, task.waker());
    list.extract_wakers().notify_all();

    assert_eq!(2, task.wake_count());
}

#[test]
fn unlink() {
    let task = Task::new();

    let mut list = pin!(WakerList::new());
    let mut slot1 = pin!(WakerSlot::new());
    assert!(!slot1.is_linked());
    list.as_mut().link(slot1.as_mut(), task.waker());
    assert!(slot1.is_linked());
    list.as_mut().unlink(slot1.as_mut());
    assert!(!slot1.is_linked());

    list.extract_wakers().notify_all();
    assert_eq!(0, task.wake_count());
}

#[test]
fn unlink_and_relink() {
    let task = Task::new();

    let mut list = pin!(WakerList::new());
    let mut slot1 = pin!(WakerSlot::new());
    assert!(!slot1.is_linked());
    list.as_mut().link(slot1.as_mut(), task.waker());
    assert!(slot1.is_linked());
    list.as_mut().unlink(slot1.as_mut());
    assert!(!slot1.is_linked());

    list.as_mut().extract_wakers().notify_all();
    assert_eq!(0, task.wake_count());

    list.as_mut().link(slot1.as_mut(), task.waker());
    assert!(slot1.is_linked());

    list.extract_wakers().notify_all();
    assert_eq!(1, task.wake_count());
}

#[test]
fn unlink_slot_drops_waker() {
    let task = Task::new();
    let mut list = pin!(WakerList::new());
    let mut slot1 = pin!(WakerSlot::new());
    let mut slot2 = pin!(WakerSlot::new());
    list.as_mut().link(slot1.as_mut(), task.waker());
    list.as_mut().link(slot2.as_mut(), task.waker());
    // Comment out the following two lines to observe the
    // compile-time error message.
    list.as_mut().unlink(slot1.as_mut());
    list.as_mut().unlink(slot2.as_mut());
    // Uncomment the following line to assure the list must
    // outlive slots.
    //drop(list);
}

#[test]
fn unlink_wrong_list_panics() {
    let task = Task::new();

    let mut list1 = Box::pin(WakerList::new());
    let mut list2 = Box::pin(WakerList::new());
    let mut slot = Box::pin(WakerSlot::new());
    list1.as_mut().link(slot.as_mut(), task.waker());
    let result = catch_unwind(AssertUnwindSafe(|| {
        list2.as_mut().unlink(slot.as_mut());
    }));
    assert!(result.is_err());
    list1.as_mut().unlink(slot.as_mut());
}

#[test]
fn drop_list_before_slot_panics() {
    let task = Task::new();

    let mut list = Box::pin(WakerList::new());
    let mut slot = Box::pin(WakerSlot::new());
    list.as_mut().link(slot.as_mut(), task.waker());
    // Toggle to illustrate that the process terminates.
    if !true {
        mem::forget(slot);
        drop(list);
        assert!(false, "program should have terminated already");
    } else {
        list.as_mut().unlink(slot.as_mut());
    }
}

#[cfg(target_pointer_width = "64")]
mod tests64 {
    use super::*;
    use core::mem;

    #[test]
    fn test_sizes() {
        assert_eq!(16, mem::size_of::<WakerList>());
        // TODO: Can we make this fit in 40?
        assert_eq!(48, mem::size_of::<WakerSlot>());
    }
}
