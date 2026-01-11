#![forbid(unsafe_code)]

#[cfg(target_arch = "wasm32")]
mod wasm {
    use std::cell::RefCell;

    use aero_gpu::{AeroGpuCommandProcessor, AeroGpuEvent};
    use js_sys::{BigInt, Object, Reflect, Uint8Array};
    use wasm_bindgen::prelude::*;

    thread_local! {
        static PROCESSOR: RefCell<AeroGpuCommandProcessor> = RefCell::new(AeroGpuCommandProcessor::new());
    }

    #[wasm_bindgen]
    pub fn submit_aerogpu(
        cmd_stream: Uint8Array,
        signal_fence: u64,
        alloc_table: Option<Uint8Array>,
    ) -> Result<JsValue, JsValue> {
        // `alloc_table` is reserved for future guest-memory backing support.
        // Keep the parameter so the JS/IPC surface remains stable.
        drop(alloc_table);

        let mut bytes = vec![0u8; cmd_stream.length() as usize];
        cmd_stream.copy_to(&mut bytes);

        let present_count = PROCESSOR.with(|processor| {
            let mut processor = processor.borrow_mut();
            let events = processor
                .process_submission(&bytes, signal_fence)
                .map_err(|err| JsValue::from_str(&err.to_string()))?;

            let mut present_count = 0u64;
            for event in events {
                if matches!(event, AeroGpuEvent::PresentCompleted { .. }) {
                    present_count = present_count.saturating_add(1);
                }
            }

            Ok::<u64, JsValue>(present_count)
        })?;

        let out = Object::new();
        Reflect::set(
            &out,
            &JsValue::from_str("completedFence"),
            &BigInt::from(signal_fence).into(),
        )?;
        Reflect::set(
            &out,
            &JsValue::from_str("presentCount"),
            &BigInt::from(present_count).into(),
        )?;

        Ok(out.into())
    }
}

// Re-export wasm bindings so the crate's public surface is identical across
// `crate::` and `crate::wasm::` paths.
#[cfg(target_arch = "wasm32")]
pub use wasm::*;
