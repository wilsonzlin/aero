use futures_intrusive::channel::shared::OneshotReceiver;

#[cfg(target_arch = "wasm32")]
fn noop_waker() -> std::task::Waker {
    use std::task::{RawWaker, RawWakerVTable, Waker};

    unsafe fn clone(_: *const ()) -> RawWaker {
        RawWaker::new(std::ptr::null(), &VTABLE)
    }
    unsafe fn wake(_: *const ()) {}
    unsafe fn wake_by_ref(_: *const ()) {}
    unsafe fn drop(_: *const ()) {}

    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop);
    unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) }
}

#[cfg(target_arch = "wasm32")]
async fn yield_now() {
    // Yield to the JS event loop so WebGPU/WebGL work can make progress while we poll.
    // A resolved promise schedules a microtask; that's sufficient to avoid blocking.
    let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(
        &wasm_bindgen::JsValue::UNDEFINED,
    ))
    .await;
}

/// Receive a `futures_intrusive` oneshot channel, ensuring `wgpu::Device::poll` is driven until the
/// sender fires.
///
/// On native targets we can simply block in `Maintain::Wait`. On wasm32, we must repeatedly call
/// `Maintain::Poll` while yielding to the JS event loop, otherwise `map_async` completions can stall
/// indefinitely.
pub(crate) async fn receive_oneshot_with_wgpu_poll<T>(
    device: &wgpu::Device,
    receiver: OneshotReceiver<T>,
) -> Option<T> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        device.poll(wgpu::Maintain::Wait);
        receiver.receive().await
    }

    #[cfg(target_arch = "wasm32")]
    {
        use std::future::Future;
        use std::pin::Pin;
        use std::task::{Context, Poll};

        let mut fut = std::pin::pin!(receiver.receive());
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        loop {
            device.poll(wgpu::Maintain::Poll);
            if let Poll::Ready(v) = Future::poll(Pin::as_mut(&mut fut), &mut cx) {
                return v;
            }
            yield_now().await;
        }
    }
}
