use aero_usb::device::AttachedUsbDevice;
use aero_usb::ehci::EhciController;
use aero_usb::{UsbSpeed, UsbWebUsbPassthroughDevice};

/// Scan an attached device (and any nested hubs) and collect the topology paths of
/// [`UsbWebUsbPassthroughDevice`] instances.
fn collect_webusb_devices_in_attached(
    dev: &mut AttachedUsbDevice,
    path: &mut Vec<u8>,
    out: &mut Vec<(Vec<u8>, UsbWebUsbPassthroughDevice)>,
) {
    let model_any = dev.model() as &dyn core::any::Any;
    if let Some(webusb) = model_any.downcast_ref::<UsbWebUsbPassthroughDevice>() {
        out.push((path.clone(), webusb.clone()));
        return;
    }

    if let Some(hub) = dev.as_hub_mut() {
        for port_idx in 0..hub.num_ports() {
            if let Some(child) = hub.downstream_device_mut(port_idx) {
                // Hub port numbers are 1-based in topology paths.
                let port_num = (port_idx + 1).min(255) as u8;
                path.push(port_num);
                collect_webusb_devices_in_attached(child, path, out);
                path.pop();
            }
        }
    }
}

/// Collect every WebUSB passthrough device present in the EHCI controller's topology.
///
/// The returned paths follow the `aero_usb::ehci::hub::RootHub` convention:
/// - `path[0]` is the EHCI root port index (0-based).
/// - `path[1..]` are hub ports (1-based).
pub(crate) fn collect_ehci_webusb_devices(
    ctrl: &mut EhciController,
) -> Vec<(Vec<u8>, UsbWebUsbPassthroughDevice)> {
    let mut out = Vec::new();
    let hub = ctrl.hub_mut();
    for root_port in 0..hub.num_ports() {
        if let Some(mut dev) = hub.port_device_mut(root_port) {
            let mut path = vec![root_port as u8];
            collect_webusb_devices_in_attached(&mut dev, &mut path, &mut out);
        }
    }
    out
}

