use core::mem;
use core::panic::AssertUnwindSafe;
use core::pin::pin;
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering;
use core::task::Waker;
use std::panic::catch_unwind;
use std::pin::Pin;
use std::sync::Arc;
use wakerset::ExtractedWakers;
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

/// Returns the number of rounds needed to wake all wakers.
fn wake_all(mut list: Pin<&mut WakerList>) -> u64 {
    let round = list.as_mut().begin_extraction();
    let mut wakers = ExtractedWakers::new();
    let mut count = 0;
    let mut more = true;
    while more {
        more = list.as_mut().extract_some_wakers(round, &mut wakers);
        wakers.wake_all();
        count += 1;
    }
    count
}

#[test]
fn link_and_wake_all() {
    let task = Task::new();

    let mut list = pin!(WakerList::new());
    let slot = pin!(WakerSlot::new());
    list.as_mut().link(slot, task.waker());
    assert_eq!(1, wake_all(list));

    assert_eq!(1, task.wake_count());
}

#[test]
fn link_and_wake_all_two_tasks() {
    let task = Task::new();

    let mut list = pin!(WakerList::new());
    let slot1 = pin!(WakerSlot::new());
    let slot2 = pin!(WakerSlot::new());
    list.as_mut().link(slot1, task.waker());
    list.as_mut().link(slot2, task.waker());
    assert_eq!(1, wake_all(list));

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

    assert_eq!(1, wake_all(list));
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

    assert_eq!(1, wake_all(list.as_mut()));
    assert_eq!(0, task.wake_count());

    list.as_mut().link(slot1.as_mut(), task.waker());
    assert!(slot1.is_linked());

    assert_eq!(1, wake_all(list));
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
fn unlink_without_link_is_no_op() {
    let mut list = pin!(WakerList::new());
    let mut slot = pin!(WakerSlot::new());
    list.as_mut().unlink(slot.as_mut());
    list.as_mut().unlink(slot.as_mut());
}

#[test]
fn relink_wrong_list_panics() {
    let task = Task::new();

    let mut list1 = Box::pin(WakerList::new());
    let mut list2 = Box::pin(WakerList::new());
    let mut slot = Box::pin(WakerSlot::new());
    list1.as_mut().link(slot.as_mut(), task.waker());
    let result = catch_unwind(AssertUnwindSafe(|| {
        list2.as_mut().link(slot.as_mut(), task.waker());
    }));
    assert!(result.is_err());
    list1.as_mut().unlink(slot.as_mut());
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

#[test]
fn second_link_replaces_waker() {
    let task1 = Task::new();
    let task2 = Task::new();

    let mut list = Box::pin(WakerList::new());
    let mut slot = Box::pin(WakerSlot::new());
    list.as_mut().link(slot.as_mut(), task1.waker());
    list.as_mut().link(slot.as_mut(), task2.waker());

    assert_eq!(1, wake_all(list.as_mut()));
    assert_eq!(0, task1.wake_count());
    assert_eq!(1, task2.wake_count());
}

#[test]
fn just_full_wakers_needs_no_continue() {
    // This test relies on EXTRACT_CAPACITY being seven.
    let the_task = Task::new();
    let mut list = pin!(WakerList::new());

    let mut slot1 = pin!(WakerSlot::new());
    list.as_mut().link(slot1.as_mut(), the_task.waker());

    let mut slot2 = pin!(WakerSlot::new());
    list.as_mut().link(slot2.as_mut(), the_task.waker());

    let mut slot3 = pin!(WakerSlot::new());
    list.as_mut().link(slot3.as_mut(), the_task.waker());

    let mut slot4 = pin!(WakerSlot::new());
    list.as_mut().link(slot4.as_mut(), the_task.waker());

    let mut slot5 = pin!(WakerSlot::new());
    list.as_mut().link(slot5.as_mut(), the_task.waker());

    let mut slot6 = pin!(WakerSlot::new());
    list.as_mut().link(slot6.as_mut(), the_task.waker());

    let mut slot7 = pin!(WakerSlot::new());
    list.as_mut().link(slot7.as_mut(), the_task.waker());

    // There are not 8, so no need to continue.
    assert_eq!(1, wake_all(list));
    assert_eq!(7, the_task.wake_count());
}

#[test]
fn full_wakers_needs_continue() {
    // This test relies on EXTRACT_CAPACITY being seven.
    let the_task = Task::new();
    let mut list = pin!(WakerList::new());

    let mut slot1 = pin!(WakerSlot::new());
    list.as_mut().link(slot1.as_mut(), the_task.waker());

    let mut slot2 = pin!(WakerSlot::new());
    list.as_mut().link(slot2.as_mut(), the_task.waker());

    let mut slot3 = pin!(WakerSlot::new());
    list.as_mut().link(slot3.as_mut(), the_task.waker());

    let mut slot4 = pin!(WakerSlot::new());
    list.as_mut().link(slot4.as_mut(), the_task.waker());

    let mut slot5 = pin!(WakerSlot::new());
    list.as_mut().link(slot5.as_mut(), the_task.waker());

    let mut slot6 = pin!(WakerSlot::new());
    list.as_mut().link(slot6.as_mut(), the_task.waker());

    let mut slot7 = pin!(WakerSlot::new());
    list.as_mut().link(slot7.as_mut(), the_task.waker());

    let mut slot8 = pin!(WakerSlot::new());
    list.as_mut().link(slot8.as_mut(), the_task.waker());

    // There are 8, so we need a second call.
    assert_eq!(2, wake_all(list));
    assert_eq!(8, the_task.wake_count());
}

#[test]
fn only_extract_wakers_since_beginning_of_extraction() {
    // This test relies on EXTRACT_CAPACITY being seven.
    let the_task = Task::new();
    let mut list = pin!(WakerList::new());

    let mut slot1 = pin!(WakerSlot::new());
    list.as_mut().link(slot1.as_mut(), the_task.waker());

    let mut slot2 = pin!(WakerSlot::new());
    list.as_mut().link(slot2.as_mut(), the_task.waker());

    let mut slot3 = pin!(WakerSlot::new());
    list.as_mut().link(slot3.as_mut(), the_task.waker());

    let mut slot4 = pin!(WakerSlot::new());
    list.as_mut().link(slot4.as_mut(), the_task.waker());

    let mut slot5 = pin!(WakerSlot::new());
    list.as_mut().link(slot5.as_mut(), the_task.waker());

    let mut slot6 = pin!(WakerSlot::new());
    list.as_mut().link(slot6.as_mut(), the_task.waker());

    let mut slot7 = pin!(WakerSlot::new());
    list.as_mut().link(slot7.as_mut(), the_task.waker());

    let mut slot8 = pin!(WakerSlot::new());
    list.as_mut().link(slot8.as_mut(), the_task.waker());

    let round = list.as_mut().begin_extraction();
    let mut wakers = ExtractedWakers::new();

    // There are 8, so we need a second call.
    assert!(list.as_mut().extract_some_wakers(round, &mut wakers));
    wakers.wake_all();
    assert_eq!(7, the_task.wake_count());

    list.as_mut().link(slot1.as_mut(), the_task.waker());
    list.as_mut().link(slot2.as_mut(), the_task.waker());
    list.as_mut().link(slot3.as_mut(), the_task.waker());
    list.as_mut().link(slot4.as_mut(), the_task.waker());
    // TODO: We should have a test that reasons through what happens
    // when a slot is relinked during extraction.
    /*
    list.as_mut().link(slot5.as_mut(), the_task.waker());
    list.as_mut().link(slot6.as_mut(), the_task.waker());
    list.as_mut().link(slot7.as_mut(), the_task.waker());
    list.as_mut().link(slot8.as_mut(), the_task.waker());
     */

    assert!(!list.as_mut().extract_some_wakers(round, &mut wakers));
    wakers.wake_all();
    assert_eq!(8, the_task.wake_count());

    // Get the remaining half.
    let round = list.as_mut().begin_extraction();
    list.as_mut().extract_some_wakers(round, &mut wakers);
    wakers.wake_all();
    assert_eq!(12, the_task.wake_count());
}

#[test]
fn extracted_wakers_is_empty() {
    // This test relies on EXTRACT_CAPACITY being seven.
    let the_task = Task::new();
    let mut list = pin!(WakerList::new());

    let mut wakers = ExtractedWakers::new();
    let round = list.as_mut().begin_extraction();
    assert!(!list.as_mut().extract_some_wakers(round, &mut wakers));
    assert!(wakers.is_empty());

    let mut slot1 = pin!(WakerSlot::new());
    list.as_mut().link(slot1.as_mut(), the_task.waker());

    let round = list.as_mut().begin_extraction();
    assert!(!list.as_mut().extract_some_wakers(round, &mut wakers));
    assert!(!wakers.is_empty());}


#[cfg(target_pointer_width = "64")]
mod tests64 {
    use super::*;
    use core::mem;

    #[test]
    fn test_sizes() {
        assert_eq!(24, mem::size_of::<WakerList>());
        assert_eq!(48, mem::size_of::<WakerSlot>());
    }

    #[test]
    fn test_extracted_waker_set_size() {
        // Unfortunately, arrayvec::ArrayVec's use of MaybeUnunit
        // seems to disable niche-filling. That's probably okay,
        // because we can fit 15 wakers in 256 bytes. But now we're
        // okay, because I added a bool to ExtractedWakers, providing
        // a niche.
        if false {
            assert_eq!(
                mem::size_of::<ExtractedWakers>(),
                mem::size_of::<Option<ExtractedWakers>>()
            );
        }
        // It's okay to adjust this. Just wanted to measure. We're
        // trading stack copying with number of locks acquired during
        // wake.
        if true {
            assert_eq!(120, mem::size_of::<ExtractedWakers>());
        }
    }
}
