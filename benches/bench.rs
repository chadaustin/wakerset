use divan::black_box;
use divan::Bencher;
use std::pin::pin;
use wakerset::WakerList;

fn main() {
    divan::main()
}

#[divan::bench]
fn extract_some_wakers(bencher: Bencher) {
    let mut wakers = WakerList::new();
    let mut wakers = pin!(wakers);
    bencher.bench_local(|| black_box(wakers.as_mut().extract_some_wakers()));
}
