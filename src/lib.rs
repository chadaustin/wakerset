#![no_std]

use arrayvec::ArrayVec;
use core::cell::UnsafeCell;
use core::marker::PhantomPinned;
use core::mem::offset_of;
use core::mem::MaybeUninit;
use core::ops::DerefMut;
use core::pin::Pin;
use core::ptr;
use core::ptr::addr_of_mut;
use core::sync::atomic::AtomicPtr;
use core::sync::atomic::Ordering;
use core::task::Waker;

const EXTRACT_CAPACITY: usize = 7;

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
    unsafe fn knot(ptrs: *mut Pointers) {
        // SAFETY: We are pinned and not moving anything.
        unsafe {
            if (*ptrs).next.is_null() {
                assert!(
                    (*ptrs).prev.is_null(),
                    "either both are null or neither are"
                );
                (*ptrs).next = ptrs;
                (*ptrs).prev = ptrs;
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
    unsafe fn link_back(list: *mut Pointers, node: *mut Pointers) {
        // SAFETY: TODO
        unsafe {
            Pointers::knot(list);
            assert!((*node).next.is_null());
            assert!((*node).prev.is_null());
            (*node).next = list;
            (*node).prev = (*list).prev;
            (*(*list).prev).next = node;
            (*list).prev = node;
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
    pub fn link(self: Pin<&mut Self>, slot: Pin<&mut WakerSlot>, waker: Waker) {
        unsafe {
            let selfp = self.get_unchecked_mut() as *mut Self;
            let slotp = slot.get_unchecked_mut() as *mut WakerSlot;

            if (*slotp).is_linked() {
                *(*slotp).waker.assume_init_mut().deref_mut() = waker.into();
            } else {
                Pointers::link_back(
                    addr_of_mut!((*selfp).pointers),
                    addr_of_mut!((*slotp).pointers),
                );
                addr_of_mut!((*slotp).waker).write(MaybeUninit::new(UnsafeCell::new(waker)));
            }
            (*slotp).list.store(selfp, Ordering::Release);
        }
    }

    /// Unlinks a slot from the list, dropping its [core::task::Waker].
    /// No-op if not linked.
    pub fn unlink(self: Pin<&mut Self>, mut slot: Pin<&mut WakerSlot>) {
        // Relaxed is safe because the list mutex is acquired.
        let slot_list = slot.list.load(Ordering::Relaxed);
        if slot_list.is_null() {
            // Caller probably already checked is_link() before
            // unlink(), but the slot was unlinked before acquiring
            // the list mutex.
            return;
        }
        // SAFETY: TODO ...
        unsafe {
            let list = self.get_unchecked_mut() as *mut _;
            assert_eq!(list, slot_list, "slot must be unlinked from same list");
            slot.as_mut().pointers().unlink();
            let slot = slot.get_unchecked_mut();
            slot.waker.assume_init_drop();
            slot.list.store(ptr::null_mut(), Ordering::Release);
        }
    }

    pub fn extract_wakers(mut self: Pin<&mut Self>) -> UnlockedWakerList {
        let mut wakers = ArrayVec::new();

        // SAFETY: self is pinned
        unsafe {
            let listp = self.as_mut().pointers().get_unchecked_mut() as *mut Pointers;
            let mut p = (*listp).next;

            // Check if never been knotted.
            if p.is_null() {
                assert!((*listp).prev.is_null());
                return UnlockedWakerList::default();
            }

            loop {
                if p == listp {
                    assert_eq!((*listp).next, listp);
                    assert_eq!((*listp).prev, listp);
                    break;
                }
                if wakers.is_full() {
                    break;
                }

                let slot = p
                    .byte_sub(offset_of!(WakerSlot, pointers))
                    .cast::<WakerSlot>();

                let waker = ptr::addr_of!((*slot).waker)
                    .read()
                    .assume_init_read()
                    .into_inner();
                wakers.push(waker);

                //wakers.push((*slot).waker.assume_init_read().into_inner());

                // Unlink this node.
                (*(*p).next).prev = (*p).prev;
                (*(*p).prev).next = (*p).next;

                let next = (*p).next;

                (*p).next = ptr::null_mut();
                (*p).prev = ptr::null_mut();

                // Advertise unlinked to any racing is_linked() call.
                (*slot).list.store(ptr::null_mut(), Ordering::Release);

                p = next;
            }
        };

        UnlockedWakerList { wakers }
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
    wakers: ArrayVec<Waker, EXTRACT_CAPACITY>,
}

impl Default for UnlockedWakerList {
    fn default() -> Self {
        Self {
            wakers: ArrayVec::new(),
        }
    }
}

// TODO: impl Drop for UnlockedWakerList

impl UnlockedWakerList {
    // TODO: document must release the lock before invoking wakers
    pub fn notify_all(mut self) {
        // Generated code has no memcpy with drain(..).
        for waker in self.wakers.drain(..) {
            waker.wake();
        }
    }
}

/// A [core::future::Future]'s waker registration slot.
#[derive(Debug)]
pub struct WakerSlot {
    // This data structure is entirely synchronized by the WakerList's
    // mutex.
    /// When linked, points to the owning list. Will not invalidate
    /// because WakerList Drop explodes if non-empty. Only written
    /// under the WakerList lock, but may be optimistically checked
    /// outside of the lock with `is_linked`. If non-null, the
    /// pointers are linked into the referenced WakerList.
    list: AtomicPtr<WakerList>,
    /// Null pointers or linked into WakerList.
    /// When linked, `waker` is valid.
    pointers: Pointers,
    // Aliasable = writes safe outside of stacked borrows model
    // MaybeUninit = only set iff linked
    // UnsafeCell = may be mutated through shared references
    waker: MaybeUninit<UnsafeCell<Waker>>,
}

unsafe impl Send for WakerSlot {}

impl Default for WakerSlot {
    fn default() -> Self {
        Self {
            list: AtomicPtr::new(ptr::null_mut()),
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
    /// TODO: document the race
    pub fn is_linked(&self) -> bool {
        !self.list.load(Ordering::Acquire).is_null()
    }

    fn pointers(self: Pin<&mut Self>) -> Pin<&mut Pointers> {
        // SAFETY: pointers is pinned when self is
        unsafe { self.map_unchecked_mut(|s| &mut s.pointers) }
    }
}
