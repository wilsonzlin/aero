use std::cell::{Cell, RefCell};
use std::panic;
use std::sync::Arc;
use std::sync::Mutex;

thread_local! {
    static LAST_PANIC_LOC: RefCell<Option<(String, u32)>> = const { RefCell::new(None) };
    static CAPTURE_PANIC_OUTPUT: Cell<bool> = const { Cell::new(false) };
}

// `std::panic::set_hook` installs a process-wide hook. Even though our CI/safe-run environment
// forces single-threaded test execution, other runners may execute tests concurrently. Guard
// against racy hook replacement by serializing access here.
static PANIC_HOOK_LOCK: Mutex<()> = Mutex::new(());

pub(crate) fn capture_panic_location(f: impl FnOnce()) -> (String, u32) {
    let _guard = PANIC_HOOK_LOCK.lock().expect("panic hook lock poisoned");
    LAST_PANIC_LOC.with(|cell| cell.borrow_mut().take());
    CAPTURE_PANIC_OUTPUT.with(|cell| cell.set(true));
    struct CaptureGuard;
    impl Drop for CaptureGuard {
        fn drop(&mut self) {
            CAPTURE_PANIC_OUTPUT.with(|cell| cell.set(false));
        }
    }
    let _capture_guard = CaptureGuard;

    let prev_hook = Arc::new(panic::take_hook());
    let prev_hook_for_other_panics = prev_hook.clone();
    panic::set_hook(Box::new(move |info| {
        let capture = CAPTURE_PANIC_OUTPUT.with(|cell| cell.get());
        if capture {
            if let Some(loc) = info.location() {
                LAST_PANIC_LOC.with(|cell| {
                    *cell.borrow_mut() = Some((loc.file().to_string(), loc.line()))
                });
            }
            // Suppress panic output for the expected panic we are catching in this thread.
            return;
        }
        // Preserve the previous panic hook output for other threads/tests while the global hook is
        // temporarily installed.
        (**prev_hook_for_other_panics)(info);
    }));

    let result = panic::catch_unwind(panic::AssertUnwindSafe(f));
    // Restore the original hook without races:
    // - `take_hook` swaps in the default hook and returns our temporary hook, so dropping it
    //   releases its `Arc` reference to the previous hook.
    // - After that, `prev_hook` should have unique ownership and can be unwrapped back into the
    //   original `Box<dyn Fn(..)>`.
    let tmp = panic::take_hook();
    drop(tmp);
    let prev = match Arc::try_unwrap(prev_hook) {
        Ok(prev) => prev,
        Err(_) => panic!("previous panic hook is still referenced"),
    };
    panic::set_hook(prev);

    assert!(result.is_err(), "expected a panic");
    LAST_PANIC_LOC.with(|cell| {
        cell.borrow()
            .clone()
            .expect("panic hook did not capture a location")
    })
}

