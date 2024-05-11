#![no_std]

use core::cell::UnsafeCell;
use core::marker::PhantomPinned;
use core::mem;
use core::mem::offset_of;
use core::mem::MaybeUninit;
use core::ops::DerefMut;
use core::pin::Pin;
use core::ptr;
use core::sync::atomic::AtomicU8;
use core::sync::atomic::Ordering;
use core::task::Waker;
use pinned_aliasable::Aliasable;

/// The linkage in a doubly-linked list. Used as the list container
/// (pointing to head and tail), each node (pointing to prev and
/// next), and used as a singly-linked waker list (no prev).
///
/// The default, when it's still movable, is two null pointers. When
/// pinned, it can transition to self-referential, where both `next`
/// and `prev` point to self.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
struct Pointers {
    next: *mut Pointers,
    prev: *mut Pointers,
    pinned: PhantomPinned,
}

impl Default for Pointers {
    fn default() -> Self {
        Self {
            next: ptr::null_mut(),
            prev: ptr::null_mut(),
            pinned: PhantomPinned,
        }
    }
}

impl Pointers {
    /// Now that we know this node is pinned, if the pointers are
    /// null, point to ourselves.
    fn knot(self: Pin<&mut Self>) {
        // SAFETY: We are pinned and not moving anything.
        unsafe {
            let selfp = self.get_unchecked_mut() as *mut Pointers;
            if (*selfp).next.is_null() {
                assert!(
                    (*selfp).prev.is_null(),
                    "either both are null or neither are"
                );
                (*selfp).next = selfp;
                (*selfp).prev = selfp;
            }
        }
    }

    /// Is empty when either null or self-linked.
    fn is_empty(&self) -> bool {
        self.next.is_null() || (self as *const Pointers == self.next)
    }

    /// Returns whether next is non-null.
    fn is_linked(&self) -> bool {
        !self.next.is_null()
    }

    /// Unlink this node from its neighbors.
    // TODO: Is this function safe or not?
    fn unlink(self: Pin<&mut Self>) {
        // SAFETY: self is pinned, and we assume that next and prev
        // are valid and locked
        unsafe {
            let elem = self.get_unchecked_mut() as *mut Pointers;
            let next = (*elem).next;
            let prev = (*elem).prev;
            assert!(!next.is_null());
            assert!(!prev.is_null());
            (*prev).next = next;
            (*next).prev = prev;
            (*elem).next = ptr::null_mut();
            (*elem).prev = ptr::null_mut();
        }
    }

    /// Link a value to the back of the list. The value must be
    /// unlinked.
    unsafe fn link_back(mut self: Pin<&mut Self>, value: Pin<&mut Pointers>) {
        self.as_mut().knot();
        // SAFETY: both pointers are pinned
        unsafe {
            let list = self.get_unchecked_mut() as *mut Pointers;
            let elem = value.get_unchecked_mut() as *mut Pointers;
            assert!((*elem).next.is_null());
            assert!((*elem).prev.is_null());
            (*elem).next = list;
            (*elem).prev = (*list).prev;
            (*(*list).prev).next = elem;
            (*list).prev = elem;
        }
    }
}

/// An ordered list of [core::task::Waker]s.
///
/// Does not allocate: the wakers themselves are stored in
/// [WakerSlot]. Only two pointers in size.

#[derive(Debug, Default)]
pub struct WakerList {
    // `next` is head and `prev` is tail. Upon default initialization,
    // the pointers are null and movable. On first pinned use, they
    // are knotted and become self-referential.
    pointers: Pointers,
}

unsafe impl Send for WakerList {}

#[allow(non_snake_case)]
#[inline(never)]
#[cold]
extern "C" fn MUST_UNLINK_ALL_WakerSlots_BEFORE_DROPPING_LIST() -> ! {
    // panic! from extern "C" is an abort with an error message.
    panic!("Must unlink all WakerSlots before dropping list")
    // Another option, at the cost of a tiny, stable, dependency, is
    // the `abort` crate.
    //abort::abort()
}

impl Drop for WakerList {
    fn drop(&mut self) {
        if !self.pointers.is_empty() {
            // We cannot panic, because it is UB to deallocate the
            // list while slots hold references.
            MUST_UNLINK_ALL_WakerSlots_BEFORE_DROPPING_LIST();
        }
    }
}

impl WakerList {
    /// Returns an empty list.
    pub fn new() -> Self {
        Default::default()
    }

    /// Adds a [core::task::Waker] to the list, storing it in [WakerSlot], which is linked into the [WakerList].
    pub fn link(mut self: Pin<&mut Self>, mut slot: Pin<&mut WakerSlot>, waker: Waker) {
        // SAFETY: TODO ...
        unsafe {
            // TODO: spinlock until slot.state != SLOT_PENDING_WAKE

            if slot.is_linked() {
                let slot = slot.get_unchecked_mut();
                slot.list = self.get_unchecked_mut() as *mut _;
                slot.state.store(STATE_LINKED, Ordering::Release);
                *slot.waker.assume_init_mut().deref_mut() = waker.into();
            } else {
                self.as_mut().pointers().link_back(slot.as_mut().pointers());

                let slot = slot.get_unchecked_mut();
                slot.list = self.get_unchecked_mut() as *mut _;

                // The list lock is held, and we

                // TODO: CAS
                slot.state.store(STATE_LINKED, Ordering::Release);
                slot.waker.write(UnsafeCell::new(waker));
            }
        }
    }

