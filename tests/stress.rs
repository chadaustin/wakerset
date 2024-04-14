use core::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use wakerset::WakerList;
use wakerset::WakerSlot;

const TC: usize = 16;
const ITER: u64 = 10000;

#[derive(Default)]
struct Inner {
    iter_limit: u64,
    count: u64,
    waiters: WakerList,
}

struct DS(Mutex<Inner>);

impl DS {
    fn new(iter_limit: u64) -> DS {
        DS(Mutex::new(Inner {
            iter_limit,
            ..Default::default()
        }))
    }

    fn wake_waiters(self: Pin<&Self>) -> bool {
        let inner = self.0.lock().unwrap();
        if inner.count >= inner.iter_limit {
            return false;
        }
        let wakers = inner.waiters.extract_wakers();
        drop(inner);
        wakers.notify_all();
        true
    }
}

async fn wake_repeatedly(ds: Pin<Arc<DS>>) {
    while ds.as_ref().wake_waiters() {}
}

#[test]
fn stress_test() -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(TC)
        .build()
        .unwrap();
    rt.block_on(async move {
        let ds = Arc::pin(DS::new(ITER));
        let waker_task = tokio::spawn(wake_repeatedly(ds.clone()));
        drop(ds);

        waker_task.await.unwrap();
        Ok(())
    })
}
