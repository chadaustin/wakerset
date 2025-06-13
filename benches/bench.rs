use divan::Bencher;
use divan::black_box;
use std::sync::atomic::Ordering;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::task::Waker;
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
fn extract_some_wakers(bencher: Bencher) {
    let mut wakers = black_box(WakerList::new());
    let mut wakers = pin!(wakers);
    let slot = pin!(WakerSlot::new());
    let task = Task::new();
    wakers.as_mut().link(slot, task.waker());
    let mut wakers = black_box(wakers);
    bencher.bench_local(|| {
        let mut w = wakers.as_mut().extract_some_wakers();
        while w.wake_all() {
            w.extract_more(wakers.as_mut());
        }
    });
}
