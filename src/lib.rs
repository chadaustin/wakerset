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

/// The linkage in a doubly-linked list. Used both by the list
/// container (pointing to head and tail) and each node (pointing to
/// prev and next).
///
/// There are two representations of the empty list: both pointers are
/// null or they both point to self.
///
/// Null is the default, because it's movable. Once pinned, we know it
/// will never be moved again, so it's made cyclic, where `next` and
/// `prev` point to self.
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
    /// null, point to ourselves. This simplifies all remaining
    /// pointer updates.
    unsafe fn ensure_cyclic(node: *mut Pointers) {
        // SAFETY: `node` is pinned and nothing will move again.
        // MIRI: No references are formed.
        unsafe {
            let nextp = addr_of_mut!((*node).next);
            let prevp = addr_of_mut!((*node).prev);
            if nextp.read().is_null() {
                assert!(
                    prevp.read().is_null(),
                    "either both are null or neither are"
                );
                nextp.write(node);
                prevp.write(node);
            }
        }
    }

    /// Is empty when either null or self-linked.
    fn is_empty(&self) -> bool {
        // TODO: Is forming a reference here compatible with MIRI?
        self.next.is_null() || (self as *const Pointers == self.next)
    }

    /// Unlink this node from its neighbors.
    unsafe fn unlink(node: *mut Pointers) {
        // SAFETY: self is pinned, and we assume that next and prev
        // are valid and locked
        unsafe {
            let nextp = addr_of_mut!((*node).next);
            let prevp = addr_of_mut!((*node).prev);
            let next = nextp.read();
            let prev = prevp.read();
            assert!(!next.is_null());
            assert!(!prev.is_null());
            addr_of_mut!((*prev).next).write(next);
            addr_of_mut!((*next).prev).write(prev);
            nextp.write(ptr::null_mut());
            prevp.write(ptr::null_mut());
        }
    }

    /// Link a value to the back of the list. The value must be
    /// unlinked.
    unsafe fn link_back(list: *mut Pointers, node: *mut Pointers) {
        // SAFETY: TODO
        unsafe {
            Pointers::ensure_cyclic(list);
            let nodenextp = addr_of_mut!((*node).next);
            let nodeprevp = addr_of_mut!((*node).prev);
            assert!(nodenextp.read().is_null());
            assert!(nodeprevp.read().is_null());
            nodenextp.write(list);
            nodeprevp.write(addr_of_mut!((*list).prev).read());
            addr_of_mut!((*(*list).prev).next).write(node);
            addr_of_mut!((*list).prev).write(node);
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
    // the pointers are null and movable. On first pinned use, we know
    // it will never be moved again, so they become self-referential.
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
            let slotp = slot.as_mut().get_unchecked_mut() as *mut WakerSlot;
            Pointers::unlink(addr_of_mut!((*slotp).pointers));
            let slot = slot.get_unchecked_mut();
            slot.waker.assume_init_drop();
            slot.list.store(ptr::null_mut(), Ordering::Release);
        }
    }

    pub fn extract_wakers(self: Pin<&mut Self>) -> UnlockedWakerList {
        let mut wakers = ArrayVec::new();

        // SAFETY: self is pinned
        unsafe {
            let selfp = self.get_unchecked_mut() as *mut Self;
            let listp = addr_of_mut!((*selfp).pointers);

            let mut p = addr_of_mut!((*listp).next).read();

            // If null, then not cyclic, and we can just return.
            if p.is_null() {
                assert!(addr_of_mut!((*listp).prev).read().is_null());
                return UnlockedWakerList::default();
            }

            loop {
                if p == listp {
                    assert_eq!(addr_of_mut!((*listp).next).read(), listp);
                    assert_eq!(addr_of_mut!((*listp).prev).read(), listp);
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

                let next = addr_of_mut!((*p).next).read();
                let prev = addr_of_mut!((*p).prev).read();

                // Unlink this node.
                addr_of_mut!((*next).prev).write(prev);
                addr_of_mut!((*prev).next).write(next);

                addr_of_mut!((*p).next).write(ptr::null_mut());
                addr_of_mut!((*p).prev).write(ptr::null_mut());

                // Advertise unlinked to any racing is_linked() call.
                (*slot).list.store(ptr::null_mut(), Ordering::Release);

                p = next;
            }
        };

        UnlockedWakerList { wakers }
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
        if self.is_linked() {
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
}
