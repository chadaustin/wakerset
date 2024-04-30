use core::pin::Pin;
use pin_project::pin_project;
use pinned_mutex::std::PinnedMutex;
use std::sync::Arc;
use wakerset::WakerList;
use wakerset::WakerSlot;

const TC: usize = 16;
const ITER: u64 = 1000000;

#[derive(Default)]
#[pin_project]
struct Inner {
    iter_limit: u64,
    count: u64,
    #[pin]
    waiters: WakerList,
}

#[derive(Clone)]
struct DS(Pin<Arc<PinnedMutex<Inner>>>);

impl DS {
    fn new(iter_limit: u64) -> DS {
        DS(Arc::pin(PinnedMutex::new(Inner {
            iter_limit,
            ..Default::default()
        })))
    }

    fn wake_waiters(self: &DS) -> bool {
        let mut inner = self.0.as_ref().lock();
        if inner.count >= inner.iter_limit {
            return false;
        }
        *inner.as_mut().project().count += 1;
        let wakers = inner.as_mut().project().waiters.extract_wakers();
        drop(inner);
        wakers.notify_all();
        true
    }
}

async fn wake_repeatedly(ds: DS) {
    while ds.wake_waiters() {}
}

#[test]
fn stress_test() -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(TC)
        .build()
        .unwrap();
    rt.block_on(async move {
        let ds = DS::new(ITER);
        let waker_task = tokio::spawn(wake_repeatedly(ds.clone()));
        drop(ds);

        waker_task.await.unwrap();
        Ok(())
    })
}
