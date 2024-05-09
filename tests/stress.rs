use core::future::Future;
use core::pin::Pin;
use core::task::Context;
use core::task::Poll;
use pin_project::pin_project;
use pinned_mutex::std::PinnedMutex;
use std::sync::Arc;
use wakerset::WakerList;
use wakerset::WakerSlot;

const TC: usize = 16;
const ITER: u64 = if cfg!(miri) { 1000 } else { 1000000 };
const WAKER_TASKS: u64 = 1;
const YIELD_TASKS: u64 = 8;

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
        let result = if inner.count < inner.iter_limit {
            *inner.as_mut().project().count += 1;
            true
        } else {
            false
        };
        let wakers = inner.as_mut().project().waiters.extract_wakers();
        drop(inner);
        wakers.notify_all();
        result
    }

    fn block(self: &DS) -> impl Future<Output = bool> + '_ {
        let inner = self.0.as_ref().lock();
        let count = inner.count;

        Block {
            ds: self,
            count,
            waker: WakerSlot::new(),
        }
    }
}

#[pin_project]
struct Block<'a> {
    ds: &'a DS,
    count: u64,
    #[pin]
    waker: WakerSlot,
}

impl Future for Block<'_> {
    type Output = bool;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut inner = self.ds.0.as_ref().lock();
        if inner.count == inner.iter_limit {
            //eprintln!("done");
            Poll::Ready(false)
        } else if self.count == inner.count {
            //eprintln!("linking and waiting");
            inner
                .as_mut()
                .project()
                .waiters
                .link(self.project().waker, cx.waker().clone());
            Poll::Pending
        } else {
            //eprintln!("unblocking");
            Poll::Ready(true)
        }
    }
}

async fn wake_repeatedly(ds: DS) {
    while ds.wake_waiters() {
        //eprintln!("woke");
    }
}

async fn yield_repeatedly(ds: DS) {
    while ds.block().await {
        //eprintln!("blocked");
    }
}

#[test]
fn stress_test() -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(TC)
        .build()
        .unwrap();
    rt.block_on(async move {
        let mut jh = Vec::new();
        let ds = DS::new(ITER);

        for _ in 0..WAKER_TASKS {
            jh.push(tokio::spawn(wake_repeatedly(ds.clone())));
        }

        for _ in 0..YIELD_TASKS {
            jh.push(tokio::spawn(yield_repeatedly(ds.clone())));
        }

        drop(ds);
        for jh in jh {
            jh.await.unwrap();
        }
        Ok(())
    })
}