    /// Unlinks a slot from the list, dropping its [core::task::Waker].
    /// Panics if not linked.
    pub fn unlink(self: Pin<&mut Self>, mut slot: Pin<&mut WakerSlot>) {
        assert!(!slot.list.is_null(), "unlink called on unlinked slot");
        // SAFETY: TODO ...
        unsafe {
            let list = self.get_unchecked_mut() as *mut _;
            assert_eq!(list, slot.list, "slot must be unlinked from same list");
            slot.as_mut().pointers().unlink();
            let slot = slot.get_unchecked_mut();
            slot.list = ptr::null_mut();
            // TODO: update `state`
            slot.waker.assume_init_drop();
        }
    }

    pub fn extract_wakers(mut self: Pin<&mut Self>) -> UnlockedWakerList {
        // TODO: self.knot()
        // SAFETY: self is pinned
        let head = unsafe {
            let selfp = self.as_mut().pointers().get_unchecked_mut() as *mut Pointers;
            let next = (*selfp).next;
            let prev = (*selfp).prev;

            if next.is_null() {
                assert!(prev.is_null());
                ptr::null_mut()
            } else if next == selfp {
                assert_eq!(prev, selfp);
                ptr::null_mut()
            } else {
                // Convert the existing list to singly-linked.
                (*prev).next = ptr::null_mut();
                let head = next;
                // Knot back to empty.
                (*selfp).next = selfp;
                (*selfp).prev = selfp;
                head
            }
        };
        // If WakerList is protected by a lock, it is held. Mark every
        // slot as PENDING_WAKE.
        // TODO: actually mark slots as reserved
        UnlockedWakerList { head }
    }

    fn pointers(self: Pin<&mut Self>) -> Pin<&mut Pointers> {
        // SAFETY: pointers is pinned when self is
        unsafe { self.map_unchecked_mut(|s| &mut s.pointers) }
    }
}

/// List of [core::task::Waker] extracted from [WakerList] so that
/// [core::task::Waker::wake] can be called outside of any mutex
/// protecting [WakerList].
#[derive(Debug)]
pub struct UnlockedWakerList {
    // `prev` is not used. null denotes the end.
    head: *mut Pointers,
}

impl Default for UnlockedWakerList {
    fn default() -> Self {
        Self {
            head: ptr::null_mut(),
        }
    }
}

// TODO: impl Drop for UnlockedWakerList

impl UnlockedWakerList {
    // TODO: must release the lock before invoking wakers
    pub fn notify_all(mut self) {
        let mut p = mem::replace(&mut self.head, ptr::null_mut());
        while !p.is_null() {
            unsafe {
                // TODO: properly unlink
                let next = (*p).next;
                (*p).next = ptr::null_mut();
                (*p).prev = ptr::null_mut();

                let slot = p
                    .byte_sub(offset_of!(WakerSlot, pointers))
                    .cast::<WakerSlot>();
                let w = Pin::new_unchecked(&(*slot).waker);
                // extract waker
                let waker = w.assume_init_read();
                // TODO: set as empty again
                waker.into_inner().wake();
                p = next;
            }
        }
    }
}

/**
 * Possible state transitions:
 * EMPTY -> LINKED
 *   - WakerList lock must be held
 * LINKED -> EMPTY
 *   - WakerList lock must be held
 * LINKED -> PENDING_WAKE
 *   - WakerList lock must be held
 * PENDING_WAKE -> EMPTY
 *   - no locks held.
 */

const STATE_EMPTY: u8 = 0;
const STATE_LINKED: u8 = 1;
const STATE_PENDING_WAKE: u8 = 2;

/// A [core::future::Future]'s waker registration slot.
#[derive(Debug)]
pub struct WakerSlot {
    // Invariants follow:
    state: AtomicU8,
    /// When linked, points to the owning list. Will not invalidate
    /// because WakerList Drop explodes if non-empty.
    /// Must only be modified under the WakerList lock.
    list: *mut WakerList,
    /// Null pointers or linked into WakerList.
    /// Transitions to linked under the WakerList lock.
    /// Transitions to unlinked on STATE_PENDING_WAKE -> STATE_EMPTY.
    pointers: Pointers,
    // Aliasable = writes safe outside of stacked borrows model
    // MaybeUninit = only set iff state != SLOT_EMPTY
    // UnsafeCell = may be mutated through shared references
    waker: MaybeUninit<UnsafeCell<Waker>>,
}

unsafe impl Send for WakerSlot {}

impl Default for WakerSlot {
    fn default() -> Self {
        Self {
            state: AtomicU8::new(STATE_EMPTY),
            list: ptr::null_mut(),
            pointers: Pointers::default(),
            waker: MaybeUninit::uninit(),
        }
    }
}

#[allow(non_snake_case)]
#[inline(never)]
#[cold]
extern "C" fn MUST_UNLINK_WakerSlot_BEFORE_DROP() -> ! {
    // panic! from extern "C" is an abort with an error message.
    panic!("Must unlink WakerSlot before drop")
    // Another option, at the cost of a tiny, stable, dependency, is
    // the `abort` crate.
    //abort::abort()
}

impl Drop for WakerSlot {
    fn drop(&mut self) {
        if self.pointers.is_linked() {
            // It is undefined behavior to drop a linked WakerSlot
            // while linked and the WakerList's mutex is not held, so
            // all we can do is abort. panic! could be caught and
            // therefore UB observed.
            MUST_UNLINK_WakerSlot_BEFORE_DROP()
        }
    }
}

impl WakerSlot {
    /// Returns an empty slot.
    pub fn new() -> WakerSlot {
        Default::default()
    }

    /// Returns whether this slot is linked into the [WakerList] (and
    /// therefore has a waker).
    pub fn is_linked(&self) -> bool {
        // TODO: spin on state != STATE_PENDING_WAKE
        self.pointers.is_linked()
    }

    fn pointers(self: Pin<&mut Self>) -> Pin<&mut Pointers> {
        // SAFETY: pointers is pinned when self is
        unsafe { self.map_unchecked_mut(|s| &mut s.pointers) }
    }
}
