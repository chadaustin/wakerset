use divan::black_box;
use divan::Bencher;
use std::pin::pin;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::task::Waker;
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

    #[allow(unused)]
    fn wake_count(&self) -> u64 {
        self.wake_count.load(Ordering::Acquire)
    }
}

impl std::task::Wake for Task {
    fn wake(self: Arc<Self>) {
        self.wake_count.fetch_add(1, Ordering::AcqRel);
    }
}

fn main() {
    divan::main()
}

#[divan::bench]
fn extract_wakers_never_linked(bencher: Bencher) {
    let mut wl = pin!(black_box(WakerList::new()));
    bencher.bench_local(|| {
        let round = wl.as_mut().begin_extraction();
        let mut wakers = ExtractedWakers::new();
        while wl.as_mut().extract_some_wakers(round, &mut wakers) {
            wakers.wake_all();
        }
    });
}

#[divan::bench]
fn extract_wakers_one_unlinked_slot(bencher: Bencher) {
    let mut wl = pin!(black_box(WakerList::new()));
    let mut slot = pin!(WakerSlot::new());
    let task = Task::new();
    wl.as_mut().link(slot.as_mut(), task.waker());
    wl.as_mut().unlink(slot);
    bencher.bench_local(|| {
        let round = wl.as_mut().begin_extraction();
        let mut wakers = ExtractedWakers::new();
        while wl.as_mut().extract_some_wakers(round, &mut wakers) {
            wakers.wake_all();
        }
    });
}

#[divan::bench]
fn extract_wakers_one_waker(bencher: Bencher) {
    let mut wl = pin!(black_box(WakerList::new()));
    let mut slot = pin!(WakerSlot::new());
    let task = Task::new();
    let waker = task.waker();
    bencher.bench_local(|| {
        wl.as_mut().link(slot.as_mut(), waker.clone());
        let round = wl.as_mut().begin_extraction();
        let mut wakers = ExtractedWakers::new();
        while wl.as_mut().extract_some_wakers(round, &mut wakers) {
            wakers.wake_all();
        }
    });
}
