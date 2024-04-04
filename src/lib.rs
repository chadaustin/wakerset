#![no_std]

use core::marker::PhantomPinned;
use core::mem;
use core::mem::MaybeUninit;
use core::pin::Pin;
use core::ptr;
use core::sync::atomic::AtomicU8;
use core::sync::atomic::Ordering;
use core::task::Waker;

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
    /// Now that we know this node is pinned, if the pointers are
    /// null, point to ourselves.
    fn knot(mut self: Pin<&mut Self>) {
        if self.next.is_null() {
            assert!(self.prev.is_null(), "either both are null or neither are");
            let selfp = Pin::into_inner(self.as_mut()) as *mut Pointers;
            self.next = selfp;
            self.prev = selfp;
        }
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
    _pinned: PhantomPinned,
}

unsafe impl Send for WakerList {}

// TODO: impl Drop for WakerList

impl WakerList {
    /// Returns an empty list.
    pub fn new() -> Self {
        Default::default()
    }

    /// Adds a [core::task::Waker] to the list, storing it in [WakerSlot], which is linked into the [WakerList].
    pub fn link(mut self: Pin<&mut Self>, mut slot: Pin<&mut WakerSlot>, waker: Waker) {
        // assert that slot is unlinked?
        // SAFETY: TODO ...
        unsafe {
            self.as_mut().pointers().link_back(slot.as_mut().pointers());

            let slot = slot.get_unchecked_mut();
            slot.list = self.get_unchecked_mut() as *mut _;
            // TODO: CAS
            slot.state.store(SLOT_FULL, Ordering::Release);
            slot.waker.write(waker);
        }
    }

    /// Unlinks a slot from the list, dropping its [core::task::Waker].
    pub fn unlink(self: Pin<&mut Self>, mut slot: Pin<&mut WakerSlot>) {
        // SAFETY: TODO ...
        unsafe {
            let list = self.get_unchecked_mut() as *mut _;
            assert_eq!(list, slot.list, "slot must be unlinked from same list");
            slot.as_mut().pointers().unlink();
            let slot = slot.get_unchecked_mut();
            slot.list = ptr::null_mut();
            // TODO: update `state`
            slot.waker.assume_init_drop()
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

// TODO: impl Drop for WakerList

impl UnlockedWakerList {
    // TODO: must release the lock before invoking wakers
    pub fn notify_all(mut self) {
        let mut p = mem::replace(&mut self.head, ptr::null_mut());
        while !p.is_null() {
            let next = unsafe {
                // TODO: properly unlink
                let next = (*p).next;
                (*p).next = ptr::null_mut();
                (*p).prev = ptr::null_mut();
                next
            };
            let slotp = p as *mut WakerSlot;
            // extract waker
            let waker = unsafe { (*slotp).waker.assume_init_read() };
            // TODO: set as empty again
            waker.wake();
            p = next;
        }
    }
}

const SLOT_EMPTY: u8 = 0;
const SLOT_FULL: u8 = 1;
//const SLOT_PENDING_WAKE: u8 = 2;

/// A [core::future::Future]'s waker registration slot.
#[repr(C)]
#[derive(Debug)]
pub struct WakerSlot {
    pointers: Pointers,
    // Null when unlinked. Points to the owning list when linked.
    list: *mut WakerList,
    state: AtomicU8,
    waker: MaybeUninit<Waker>,
    pinned: PhantomPinned,
}

unsafe impl Send for WakerSlot {}

// assert_eq!(0, mem::offset_of!(WakerSlot, pointers));
const _: [(); 0] = [(); mem::offset_of!(WakerSlot, pointers)];

impl Default for WakerSlot {
    fn default() -> Self {
        Self {
            pointers: Pointers::default(),
            list: ptr::null_mut(),
            state: AtomicU8::new(SLOT_EMPTY),
            waker: MaybeUninit::uninit(),
            pinned: PhantomPinned,
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
        self.pointers.is_linked()
    }

    fn pointers(self: Pin<&mut Self>) -> Pin<&mut Pointers> {
        // SAFETY: pointers is pinned when self is
        unsafe { self.map_unchecked_mut(|s| &mut s.pointers) }
    }
}