/// Enforce the WebUSB passthrough device connection state for an EHCI controller.
///
/// This helper is shared between the wasm-bindgen bridge and host-side unit tests so the logic
/// remains covered by `cargo test` even though the bridge itself is `wasm32`-only.
///
/// Behaviour:
/// - `connected=false`: detaches *only* [`UsbWebUsbPassthroughDevice`] instances (anywhere in the
///   topology) without disturbing unrelated devices such as the external hub on root port 0.
/// - `connected=true`: ensures *exactly one* WebUSB device exists and that it is attached at the
///   reserved root port (`crate::webusb_ports::WEBUSB_ROOT_PORT`).
pub(crate) fn set_ehci_webusb_connected(
    ctrl: &mut EhciController,
    webusb_handle: &mut Option<UsbWebUsbPassthroughDevice>,
    connected: bool,
) -> bool {
    let reserved_port = crate::webusb_ports::WEBUSB_ROOT_PORT;
    let reserved_path = [reserved_port];

    let found = collect_ehci_webusb_devices(ctrl);

    if !connected {
        // Best-effort: if the bridge lost its handle but the topology still contains a WebUSB
        // device, recover a clone so we can reset host-side state after detaching.
        if webusb_handle.is_none() {
            if let Some((_, handle)) = found.first() {
                *webusb_handle = Some(handle.clone());
            }
        }

        for (path, _) in found {
            let _ = ctrl.hub_mut().detach_at_path(&path);
        }

        if let Some(dev) = webusb_handle.as_ref() {
            // Preserve UHCI semantics: disconnect drops queued/in-flight host state, but keep the
            // handle alive so `UsbPassthroughDevice.next_id` remains monotonic across reconnects.
            dev.reset();
        }

        return false;
    }

    // If we already have a device at the reserved root port, prefer that handle.
    if let Some((_, handle)) = found
        .iter()
        .find(|(path, _)| path.as_slice() == reserved_path)
    {
        *webusb_handle = Some(handle.clone());
    } else if webusb_handle.is_none() {
        // Otherwise, recover any existing WebUSB handle so action IDs remain monotonic across a
        // "move" from a legacy port.
        if let Some((_, handle)) = found.first() {
            *webusb_handle = Some(handle.clone());
        }
    }

    // Detach any WebUSB passthrough device not already attached at the reserved root port.
    for (path, _) in &found {
        if path.as_slice() != reserved_path {
            let _ = ctrl.hub_mut().detach_at_path(path);
        }
    }

    // Fast path: device already attached at the reserved root port; avoid detach/attach churn so
    // the guest doesn't observe a spurious reconnect.
    if found.iter().any(|(path, _)| path.as_slice() == reserved_path) {
        if let Some(dev) = webusb_handle.as_ref() {
            dev.set_speed(UsbSpeed::High);
        }
        return true;
    }

    // Attach the shared handle at the reserved root port with replace semantics.
    let dev = webusb_handle.get_or_insert_with(|| UsbWebUsbPassthroughDevice::new_with_speed(UsbSpeed::High));
    dev.set_speed(UsbSpeed::High);
    let _ = ctrl.hub_mut().detach_at_path(&reserved_path);
    ctrl.hub_mut()
        .attach_at_path(&reserved_path, Box::new(dev.clone()))
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    use aero_usb::hub::UsbHubDevice;

    fn is_external_hub_attached(ctrl: &mut EhciController) -> bool {
        let Some(dev) = ctrl.hub_mut().port_device(0) else {
            return false;
        };
        let model_any = dev.model() as &dyn core::any::Any;
        model_any.downcast_ref::<UsbHubDevice>().is_some()
    }

    #[test]
    fn webusb_reserved_port_does_not_clobber_external_hub() {
        let mut ctrl = EhciController::new();
        ctrl.hub_mut()
            .attach_at_path(&[0], Box::new(UsbHubDevice::with_port_count(16)))
            .unwrap();
        assert!(is_external_hub_attached(&mut ctrl));

        let mut handle: Option<UsbWebUsbPassthroughDevice> = None;
        assert!(set_ehci_webusb_connected(&mut ctrl, &mut handle, true));
        assert!(is_external_hub_attached(&mut ctrl));
    }

    #[test]
    fn disconnect_scans_topology_without_detaching_unrelated_devices() {
        let mut ctrl = EhciController::new();
        ctrl.hub_mut()
            .attach_at_path(&[0], Box::new(UsbHubDevice::with_port_count(16)))
            .unwrap();
        assert!(is_external_hub_attached(&mut ctrl));

        // Attach WebUSB at a non-reserved port (simulating older snapshots or legacy host code).
        ctrl.hub_mut()
            .attach_at_path(
                &[2],
                Box::new(UsbWebUsbPassthroughDevice::new_with_speed(UsbSpeed::High)),
            )
            .unwrap();

        let mut handle: Option<UsbWebUsbPassthroughDevice> = None;
        assert!(!set_ehci_webusb_connected(&mut ctrl, &mut handle, false));

        assert!(is_external_hub_attached(&mut ctrl));
        assert!(collect_ehci_webusb_devices(&mut ctrl).is_empty());
    }

    #[test]
    fn connect_moves_legacy_webusb_device_to_reserved_root_port() {
        let mut ctrl = EhciController::new();
        ctrl.hub_mut()
            .attach_at_path(&[0], Box::new(UsbHubDevice::with_port_count(16)))
            .unwrap();
        assert!(is_external_hub_attached(&mut ctrl));

        // Attach WebUSB at a non-reserved port.
        ctrl.hub_mut()
            .attach_at_path(
                &[2],
                Box::new(UsbWebUsbPassthroughDevice::new_with_speed(UsbSpeed::High)),
            )
            .unwrap();

        let mut handle: Option<UsbWebUsbPassthroughDevice> = None;
        assert!(set_ehci_webusb_connected(&mut ctrl, &mut handle, true));

        assert!(is_external_hub_attached(&mut ctrl));

        let found = collect_ehci_webusb_devices(&mut ctrl);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, vec![crate::webusb_ports::WEBUSB_ROOT_PORT]);
    }
}

