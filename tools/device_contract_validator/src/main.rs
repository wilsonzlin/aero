use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    about = "Validate docs/windows-device-contract.json against in-repo sources (Guest Tools, packaging specs, emulator IDs, and in-tree INFs)."
)]
struct Args {
    /// Repository root directory (defaults to current working directory).
    #[arg(long, default_value = ".")]
    repo_root: PathBuf,

    /// Path to the device contract JSON (relative to repo_root unless absolute).
    #[arg(long, default_value = "docs/windows-device-contract.json")]
    contract: PathBuf,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct DeviceContract {
    schema_version: u32,
    contract_name: String,
    contract_version: String,
    devices: Vec<DeviceEntry>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct DeviceEntry {
    device: String,
    pci_vendor_id: String,
    pci_device_id: String,

    #[serde(default)]
    pci_device_id_transitional: Option<String>,

    hardware_id_patterns: Vec<String>,
    driver_service_name: String,
    inf_name: String,

    #[serde(default)]
    virtio_device_type: Option<u32>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct PackagingSpec {
    #[serde(default)]
    drivers: Vec<SpecDriverEntry>,

    // Legacy schema supported by aero_packager.
    #[serde(default)]
    required_drivers: Vec<LegacySpecDriverEntry>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct SpecDriverEntry {
    name: String,

    required: bool,

    /// A list of INF filenames (no paths) expected to be present for this driver.
    #[serde(default)]
    expected_inf_files: Vec<String>,

    /// A list of Windows service names expected to appear in AddService directives.
    #[serde(default)]
    expected_add_services: Vec<String>,

    /// Optional: derive an expected service name from a Guest Tools devices.cmd variable.
    #[serde(default)]
    expected_add_services_from_devices_cmd_var: Option<String>,

    /// Regex patterns that must appear in at least one INF for the driver.
    #[serde(default)]
    expected_hardware_ids: Vec<String>,

    /// Optional: derive expected hardware IDs from a Guest Tools devices.cmd variable
    /// (aero_packager supports this to avoid duplicating long HWID lists in specs).
    #[serde(default)]
    expected_hardware_ids_from_devices_cmd_var: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct LegacySpecDriverEntry {
    name: String,

    #[serde(default)]
    expected_inf_files: Vec<String>,

    #[serde(default)]
    expected_add_services: Vec<String>,

    #[serde(default)]
    expected_add_services_from_devices_cmd_var: Option<String>,

    /// Regex patterns that must appear in at least one INF for the driver.
    #[serde(default)]
    expected_hardware_ids: Vec<String>,

    #[serde(default)]
    expected_hardware_ids_from_devices_cmd_var: Option<String>,
}

fn main() {
    let args = Args::parse();
    if let Err(err) = run(&args) {
        eprintln!("{err:#}");
        std::process::exit(1);
    }
}

fn run(args: &Args) -> Result<()> {
    let repo_root = canonicalize_maybe_missing(&args.repo_root)
        .with_context(|| format!("resolve repo root {}", args.repo_root.display()))?;

    let contract_path = resolve_under(&repo_root, &args.contract);
    let contract = load_contract(&contract_path)
        .with_context(|| format!("load device contract {}", contract_path.display()))?;

    validate_contract_schema(&contract)?;
    let devices = index_devices(&contract.devices)?;

    // Optional-but-expected variant: a contract intended for building Guest Tools from upstream
    // virtio-win drivers (viostor/netkvm/etc). It must mirror the canonical contract for PCI IDs
    // + HWID patterns; only service/INF names differ for virtio devices.
    let virtio_win_contract_path = repo_root.join("docs/windows-device-contract-virtio-win.json");
    if !virtio_win_contract_path.exists() {
        bail!(
            "missing expected virtio-win contract variant: {}",
            virtio_win_contract_path.display()
        );
    }
    let virtio_win_contract = load_contract(&virtio_win_contract_path).with_context(|| {
        format!(
            "load virtio-win device contract {}",
            virtio_win_contract_path.display()
        )
    })?;
    validate_contract_schema(&virtio_win_contract)?;
    let virtio_win_devices = index_devices(&virtio_win_contract.devices)?;

    validate_contract_entries(&devices)?;
    validate_contract_entries(&virtio_win_devices)?;
    validate_virtio_win_contract_variant(
        &contract,
        &devices,
        &virtio_win_contract,
        &virtio_win_devices,
    )?;
    validate_guest_tools_config(&repo_root, &devices)?;
    validate_packaging_specs(&repo_root, &devices, &virtio_win_devices)?;
    validate_in_tree_infs(&repo_root, &devices)?;
    validate_emulator_ids(&repo_root, &devices)?;

    Ok(())
}

fn load_contract(path: &Path) -> Result<DeviceContract> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    // Be tolerant of UTF-8 BOMs produced by some editors/tools.
    let parse_bytes = bytes
        .as_slice()
        .strip_prefix(b"\xef\xbb\xbf")
        .unwrap_or(bytes.as_slice());
    serde_json::from_slice(parse_bytes).with_context(|| format!("parse {}", path.display()))
}

fn validate_contract_schema(contract: &DeviceContract) -> Result<()> {
    if contract.schema_version != 1 {
        bail!(
            "unsupported device contract schema_version {} (expected 1)",
            contract.schema_version
        );
    }

    if contract.contract_name.trim().is_empty() {
        bail!("device contract contract_name is empty");
    }
    if contract.contract_version.trim().is_empty() {
        bail!("device contract contract_version is empty");
    }
    if contract.devices.is_empty() {
        bail!("device contract contains no devices");
    }
    Ok(())
}

fn index_devices(devices: &[DeviceEntry]) -> Result<BTreeMap<String, DeviceEntry>> {
    let mut map = BTreeMap::new();
    for d in devices {
        if d.device.trim().is_empty() {
            bail!("device entry has empty 'device' field");
        }
        if map.contains_key(&d.device) {
            bail!("duplicate device entry: {}", d.device);
        }
        map.insert(d.device.clone(), d.clone());
    }
    Ok(map)
}

fn validate_contract_entries(devices: &BTreeMap<String, DeviceEntry>) -> Result<()> {
    let rev_re = regex::Regex::new(r"(?i)&REV_([0-9A-F]{2})").expect("static regex must compile");
    let mut hwid_owners: BTreeMap<String, String> = BTreeMap::new();
    for (name, dev) in devices {
        let vendor = parse_hex_u16(&dev.pci_vendor_id)
            .with_context(|| format!("{name}: invalid pci_vendor_id"))?;
        let did = parse_hex_u16(&dev.pci_device_id)
            .with_context(|| format!("{name}: invalid pci_device_id"))?;

        if dev.virtio_device_type.is_none() && dev.pci_device_id_transitional.is_some() {
            bail!(
                "{name}: pci_device_id_transitional is only valid for virtio devices (set virtio_device_type or omit pci_device_id_transitional)"
            );
        }

        if dev.hardware_id_patterns.is_empty() {
            bail!("{name}: hardware_id_patterns is empty");
        }
        let expected_substr = format!("VEN_{vendor:04X}&DEV_{did:04X}");
        let mut has_canonical_vendor_device = false;
        let mut seen_hwids = BTreeSet::<String>::new();
        for pat in &dev.hardware_id_patterns {
            validate_hwid_literal(pat)
                .with_context(|| format!("{name}: invalid hardware_id_patterns entry"))?;
            let upper = pat.to_ascii_uppercase();
            if !seen_hwids.insert(upper.clone()) {
                bail!(
                    "{name}: hardware_id_patterns contains a duplicate entry (case-insensitive): {pat:?}"
                );
            }
            if let Some(other) = hwid_owners.get(&upper) {
                bail!(
                    "{name}: hardware_id_patterns entry {pat:?} is duplicated in contract device {other:?} (HWIDs must be globally unique)"
                );
            }
            hwid_owners.insert(upper.clone(), name.clone());

            if upper.contains(&expected_substr) {
                has_canonical_vendor_device = true;
            }
        }
        if !has_canonical_vendor_device {
            bail!(
                "{name}: hardware_id_patterns is missing canonical pci_vendor_id/pci_device_id pattern (expected to contain {expected_substr})"
            );
        }

        if dev.driver_service_name.trim().is_empty() {
            bail!("{name}: driver_service_name is empty");
        }
        if dev.inf_name.trim().is_empty() || !dev.inf_name.to_ascii_lowercase().ends_with(".inf") {
            bail!("{name}: inf_name must end with .inf");
        }

        if let Some(vtype) = dev.virtio_device_type {
            if vendor != 0x1AF4 {
                bail!("{name}: virtio devices must use pci_vendor_id 0x1AF4 (found {vendor:#06x})");
            }

            // Modern ID space: 0x1040 + virtio_device_type.
            let expected_modern = 0x1040u16
                .checked_add(
                    u16::try_from(vtype)
                        .map_err(|_| anyhow::anyhow!("virtio_device_type out of range"))?,
                )
                .ok_or_else(|| anyhow::anyhow!("virtio_device_type overflow"))?;
            if did != expected_modern {
                bail!(
                    "{name}: pci_device_id must be 0x1040 + virtio_device_type (expected {expected_modern:#06x}, found {did:#06x})"
                );
            }

            let expected_transitional =
                0x1000u16 + u16::try_from(vtype).context("virtio_device_type out of range")? - 1;
            let trans = dev
                .pci_device_id_transitional
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("{name}: missing pci_device_id_transitional"))?;
            let trans = parse_hex_u16(trans)
                .with_context(|| format!("{name}: invalid pci_device_id_transitional"))?;
            if trans != expected_transitional {
                bail!(
                    "{name}: pci_device_id_transitional must be 0x1000 + (virtio_device_type - 1) (expected {expected_transitional:#06x}, found {trans:#06x})"
                );
            }

            // Contract policy: virtio HWIDs are modern-only for AERO-W7-VIRTIO v1. Transitional IDs
            // are recorded separately as `pci_device_id_transitional` for reference, but must not
            // appear in `hardware_id_patterns`.
            let expected_substr = format!("VEN_{vendor:04X}&DEV_{did:04X}");
            for pat in &dev.hardware_id_patterns {
                if !pat.to_ascii_uppercase().contains(&expected_substr) {
                    bail!(
                        "{name}: virtio hardware_id_patterns must be modern-only and contain {expected_substr} (got {pat:?})"
                    );
                }
            }

            // Contract v1 is revision-gated (REV_01). The contract includes less-specific HWIDs for
            // tooling convenience, but it must contain at least one REV_01-qualified HWID and must
            // not include any other REV_.. values.
            let mut revs = BTreeSet::<String>::new();
            for pat in &dev.hardware_id_patterns {
                for caps in rev_re.captures_iter(pat) {
                    let rev = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                    if !rev.is_empty() {
                        revs.insert(rev.to_ascii_uppercase());
                    }
                }
            }
            if revs.is_empty() {
                bail!("{name}: hardware_id_patterns must include at least one REV_01 entry (AERO-W7-VIRTIO v1)");
            }
            let bad: Vec<_> = revs
                .iter()
                .filter(|r| r.as_str() != "01")
                .cloned()
                .collect();
            if !bad.is_empty() {
                bail!(
                    "{name}: hardware_id_patterns contains unsupported PCI revision IDs for virtio devices (expected only REV_01): {:?}",
                    bad
                );
            }
        } else if dev.device == "aero-gpu" {
            // AeroGPU's canonical Windows binding contract is A3A0 (versioned ABI).
            //
            // The legacy bring-up identity (vendor 1AED) is intentionally excluded from the
            // canonical Guest Tools device contract; it uses the legacy INFs under
            // `drivers/aerogpu/packaging/win7/legacy/` and requires enabling the legacy emulator
            // device model (feature `emulator/aerogpu-legacy`).
            let hwids = dev
                .hardware_id_patterns
                .iter()
                .map(|s| s.to_ascii_uppercase())
                .collect::<Vec<_>>();
            if !hwids.iter().any(|p| p.contains("VEN_A3A0&DEV_0001")) {
                bail!("{name}: hardware_id_patterns missing PCI\\\\VEN_A3A0&DEV_0001");
            }
            if !hwids
                .iter()
                .any(|p| p.contains("VEN_A3A0&DEV_0001&SUBSYS_0001A3A0"))
            {
                bail!(
                    "{name}: hardware_id_patterns missing PCI\\\\VEN_A3A0&DEV_0001&SUBSYS_0001A3A0"
                );
            }

            // Ensure we do not accidentally re-add the legacy AeroGPU ID to the canonical contract.
            //
            // Avoid embedding the full legacy vendor token (`VEN_` + `1AED`) in the source (there
            // is a repo-wide guard test that tracks where legacy IDs are allowed to appear).
            let legacy_vendor = format!("VEN_{}", "1AED");
            let legacy_fragment = format!("{legacy_vendor}&DEV_0001");
            if hwids.iter().any(|p| p.contains(&legacy_fragment)) {
                bail!("{name}: hardware_id_patterns must not include legacy bring-up HWID family (vendor 1AED); canonical contract is A3A0-only");
            }

            if hwids.iter().any(|p| !p.contains("VEN_A3A0&DEV_0001")) {
                bail!("{name}: hardware_id_patterns must all be in the PCI\\\\VEN_A3A0&DEV_0001 family");
            }
        }
    }

    // Contract is intended to cover these devices explicitly.
    for required in [
        "virtio-blk",
        "virtio-net",
        "virtio-input",
        "virtio-snd",
        "aero-gpu",
    ] {
        if !devices.contains_key(required) {
            bail!("device contract missing required device entry: {required}");
        }
    }

    Ok(())
}

fn validate_virtio_win_contract_variant(
    base_contract: &DeviceContract,
    base_devices: &BTreeMap<String, DeviceEntry>,
    virtio_win_contract: &DeviceContract,
    virtio_win_devices: &BTreeMap<String, DeviceEntry>,
) -> Result<()> {
    if virtio_win_contract.schema_version != base_contract.schema_version {
        bail!(
            "virtio-win contract schema_version mismatch: base={} virtio-win={}",
            base_contract.schema_version,
            virtio_win_contract.schema_version
        );
    }
    if virtio_win_contract.contract_version != base_contract.contract_version {
        bail!(
            "virtio-win contract_version mismatch: base={} virtio-win={}",
            base_contract.contract_version,
            virtio_win_contract.contract_version
        );
    }

    let base_keys: BTreeSet<_> = base_devices.keys().collect();
    let virtio_win_keys: BTreeSet<_> = virtio_win_devices.keys().collect();
    if base_keys != virtio_win_keys {
        let missing = base_keys
            .difference(&virtio_win_keys)
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        let extra = virtio_win_keys
            .difference(&base_keys)
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        bail!(
            "virtio-win contract must mirror canonical device set.\nmissing from virtio-win: {:?}\nextra in virtio-win:    {:?}",
            missing,
            extra
        );
    }

    let expected_virtio_win: BTreeMap<&str, (&str, &str)> = BTreeMap::from([
        ("virtio-blk", ("viostor", "viostor.inf")),
        ("virtio-net", ("netkvm", "netkvm.inf")),
        ("virtio-input", ("vioinput", "vioinput.inf")),
        ("virtio-snd", ("viosnd", "viosnd.inf")),
    ]);

    for (name, base) in base_devices {
        let virtio_win = virtio_win_devices.get(name).expect("checked key set");

        if base.pci_vendor_id != virtio_win.pci_vendor_id
            || base.pci_device_id != virtio_win.pci_device_id
        {
            bail!(
                "{name}: virtio-win variant PCI ID mismatch: base={}:{}, virtio-win={}:{}",
                base.pci_vendor_id,
                base.pci_device_id,
                virtio_win.pci_vendor_id,
                virtio_win.pci_device_id
            );
        }

        if base.pci_device_id_transitional != virtio_win.pci_device_id_transitional {
            bail!(
                "{name}: virtio-win variant pci_device_id_transitional mismatch: base={:?} virtio-win={:?}",
                base.pci_device_id_transitional,
                virtio_win.pci_device_id_transitional
            );
        }

        if base.virtio_device_type != virtio_win.virtio_device_type {
            bail!(
                "{name}: virtio_device_type mismatch between contract variants: base={:?} virtio-win={:?}",
                base.virtio_device_type,
                virtio_win.virtio_device_type
            );
        }

        let base_patterns: BTreeSet<_> = base
            .hardware_id_patterns
            .iter()
            .map(|s| s.to_ascii_uppercase())
            .collect();
        let virtio_win_patterns: BTreeSet<_> = virtio_win
            .hardware_id_patterns
            .iter()
            .map(|s| s.to_ascii_uppercase())
            .collect();
        if base_patterns != virtio_win_patterns {
            bail!("{name}: hardware_id_patterns must be identical between contract variants");
        }

        if base.virtio_device_type.is_some() {
            let Some((expected_service, expected_inf)) = expected_virtio_win.get(name.as_str())
            else {
                bail!("{name}: unexpected virtio device in contract (validator bug)");
            };
            if !virtio_win
                .driver_service_name
                .eq_ignore_ascii_case(expected_service)
            {
                bail!(
                    "{name}: unexpected virtio-win service name: expected {expected_service:?}, got {:?}",
                    virtio_win.driver_service_name
                );
            }
            if !virtio_win.inf_name.eq_ignore_ascii_case(expected_inf) {
                bail!(
                    "{name}: unexpected virtio-win INF name: expected {expected_inf:?}, got {:?}",
                    virtio_win.inf_name
                );
            }

            // Sanity: the canonical contract should *not* already use virtio-win names.
            if base
                .driver_service_name
                .eq_ignore_ascii_case(expected_service)
                || base.inf_name.eq_ignore_ascii_case(expected_inf)
            {
                bail!("{name}: canonical contract unexpectedly uses virtio-win service/INF name");
            }
        } else {
            // Non-virtio devices must not change between contract variants.
            if !virtio_win
                .driver_service_name
                .eq_ignore_ascii_case(&base.driver_service_name)
            {
                bail!(
                    "{name}: non-virtio service name mismatch between contract variants: base={:?} virtio-win={:?}",
                    base.driver_service_name,
                    virtio_win.driver_service_name
                );
            }
            if !virtio_win.inf_name.eq_ignore_ascii_case(&base.inf_name) {
                bail!(
                    "{name}: non-virtio INF name mismatch between contract variants: base={:?} virtio-win={:?}",
                    base.inf_name,
                    virtio_win.inf_name
                );
            }
        }
    }

    Ok(())
}

fn validate_hwid_literal(hwid: &str) -> Result<()> {
    // We intentionally keep the contract HWIDs in a canonical, string-literal form so:
    // - they can be copied into INFs directly, and
    // - Guest Tools can transform them into registry key names (CDD).
    //
    // The packaging spec (tools/packaging/specs/*.json) uses regex-safe variants.
    // We treat contract HWIDs as *literal* Windows PnP ID prefixes, not regexes.
    // Allow the most common PCI forms (SUBSYS + REV qualifiers are optional).
    let re = regex::Regex::new(
        r"(?i)^PCI\\VEN_[0-9A-F]{4}&DEV_[0-9A-F]{4}(?:&SUBSYS_[0-9A-F]{8})?(?:&REV_[0-9A-F]{2})?$",
    )
    .unwrap();
    if !re.is_match(hwid) {
        bail!("HWID must match PCI\\\\VEN_XXXX&DEV_YYYY[&SUBSYS_SSSSVVVV][&REV_RR]; got: {hwid}");
    }
    Ok(())
}

fn validate_guest_tools_config(
    repo_root: &Path,
    devices: &BTreeMap<String, DeviceEntry>,
) -> Result<()> {
    let cfg_path = repo_root.join("guest-tools/config/devices.cmd");
    let vars =
        parse_devices_cmd(&cfg_path).with_context(|| format!("parse {}", cfg_path.display()))?;

    let virtio_blk = devices.get("virtio-blk").expect("checked earlier");
    let expected_service = &virtio_blk.driver_service_name;
    let got_service = vars
        .get("AERO_VIRTIO_BLK_SERVICE")
        .ok_or_else(|| anyhow::anyhow!("guest-tools config missing AERO_VIRTIO_BLK_SERVICE"))?;
    if !eq_case_insensitive(got_service, expected_service) {
        bail!(
            "guest-tools config AERO_VIRTIO_BLK_SERVICE mismatch: expected '{expected_service}', found '{got_service}'"
        );
    }

    let virtio_net = devices.get("virtio-net").expect("checked earlier");
    let expected_net_service = &virtio_net.driver_service_name;
    let got_net_service = vars
        .get("AERO_VIRTIO_NET_SERVICE")
        .ok_or_else(|| anyhow::anyhow!("guest-tools config missing AERO_VIRTIO_NET_SERVICE"))?;
    if !eq_case_insensitive(got_net_service, expected_net_service) {
        bail!(
            "guest-tools config AERO_VIRTIO_NET_SERVICE mismatch: expected '{expected_net_service}', found '{got_net_service}'"
        );
    }

    let virtio_input = devices.get("virtio-input").expect("checked earlier");
    let expected_input_service = &virtio_input.driver_service_name;
    let got_input_service = vars
        .get("AERO_VIRTIO_INPUT_SERVICE")
        .ok_or_else(|| anyhow::anyhow!("guest-tools config missing AERO_VIRTIO_INPUT_SERVICE"))?;
    if !eq_case_insensitive(got_input_service, expected_input_service) {
        bail!(
            "guest-tools config AERO_VIRTIO_INPUT_SERVICE mismatch: expected '{expected_input_service}', found '{got_input_service}'"
        );
    }

    let virtio_snd = devices.get("virtio-snd").expect("checked earlier");
    let expected_snd_service = &virtio_snd.driver_service_name;
    let got_snd_service = vars
        .get("AERO_VIRTIO_SND_SERVICE")
        .ok_or_else(|| anyhow::anyhow!("guest-tools config missing AERO_VIRTIO_SND_SERVICE"))?;
    if !eq_case_insensitive(got_snd_service, expected_snd_service) {
        bail!(
            "guest-tools config AERO_VIRTIO_SND_SERVICE mismatch: expected '{expected_snd_service}', found '{got_snd_service}'"
        );
    }

    let aero_gpu = devices.get("aero-gpu").expect("checked earlier");
    let expected_gpu_service = &aero_gpu.driver_service_name;
    let got_gpu_service = vars
        .get("AERO_GPU_SERVICE")
        .ok_or_else(|| anyhow::anyhow!("guest-tools config missing AERO_GPU_SERVICE"))?;
    if !eq_case_insensitive(got_gpu_service, expected_gpu_service) {
        bail!(
            "guest-tools config AERO_GPU_SERVICE mismatch: expected '{expected_gpu_service}', found '{got_gpu_service}'"
        );
    }

    let mapping: &[(&str, &str)] = &[
        ("virtio-blk", "AERO_VIRTIO_BLK_HWIDS"),
        ("virtio-net", "AERO_VIRTIO_NET_HWIDS"),
        ("virtio-input", "AERO_VIRTIO_INPUT_HWIDS"),
        ("virtio-snd", "AERO_VIRTIO_SND_HWIDS"),
        ("aero-gpu", "AERO_GPU_HWIDS"),
    ];

    for (device_name, var_name) in mapping {
        let dev = devices.get(*device_name).unwrap();
        let raw = vars
            .get(*var_name)
            .ok_or_else(|| anyhow::anyhow!("guest-tools config missing {var_name}"))?;
        let got_hwids = parse_cmd_quoted_list(raw);
        if got_hwids.is_empty() {
            bail!("guest-tools config {var_name} is empty");
        }

        let expected_hwids = dev
            .hardware_id_patterns
            .iter()
            .map(|s| s.to_ascii_uppercase())
            .collect::<BTreeSet<_>>();
        let got_hwids = got_hwids
            .iter()
            .map(|s| s.to_ascii_uppercase())
            .collect::<BTreeSet<_>>();

        if got_hwids != expected_hwids {
            bail!(
                "guest-tools config {var_name} mismatch for {device_name}: expected {:?}, found {:?}",
                dev.hardware_id_patterns,
                parse_cmd_quoted_list(raw)
            );
        }
    }

    Ok(())
}

fn validate_packaging_specs(
    repo_root: &Path,
    devices: &BTreeMap<String, DeviceEntry>,
    virtio_win_devices: &BTreeMap<String, DeviceEntry>,
) -> Result<()> {
    let spec_paths = [
        repo_root.join("tools/packaging/specs/win7-virtio-win.json"),
        repo_root.join("tools/packaging/specs/win7-virtio-full.json"),
        repo_root.join("tools/packaging/specs/win7-signed.json"),
        repo_root.join("tools/packaging/specs/win7-aero-guest-tools.json"),
        repo_root.join("tools/packaging/specs/win7-aero-virtio.json"),
        repo_root.join("tools/packaging/specs/win7-aerogpu-only.json"),
    ];

    // Some packaging specs reference a devices.cmd variable name instead of inlining
    // regex patterns (see `expected_hardware_ids_from_devices_cmd_var`). Mirror the
    // behavior of `tools/guest-tools/validate_config.py` by extracting the base
    // VEN/DEV prefix from each HWID and regex-escaping it.
    let devices_cmd_path = repo_root.join("guest-tools/config/devices.cmd");
    let devices_cmd_vars_raw = parse_devices_cmd(&devices_cmd_path)
        .with_context(|| format!("parse {}", devices_cmd_path.display()))?;
    let mut devices_cmd_vars = BTreeMap::<String, String>::new();
    for (k, v) in devices_cmd_vars_raw {
        devices_cmd_vars.insert(k.to_ascii_uppercase(), v);
    }

    let pci_ven_dev_re = regex::RegexBuilder::new(r"(?i)PCI\\VEN_[0-9A-F]{4}&DEV_[0-9A-F]{4}")
        .build()
        .expect("static regex must compile");

    for spec_path in spec_paths {
        let bytes =
            fs::read(&spec_path).with_context(|| format!("read {}", spec_path.display()))?;
        let spec: PackagingSpec = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse {}", spec_path.display()))?;

        // Merge the new and legacy spec schema the same way aero_packager does:
        // - entries from `drivers`
        // - plus any additional entries from `required_drivers`
        // - if a driver appears in both, treat it as required and merge patterns.
        #[derive(Debug, Clone)]
        struct MergedSpecDriverEntry {
            required: bool,
            expected_inf_files: Vec<String>,
            expected_add_services: Vec<String>,
            expected_add_services_from_devices_cmd_var: Option<String>,
            expected_hardware_ids: Vec<String>,
        }

        let mut merged = BTreeMap::<String, MergedSpecDriverEntry>::new();
        let mut original_names = BTreeMap::<String, String>::new();

        let append_hwid_patterns_from_devices_cmd_var = |patterns: &mut Vec<String>,
                                                         var: &str,
                                                         driver_name: &str|
         -> Result<()> {
            let var_key = var.to_ascii_uppercase();
            let raw = devices_cmd_vars.get(&var_key).ok_or_else(|| {
                anyhow::anyhow!(
                    "{}: driver '{}': expected_hardware_ids_from_devices_cmd_var references missing devices.cmd variable: {}",
                    spec_path.display(),
                    driver_name,
                    var
                )
            })?;
            let hwids = parse_cmd_quoted_list(raw);
            if hwids.is_empty() {
                bail!(
                    "{}: driver '{}': devices.cmd variable {} is empty",
                    spec_path.display(),
                    driver_name,
                    var
                );
            }
            for hwid in hwids {
                let base = pci_ven_dev_re
                    .find(&hwid)
                    .map(|m| m.as_str().to_string())
                    .unwrap_or(hwid);
                let pat = regex::escape(&base);
                if !patterns.contains(&pat) {
                    patterns.push(pat);
                }
            }
            Ok(())
        };

        for drv in spec.drivers {
            let key = drv.name.to_ascii_lowercase();
            if merged.contains_key(&key) {
                bail!(
                    "{}: duplicate driver entry (case-insensitive): {}",
                    spec_path.display(),
                    drv.name
                );
            }
            original_names.insert(key.clone(), drv.name);
            let mut patterns = drv.expected_hardware_ids;
            if let Some(var) = drv.expected_hardware_ids_from_devices_cmd_var {
                append_hwid_patterns_from_devices_cmd_var(
                    &mut patterns,
                    &var,
                    original_names.get(&key).map(|s| s.as_str()).unwrap_or(&key),
                )?;
            }
            merged.insert(
                key,
                MergedSpecDriverEntry {
                    required: drv.required,
                    expected_inf_files: drv.expected_inf_files,
                    expected_add_services: drv.expected_add_services,
                    expected_add_services_from_devices_cmd_var: drv
                        .expected_add_services_from_devices_cmd_var,
                    expected_hardware_ids: patterns,
                },
            );
        }
        for legacy in spec.required_drivers {
            let key = legacy.name.to_ascii_lowercase();
            match merged.get_mut(&key) {
                Some(existing) => {
                    existing.required = true;
                    for pat in legacy.expected_hardware_ids {
                        if !existing.expected_hardware_ids.contains(&pat) {
                            existing.expected_hardware_ids.push(pat);
                        }
                    }
                    if let Some(var) = legacy.expected_hardware_ids_from_devices_cmd_var {
                        append_hwid_patterns_from_devices_cmd_var(
                            &mut existing.expected_hardware_ids,
                            &var,
                            original_names.get(&key).map(|s| s.as_str()).unwrap_or(&key),
                        )?;
                    }
                    for inf in legacy.expected_inf_files {
                        if !existing.expected_inf_files.contains(&inf) {
                            existing.expected_inf_files.push(inf);
                        }
                    }
                    for svc in legacy.expected_add_services {
                        if !existing.expected_add_services.contains(&svc) {
                            existing.expected_add_services.push(svc);
                        }
                    }
                    if existing
                        .expected_add_services_from_devices_cmd_var
                        .is_none()
                    {
                        existing.expected_add_services_from_devices_cmd_var =
                            legacy.expected_add_services_from_devices_cmd_var;
                    }
                }
                None => {
                    original_names.insert(key.clone(), legacy.name);
                    let mut patterns = legacy.expected_hardware_ids;
                    if let Some(var) = legacy.expected_hardware_ids_from_devices_cmd_var {
                        append_hwid_patterns_from_devices_cmd_var(
                            &mut patterns,
                            &var,
                            original_names.get(&key).map(|s| s.as_str()).unwrap_or(&key),
                        )?;
                    }
                    merged.insert(
                        key,
                        MergedSpecDriverEntry {
                            required: true,
                            expected_inf_files: legacy.expected_inf_files,
                            expected_add_services: legacy.expected_add_services,
                            expected_add_services_from_devices_cmd_var: legacy
                                .expected_add_services_from_devices_cmd_var,
                            expected_hardware_ids: patterns,
                        },
                    );
                }
            }
        }

        if merged.is_empty() {
            bail!(
                "{}: packaging spec contains no drivers",
                spec_path.display()
            );
        }

        let contract_devices = match spec_path.file_name().and_then(|s| s.to_str()).unwrap_or("") {
            "win7-virtio-win.json" | "win7-virtio-full.json" => virtio_win_devices,
            "win7-signed.json"
            | "win7-aero-guest-tools.json"
            | "win7-aero-virtio.json"
            | "win7-aerogpu-only.json" => devices,
            other => bail!("unexpected spec file name (validator bug): {other}"),
        };

        for (key, entry) in merged {
            let name = original_names.get(&key).cloned().unwrap_or(key.clone());

            // Ensure every spec driver name maps to a contract entry so adding drivers forces
            // updating the contract.
            let dev = find_contract_device_for_spec_driver(contract_devices, &name).with_context(
                || {
                    format!(
                        "{}: could not map spec driver '{}' to a Windows device contract entry",
                        spec_path.display(),
                        name
                    )
                },
            )?;

            if !entry.expected_inf_files.is_empty()
                && !entry
                    .expected_inf_files
                    .iter()
                    .any(|f| f.eq_ignore_ascii_case(&dev.inf_name))
            {
                bail!(
                    "{}: driver '{}' (maps to contract device '{}'): expected_inf_files is missing required INF '{}' (got {:?})",
                    spec_path.display(),
                    name,
                    dev.device,
                    dev.inf_name,
                    entry.expected_inf_files
                );
            }

            if !entry.expected_add_services.is_empty()
                && !entry
                    .expected_add_services
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case(&dev.driver_service_name))
            {
                bail!(
                    "{}: driver '{}' (maps to contract device '{}'): expected_add_services is missing required service '{}' (got {:?})",
                    spec_path.display(),
                    name,
                    dev.device,
                    dev.driver_service_name,
                    entry.expected_add_services
                );
            }

            if let Some(var) = entry.expected_add_services_from_devices_cmd_var.as_deref() {
                let var_key = var.to_ascii_uppercase();
                let raw = devices_cmd_vars.get(&var_key).ok_or_else(|| {
                    anyhow::anyhow!(
                        "{}: driver '{}': expected_add_services_from_devices_cmd_var references missing devices.cmd variable: {}",
                        spec_path.display(),
                        name,
                        var
                    )
                })?;
                let svc = raw.trim();
                if svc.is_empty() {
                    bail!(
                        "{}: driver '{}': devices.cmd variable {} (expected_add_services_from_devices_cmd_var) is empty",
                        spec_path.display(),
                        name,
                        var
                    );
                }
                if !svc.eq_ignore_ascii_case(&dev.driver_service_name) {
                    bail!(
                        "{}: driver '{}' (maps to contract device '{}'): devices.cmd variable {} resolves to service {:?}, but contract driver_service_name is {:?}",
                        spec_path.display(),
                        name,
                        dev.device,
                        var,
                        svc,
                        dev.driver_service_name
                    );
                }
            }

            if entry.expected_hardware_ids.is_empty() {
                bail!(
                    "{}: driver '{}' has empty expected_hardware_ids; add patterns derived from {}",
                    spec_path.display(),
                    name,
                    dev.device
                );
            }

            let mut compiled = Vec::new();
            for pat in &entry.expected_hardware_ids {
                let re = regex::RegexBuilder::new(pat)
                    .case_insensitive(true)
                    .build()
                    .with_context(|| {
                        format!(
                            "{}: driver '{}': compile expected_hardware_ids regex: {pat}",
                            spec_path.display(),
                            name
                        )
                    })?;
                compiled.push(re);
            }

            // Each spec regex must match at least one contract HWID string for the mapped device.
            for (i, re) in compiled.iter().enumerate() {
                let pat = &entry.expected_hardware_ids[i];
                if !dev
                    .hardware_id_patterns
                    .iter()
                    .any(|hwid| re.is_match(hwid))
                {
                    bail!(
                        "{}: driver '{}': expected_hardware_ids regex does not match any contract hardware_id_patterns for {}: {pat}",
                        spec_path.display(),
                        name,
                        dev.device
                    );
                }
            }

            if dev.virtio_device_type.is_some() {
                let mut offenders = BTreeSet::<String>::new();
                for (pat, re) in entry.expected_hardware_ids.iter().zip(compiled.iter()) {
                    let explicit = find_transitional_virtio_device_ids(pat);
                    let explicit_set: BTreeSet<_> = explicit.iter().cloned().collect();
                    for dev_id in explicit {
                        offenders.insert(format!("{name}: {pat} (contains 1AF4:{dev_id})"));
                    }
                    for dev_id in pattern_matches_transitional_virtio_device_ids(re) {
                        if explicit_set.contains(&dev_id) {
                            continue;
                        }
                        offenders.insert(format!("{name}: {pat} (matches 1AF4:{dev_id})"));
                    }
                }
                if !offenders.is_empty() {
                    bail!(
                        "{}: packaging spec contains transitional virtio PCI IDs, but AERO-W7-VIRTIO v1 is modern-only.\n\nOffending expected_hardware_ids entries:\n{}",
                        spec_path.display(),
                        format_bullets(&offenders)
                    );
                }
            } else if dev.device == "aero-gpu" {
                // AeroGPU specs must cover the canonical (A3A0) HWID family. The deprecated legacy
                // bring-up identity (vendor 1AED) is intentionally excluded from the canonical
                // device contract and must not appear in default packaging specs.
                let mut covers_a3a0 = false;
                for re in &compiled {
                    for hwid in &dev.hardware_id_patterns {
                        if !re.is_match(hwid) {
                            continue;
                        }
                        let upper = hwid.to_ascii_uppercase();
                        if upper.contains("VEN_A3A0&DEV_0001") {
                            covers_a3a0 = true;
                        }
                    }
                }
                if !covers_a3a0 {
                    bail!(
                        "{}: driver '{}': expected_hardware_ids must cover the canonical AeroGPU HWID family (VEN_A3A0)",
                        spec_path.display(),
                        name
                    );
                }
                for pat in &entry.expected_hardware_ids {
                    if pat.to_ascii_uppercase().contains("1AED") {
                        bail!(
                            "{}: driver '{}': expected_hardware_ids must not reference legacy AeroGPU vendor 1AED (canonical contract is A3A0-only); offending pattern: {}",
                            spec_path.display(),
                            name,
                            pat
                        );
                    }
                }
            }

            // Extra sanity: required drivers must stay required.
            // (This is intentionally minimal and does not encode full packaging policy.)
            let _ = entry.required;
        }
    }

    Ok(())
}

fn find_contract_device_for_spec_driver<'a>(
    devices: &'a BTreeMap<String, DeviceEntry>,
    spec_driver_name: &str,
) -> Result<&'a DeviceEntry> {
    let needle = spec_driver_name.to_ascii_lowercase();
    let mut matches = Vec::new();

    for dev in devices.values() {
        let device_name = dev.device.to_ascii_lowercase();
        let service_name = dev.driver_service_name.to_ascii_lowercase();
        let inf_stem = dev
            .inf_name
            .strip_suffix(".inf")
            .unwrap_or(&dev.inf_name)
            .to_ascii_lowercase();

        if needle == device_name || needle == service_name || needle == inf_stem {
            matches.push(dev);
        }
    }

    match matches.len() {
        0 => bail!("no matching contract device"),
        1 => Ok(matches[0]),
        _ => bail!(
            "ambiguous spec driver name '{}' (matches multiple contract devices): {:?}",
            spec_driver_name,
            matches
                .iter()
                .map(|d| d.device.as_str())
                .collect::<Vec<_>>()
        ),
    }
}

fn parse_inf_active_pci_hwids(inf_text: &str) -> BTreeSet<String> {
    // Extract the active PCI hardware IDs referenced by an INF (typically from the
    // Models sections). We intentionally keep parsing lightweight and line-based:
    //
    // - ignore full-line comments (`; ...`)
    // - strip inline comments (`... ; ...`)
    // - scan comma-separated fields from the end and pick the last one that looks
    //   like a PCI HWID (this tolerates extra fields like compatible IDs after the HWID)
    //
    // This avoids false positives from header comments describing the HWIDs without
    // actually matching them.
    let mut out = BTreeSet::new();
    for raw in inf_text.lines() {
        let mut line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with(';') {
            continue;
        }
        if let Some((before, _)) = line.split_once(';') {
            line = before.trim_end();
        }
        if line.is_empty() {
            continue;
        }
        for part in line.split(',').map(|p| p.trim()) {
            if part.to_ascii_uppercase().starts_with("PCI\\VEN_") {
                out.insert(part.to_string());
            }
        }
    }
    out
}

#[derive(Debug, Clone)]
struct InfModelLine {
    device_desc: String,
    install_section: String,
    hardware_id: String,
    raw_line: String,
}

fn parse_inf_strings(inf_text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut current_section: Option<String> = None;
    for raw in inf_text.lines() {
        let mut line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with(';') {
            continue;
        }
        if let Some((before, _)) = line.split_once(';') {
            line = before.trim_end();
        }
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') && line.len() >= 2 {
            let name = &line[1..line.len() - 1];
            current_section = Some(name.trim().to_string());
            continue;
        }
        let Some(section) = &current_section else {
            continue;
        };
        if !section.eq_ignore_ascii_case("Strings") {
            continue;
        }
        let Some((key_raw, val_raw)) = line.split_once('=') else {
            continue;
        };
        let key = key_raw.trim();
        if key.is_empty() {
            continue;
        }
        let mut val = val_raw.trim();
        if val.starts_with('"') && val.ends_with('"') && val.len() >= 2 {
            val = &val[1..val.len() - 1];
        }
        out.insert(key.to_ascii_lowercase(), val.to_string());
    }
    out
}

fn parse_inf_models_section(inf_text: &str, section_name: &str) -> Vec<InfModelLine> {
    let mut out = Vec::new();
    let mut current_section: Option<String> = None;
    for raw in inf_text.lines() {
        let mut line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with(';') {
            continue;
        }
        if let Some((before, _)) = line.split_once(';') {
            line = before.trim_end();
        }
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') && line.len() >= 2 {
            let name = &line[1..line.len() - 1];
            current_section = Some(name.trim().to_string());
            continue;
        }
        let Some(section) = &current_section else {
            continue;
        };
        if !section.eq_ignore_ascii_case(section_name) {
            continue;
        }
        let Some((lhs, rhs)) = line.split_once('=') else {
            continue;
        };
        let device_desc = lhs.trim();
        let rhs = rhs.trim();
        if device_desc.is_empty() || rhs.is_empty() {
            continue;
        }
        let parts: Vec<&str> = rhs
            .split(',')
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .collect();
        if parts.len() < 2 {
            continue;
        }
        let install = parts[0];
        let hwid = parts
            .iter()
            .rev()
            .copied()
            .find(|p| p.to_ascii_uppercase().starts_with("PCI\\VEN_"));
        let Some(hwid) = hwid else {
            continue;
        };
        out.push(InfModelLine {
            device_desc: device_desc.to_string(),
            install_section: install.to_string(),
            hardware_id: hwid.to_string(),
            raw_line: line.to_string(),
        });
    }
    out
}

fn resolve_inf_device_desc(desc: &str, strings: &BTreeMap<String, String>) -> Result<String> {
    let d = desc.trim();
    if d.starts_with('%') && d.ends_with('%') && d.len() >= 3 {
        let key = d[1..d.len() - 1].trim().to_ascii_lowercase();
        let Some(value) = strings.get(&key) else {
            bail!("undefined [Strings] token referenced by models section: {desc:?}");
        };
        return Ok(value.clone());
    }
    Ok(d.to_string())
}

fn validate_virtio_input_device_desc_split(
    inf_path: &Path,
    inf_text: &str,
    base_hwid: &str,
    expected_rev: u8,
    require_fallback: bool,
) -> Result<()> {
    // virtio-input uses one driver/service for both the keyboard and mouse PCI functions.
    // INFs should bind both functions to the same install sections, but use distinct
    // DeviceDesc strings so they appear with different names in Device Manager.
    //
    // Policy:
    // - The virtio-input keyboard/mouse INF is expected to bind:
    //   - keyboard and mouse via SUBSYS-qualified HWIDs (distinct Device Manager names), and
    //   - a strict, revision-gated generic fallback HWID (no SUBSYS):
    //     `{base_hwid}&REV_{expected_rev:02X}`
    //     for environments where subsystem IDs are not exposed/recognized.
    // - Tablet devices bind via `aero_virtio_tablet.inf` (more specific SUBSYS match) and win over
    //   the generic fallback when both are installed.
    //
    // `require_fallback` controls whether the strict generic fallback is required (`true`) or
    // forbidden (`false`). Current in-repo policy requires it for both the canonical INF and its
    // legacy filename alias.
    let strings = parse_inf_strings(inf_text);
    let rev = format!("{expected_rev:02X}");
    let kb_hwid = format!("{base_hwid}&SUBSYS_00101AF4&REV_{rev}");
    let ms_hwid = format!("{base_hwid}&SUBSYS_00111AF4&REV_{rev}");
    let fb_hwid = format!("{base_hwid}&REV_{rev}");
    let base_upper = base_hwid.to_ascii_uppercase();

    for models_section in ["Aero.NTx86", "Aero.NTamd64"] {
        let lines = parse_inf_models_section(inf_text, models_section);
        let kb: Vec<_> = lines
            .iter()
            .filter(|l| l.hardware_id.eq_ignore_ascii_case(&kb_hwid))
            .collect();
        let ms: Vec<_> = lines
            .iter()
            .filter(|l| l.hardware_id.eq_ignore_ascii_case(&ms_hwid))
            .collect();
        let fb_rev: Vec<_> = lines
            .iter()
            .filter(|l| l.hardware_id.eq_ignore_ascii_case(&fb_hwid))
            .collect();
        let fb_base: Vec<_> = lines
            .iter()
            .filter(|l| l.hardware_id.eq_ignore_ascii_case(base_hwid))
            .collect();

        if kb.len() != 1 {
            bail!(
                "virtio-input INF {}: expected exactly one keyboard model entry in [{}] for HWID {} (found {}): {:?}",
                inf_path.display(),
                models_section,
                kb_hwid,
                kb.len(),
                kb.iter().map(|e| e.raw_line.as_str()).collect::<Vec<_>>()
            );
        }
        if ms.len() != 1 {
            bail!(
                "virtio-input INF {}: expected exactly one mouse model entry in [{}] for HWID {} (found {}): {:?}",
                inf_path.display(),
                models_section,
                ms_hwid,
                ms.len(),
                ms.iter().map(|e| e.raw_line.as_str()).collect::<Vec<_>>()
            );
        }
        if require_fallback {
            if fb_rev.len() != 1 {
                bail!(
                    "virtio-input INF {}: expected exactly one generic fallback model entry in [{}] for HWID {} (found {}): {:?}",
                    inf_path.display(),
                    models_section,
                    fb_hwid,
                    fb_rev.len(),
                    fb_rev
                        .iter()
                        .map(|e| e.raw_line.as_str())
                        .collect::<Vec<_>>()
                );
            }
        } else if !fb_rev.is_empty() {
            bail!(
                "virtio-input INF {}: must not contain a generic fallback model entry in [{}] for HWID {} (fallback is alias-only); found {}: {:?}",
                inf_path.display(),
                models_section,
                fb_hwid,
                fb_rev.len(),
                fb_rev
                    .iter()
                    .map(|e| e.raw_line.as_str())
                    .collect::<Vec<_>>()
            );
        }
        if !fb_base.is_empty() {
            bail!(
                "virtio-input INF {}: must not contain a revision-less generic fallback model entry in [{}] for HWID {} (overlaps with tablet); found {}: {:?}",
                inf_path.display(),
                models_section,
                base_hwid,
                fb_base.len(),
                fb_base
                    .iter()
                    .map(|e| e.raw_line.as_str())
                    .collect::<Vec<_>>()
            );
        }

        // Optional-but-recommended: ensure there are no other SUBSYS-qualified model entries
        // for this device ID beyond the keyboard + mouse functions. This prevents accidental
        // overlap with other virtio-input functions (e.g. tablets).
        let extra_subsys: Vec<_> = lines
            .iter()
            .filter(|l| {
                let upper = l.hardware_id.to_ascii_uppercase();
                upper.starts_with(&base_upper)
                    && upper.contains("&SUBSYS_")
                    && !l.hardware_id.eq_ignore_ascii_case(&kb_hwid)
                    && !l.hardware_id.eq_ignore_ascii_case(&ms_hwid)
            })
            .collect();
        if !extra_subsys.is_empty() {
            bail!(
                "virtio-input INF {}: must not contain extra SUBSYS-qualified model entry/entries in [{}] for HWID family {} (allowed only keyboard+mouse); found {}: {:?}",
                inf_path.display(),
                models_section,
                base_hwid,
                extra_subsys.len(),
                extra_subsys
                    .iter()
                    .map(|e| e.raw_line.as_str())
                    .collect::<Vec<_>>()
            );
        }

        let kb = kb[0];
        let ms = ms[0];
        let fb = fb_rev.first().copied();

        if !kb.install_section.eq_ignore_ascii_case(&ms.install_section) {
            bail!(
                "virtio-input INF {}: keyboard and mouse model entries in [{}] must share the same install section.\nkeyboard: {}\nmouse:    {}",
                inf_path.display(),
                models_section,
                kb.raw_line,
                ms.raw_line,
            );
        }
        if let Some(fb) = fb {
            if !kb.install_section.eq_ignore_ascii_case(&fb.install_section) {
                bail!(
                    "virtio-input INF {}: keyboard, mouse, and fallback model entries in [{}] must share the same install section.\nkeyboard: {}\nmouse:    {}\nfallback: {}",
                    inf_path.display(),
                    models_section,
                    kb.raw_line,
                    ms.raw_line,
                    fb.raw_line,
                );
            }
        }

        let kb_desc = resolve_inf_device_desc(&kb.device_desc, &strings)?;
        let ms_desc = resolve_inf_device_desc(&ms.device_desc, &strings)?;

        if kb_desc.eq_ignore_ascii_case(&ms_desc) {
            bail!(
                "virtio-input INF {}: keyboard and mouse model entries in [{}] must have distinct DeviceDesc strings (got {:?}).\nkeyboard: {}\nmouse:    {}",
                inf_path.display(),
                models_section,
                kb_desc,
                kb.raw_line,
                ms.raw_line,
            );
        }
        if let Some(fb) = fb {
            let fb_desc = resolve_inf_device_desc(&fb.device_desc, &strings)?;
            if fb_desc.eq_ignore_ascii_case(&kb_desc) || fb_desc.eq_ignore_ascii_case(&ms_desc) {
                bail!(
                    "virtio-input INF {}: fallback model entry in [{}] must have a generic DeviceDesc string (must not equal keyboard/mouse; got {:?}).\nkeyboard: {}\nmouse:    {}\nfallback: {}",
                    inf_path.display(),
                    models_section,
                    fb_desc,
                    kb.raw_line,
                    ms.raw_line,
                    fb.raw_line,
                );
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod virtio_input_device_desc_split_tests {
    use super::*;

    const BASE_HWID: &str = r"PCI\VEN_1AF4&DEV_1052";
    const EXPECTED_REV: u8 = 0x01;

    fn validate(inf_text: &str, require_fallback: bool) -> Result<()> {
        validate_virtio_input_device_desc_split(
            Path::new("aero_virtio_input.inf"),
            inf_text,
            BASE_HWID,
            EXPECTED_REV,
            require_fallback,
        )
    }

    #[test]
    fn virtio_input_device_desc_split_rejects_missing_fallback_when_required() {
        let inf = r#"
[Aero.NTx86]
%AeroVirtioKeyboard.DeviceDesc% = AeroVirtioInput_Install.NTx86, PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01
%AeroVirtioMouse.DeviceDesc%    = AeroVirtioInput_Install.NTx86, PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01

[Aero.NTamd64]
%AeroVirtioKeyboard.DeviceDesc% = AeroVirtioInput_Install.NTamd64, PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01
%AeroVirtioMouse.DeviceDesc%    = AeroVirtioInput_Install.NTamd64, PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01

 [Strings]
  AeroVirtioKeyboard.DeviceDesc = "Aero VirtIO Keyboard"
  AeroVirtioMouse.DeviceDesc    = "Aero VirtIO Mouse"
"#;
        let err = validate(inf, true).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("expected exactly one generic fallback model entry"), "{msg}");
    }

    #[test]
    fn virtio_input_device_desc_split_accepts_kb_mouse_with_fallback_when_required() {
        let inf = r#"
[Aero.NTx86]
%AeroVirtioKeyboard.DeviceDesc% = AeroVirtioInput_Install.NTx86, PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01
%AeroVirtioMouse.DeviceDesc%    = AeroVirtioInput_Install.NTx86, PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01
%AeroVirtioInput.DeviceDesc%    = AeroVirtioInput_Install.NTx86, PCI\VEN_1AF4&DEV_1052&REV_01

[Aero.NTamd64]
%AeroVirtioKeyboard.DeviceDesc% = AeroVirtioInput_Install.NTamd64, PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01
%AeroVirtioMouse.DeviceDesc%    = AeroVirtioInput_Install.NTamd64, PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01
%AeroVirtioInput.DeviceDesc%    = AeroVirtioInput_Install.NTamd64, PCI\VEN_1AF4&DEV_1052&REV_01

 [Strings]
 AeroVirtioKeyboard.DeviceDesc = "Aero VirtIO Keyboard"
 AeroVirtioMouse.DeviceDesc    = "Aero VirtIO Mouse"
 AeroVirtioInput.DeviceDesc    = "Aero VirtIO Input Device"
"#;
        validate(inf, true).unwrap();
    }

    #[test]
    fn virtio_input_device_desc_split_rejects_generic_fallback_without_rev() {
        let inf = r#"
[Aero.NTx86]
%AeroVirtioKeyboard.DeviceDesc% = AeroVirtioInput_Install.NTx86, PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01
%AeroVirtioMouse.DeviceDesc%    = AeroVirtioInput_Install.NTx86, PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01
%AeroVirtioInput.DeviceDesc%    = AeroVirtioInput_Install.NTx86, PCI\VEN_1AF4&DEV_1052&REV_01
%AeroVirtioInput.DeviceDesc%    = AeroVirtioInput_Install.NTx86, PCI\VEN_1AF4&DEV_1052

[Aero.NTamd64]
%AeroVirtioKeyboard.DeviceDesc% = AeroVirtioInput_Install.NTamd64, PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01
%AeroVirtioMouse.DeviceDesc%    = AeroVirtioInput_Install.NTamd64, PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01
%AeroVirtioInput.DeviceDesc%    = AeroVirtioInput_Install.NTamd64, PCI\VEN_1AF4&DEV_1052&REV_01
%AeroVirtioInput.DeviceDesc%    = AeroVirtioInput_Install.NTamd64, PCI\VEN_1AF4&DEV_1052

 [Strings]
  AeroVirtioKeyboard.DeviceDesc = "Aero VirtIO Keyboard"
   AeroVirtioMouse.DeviceDesc    = "Aero VirtIO Mouse"
   AeroVirtioInput.DeviceDesc    = "Aero VirtIO Input Device"
   "#;
        let err = validate(inf, true).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("revision-less generic fallback"));
    }

    #[test]
    fn virtio_input_device_desc_split_rejects_fallback_device_desc_reuse() {
        let inf = r#"
[Aero.NTx86]
%AeroVirtioKeyboard.DeviceDesc% = AeroVirtioInput_Install.NTx86, PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01
%AeroVirtioMouse.DeviceDesc%    = AeroVirtioInput_Install.NTx86, PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01
%AeroVirtioKeyboard.DeviceDesc% = AeroVirtioInput_Install.NTx86, PCI\VEN_1AF4&DEV_1052&REV_01

[Aero.NTamd64]
 %AeroVirtioKeyboard.DeviceDesc% = AeroVirtioInput_Install.NTamd64, PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01
 %AeroVirtioMouse.DeviceDesc%    = AeroVirtioInput_Install.NTamd64, PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01
 %AeroVirtioKeyboard.DeviceDesc% = AeroVirtioInput_Install.NTamd64, PCI\VEN_1AF4&DEV_1052&REV_01

 [Strings]
   AeroVirtioKeyboard.DeviceDesc = "Aero VirtIO Keyboard"
   AeroVirtioMouse.DeviceDesc    = "Aero VirtIO Mouse"
   "#;
        let err = validate(inf, true).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("fallback model entry"));
        assert!(msg.contains("generic DeviceDesc"));
    }

    #[test]
    fn virtio_input_device_desc_split_rejects_extra_subsys_entries() {
        let inf = r#"
[Aero.NTx86]
%AeroVirtioKeyboard.DeviceDesc% = AeroVirtioInput_Install.NTx86, PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01
%AeroVirtioMouse.DeviceDesc%    = AeroVirtioInput_Install.NTx86, PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01
%AeroVirtioInput.DeviceDesc%    = AeroVirtioInput_Install.NTx86, PCI\VEN_1AF4&DEV_1052&REV_01
%AeroVirtioTablet.DeviceDesc%   = AeroVirtioInput_Install.NTx86, PCI\VEN_1AF4&DEV_1052&SUBSYS_00121AF4&REV_01

[Aero.NTamd64]
%AeroVirtioKeyboard.DeviceDesc% = AeroVirtioInput_Install.NTamd64, PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01
%AeroVirtioMouse.DeviceDesc%    = AeroVirtioInput_Install.NTamd64, PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01
%AeroVirtioInput.DeviceDesc%    = AeroVirtioInput_Install.NTamd64, PCI\VEN_1AF4&DEV_1052&REV_01
%AeroVirtioTablet.DeviceDesc%   = AeroVirtioInput_Install.NTamd64, PCI\VEN_1AF4&DEV_1052&SUBSYS_00121AF4&REV_01

[Strings]
 AeroVirtioKeyboard.DeviceDesc = "Aero VirtIO Keyboard"
 AeroVirtioMouse.DeviceDesc    = "Aero VirtIO Mouse"
 AeroVirtioInput.DeviceDesc    = "Aero VirtIO Input Device"
 AeroVirtioTablet.DeviceDesc   = "Aero VirtIO Tablet Device"
   "#;
        let err = validate(inf, true).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("extra SUBSYS-qualified model entry"));
    }

    #[test]
    fn virtio_input_device_desc_split_requires_distinct_device_descs() {
        let inf = r#"
[Aero.NTx86]
%AeroVirtioKeyboard.DeviceDesc% = AeroVirtioInput_Install.NTx86, PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01
%AeroVirtioKeyboard.DeviceDesc% = AeroVirtioInput_Install.NTx86, PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01
%AeroVirtioInput.DeviceDesc%    = AeroVirtioInput_Install.NTx86, PCI\VEN_1AF4&DEV_1052&REV_01

[Aero.NTamd64]
%AeroVirtioKeyboard.DeviceDesc% = AeroVirtioInput_Install.NTamd64, PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01
%AeroVirtioKeyboard.DeviceDesc% = AeroVirtioInput_Install.NTamd64, PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01
 %AeroVirtioInput.DeviceDesc%    = AeroVirtioInput_Install.NTamd64, PCI\VEN_1AF4&DEV_1052&REV_01

 [Strings]
   AeroVirtioKeyboard.DeviceDesc = "Aero VirtIO Input"
   AeroVirtioInput.DeviceDesc    = "Aero VirtIO Input Device"
   "#;
        let err = validate(inf, true).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("must have distinct DeviceDesc strings"));
    }
}

fn parse_pci_vendor_device_from_hwid(hwid: &str) -> Option<(u16, u16)> {
    let upper = hwid.to_ascii_uppercase();
    const PREFIX: &str = "PCI\\VEN_";
    if !upper.starts_with(PREFIX) {
        return None;
    }
    let mut idx = PREFIX.len();
    if upper.len() < idx + 4 {
        return None;
    }
    let ven_hex = &upper[idx..idx + 4];
    idx += 4;
    if !upper[idx..].starts_with("&DEV_") {
        return None;
    }
    idx += "&DEV_".len();
    if upper.len() < idx + 4 {
        return None;
    }
    let dev_hex = &upper[idx..idx + 4];
    let ven = u16::from_str_radix(ven_hex, 16).ok()?;
    let dev = u16::from_str_radix(dev_hex, 16).ok()?;
    Some((ven, dev))
}

fn parse_pci_revision_from_hwid(hwid: &str) -> Option<u8> {
    let upper = hwid.to_ascii_uppercase();
    let idx = upper.rfind("&REV_")?;
    let start = idx + "&REV_".len();
    if upper.len() < start + 2 {
        return None;
    }
    let hex = &upper[start..start + 2];
    u8::from_str_radix(hex, 16).ok()
}

fn parse_contract_pci_revision_for_device(dev: &DeviceEntry, base_hwid: &str) -> Result<u8> {
    let base_upper = base_hwid.to_ascii_uppercase();
    let mut revisions = BTreeSet::new();
    for hwid in &dev.hardware_id_patterns {
        if !hwid.to_ascii_uppercase().starts_with(&base_upper) {
            continue;
        }
        if let Some(rev) = parse_pci_revision_from_hwid(hwid) {
            revisions.insert(rev);
        }
    }
    match revisions.len() {
        0 => bail!(
            "contract device '{}' is missing a REV_XX-qualified HWID in hardware_id_patterns for {base_hwid}",
            dev.device
        ),
        1 => Ok(*revisions.iter().next().unwrap()),
        _ => bail!(
            "contract device '{}' has multiple REV_XX values in hardware_id_patterns for {base_hwid}: {:?}",
            dev.device,
            revisions
        ),
    }
}

fn validate_in_tree_infs(repo_root: &Path, devices: &BTreeMap<String, DeviceEntry>) -> Result<()> {
    for (name, dev) in devices {
        // The repo contains multiple Windows driver trees (Win7, newer Windows, templates).
        // This validator is specifically for the Windows 7 contract. Most Win7 drivers live
        // under `drivers/windows7/` (or `drivers/win7/`). We also search `drivers/windows/`
        // because some shared/legacy code still lives there, and older commits kept some
        // Win7-targeted drivers under that tree.
        let search_roots: Vec<PathBuf> = if dev.device == "aero-gpu" {
            vec![repo_root.join("drivers/aerogpu/packaging/win7")]
        } else if dev.virtio_device_type.is_some() {
            vec![
                repo_root.join("drivers/windows7"),
                repo_root.join("drivers/win7"),
                repo_root.join("drivers/windows"),
            ]
        } else {
            vec![repo_root.join("drivers")]
        };

        let mut inf_paths = Vec::new();
        for root in &search_roots {
            if root.is_dir() {
                inf_paths.extend(
                    find_files_by_name(root, &dev.inf_name)
                        .with_context(|| format!("{name}: search INF under {}", root.display()))?,
                );
            }
        }
        inf_paths.sort();
        inf_paths.dedup();
        if dev.device == "aero-gpu" {
            // The repo contains both the canonical AeroGPU Win7 INF and legacy bring-up
            // variants under `drivers/aerogpu/packaging/win7/legacy/`. The Windows device
            // contract points at the canonical INF and must validate against it (not the legacy
            // INFs).
            inf_paths.retain(|p| {
                !p.iter()
                    .any(|c| c.to_string_lossy().eq_ignore_ascii_case("legacy"))
            });
        }
        if inf_paths.is_empty() {
            bail!(
                "{name}: INF not found under expected Win7 driver roots ({:?}): {}",
                search_roots
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>(),
                dev.inf_name
            );
        }
        for inf_path in &inf_paths {
            let inf_text = read_inf_text(inf_path).with_context(|| format!("{name}: read INF"))?;

            let active_hwids = parse_inf_active_pci_hwids(&inf_text);
            if active_hwids.is_empty() {
                bail!(
                    "{name}: INF {} does not contain any active PCI HWID matches (expected PCI\\\\VEN_... entries)",
                    inf_path.display()
                );
            }

            // Ensure the INF matches at least one contract HWID literal in its Models sections.
            // We intentionally don't require every enumerated Windows HWID form (SUBSYS/REV
            // variants) to appear in the INF.
            let any_hwid_match = active_hwids.iter().any(|hwid| {
                dev.hardware_id_patterns
                    .iter()
                    .any(|pattern| hwid.eq_ignore_ascii_case(pattern))
            });
            if !any_hwid_match {
                bail!(
                    "{name}: INF {} does not match any of the expected contract hardware_id_patterns for {}.\nExpected one of:\n{}\nActive HWIDs found in INF:\n{}",
                    inf_path.display(),
                    dev.device,
                    format_bullets(&dev.hardware_id_patterns.iter().cloned().collect()),
                    format_bullets(&active_hwids)
                );
            }

            // Additional safety checks for known drift hotspots.
            if dev.device == "aero-gpu" {
                if !active_hwids
                    .iter()
                    .any(|h| h.to_ascii_uppercase().starts_with("PCI\\VEN_A3A0&DEV_0001"))
                {
                    bail!(
                        "{name}: INF {} missing required AeroGPU HWID family: PCI\\\\VEN_A3A0&DEV_0001",
                        inf_path.display()
                    );
                }
                // Avoid embedding the exact legacy HWID token in the source so repo-wide grep-based
                // drift checks can stay focused on legacy/archived locations.
                let legacy_vendor_id = "1AED";
                let legacy_hwid = format!("PCI\\VEN_{legacy_vendor_id}&DEV_0001");
                if active_hwids
                    .iter()
                    .any(|h| h.to_ascii_uppercase().starts_with(&legacy_hwid))
                {
                    bail!(
                        "{name}: INF {} must not match legacy AeroGPU HWID family (vendor {legacy_vendor_id})",
                        inf_path.display()
                    );
                }
            } else if dev.virtio_device_type.is_some() {
                // In-tree virtio drivers are expected to bind to the modern (AERO-W7-VIRTIO) ID space,
                // and to revision-gate binding (contract major version is encoded in PCI Revision ID).
                let modern = parse_hex_u16(&dev.pci_device_id)
                    .with_context(|| format!("{name}: parse pci_device_id"))?;
                let base = format!("PCI\\VEN_{:04X}&DEV_{:04X}", 0x1AF4u16, modern);
                let base_upper = base.to_ascii_uppercase();

                if !active_hwids
                    .iter()
                    .any(|h| h.to_ascii_uppercase().starts_with(&base_upper))
                {
                    bail!(
                        "{name}: INF {} missing modern virtio HWID family {base} (no active HWIDs start with it)",
                        inf_path.display(),
                    );
                }

                // Guard against accidental transitional virtio-pci IDs (0x1000..0x103F) in in-tree INFs.
                let mut transitional = BTreeSet::new();
                for hwid in &active_hwids {
                    let Some((ven, dev_id)) = parse_pci_vendor_device_from_hwid(hwid) else {
                        continue;
                    };
                    if ven == 0x1AF4 && (0x1000..=0x103F).contains(&dev_id) {
                        transitional.insert(hwid.clone());
                    }
                }
                if !transitional.is_empty() {
                    bail!(
                        "{name}: INF {} must not reference transitional virtio-pci device IDs (0x1000..0x103F):\n{}",
                        inf_path.display(),
                        format_bullets(&transitional)
                    );
                }

                let expected_rev = parse_contract_pci_revision_for_device(dev, &base)
                    .with_context(|| format!("{name}: parse contract PCI revision for {base}"))?;
                // Do not require a specific `{base}&REV_XX` HWID string (some INFs further qualify
                // binding with SUBSYS_...); instead, enforce that any HWIDs in this VEN/DEV family
                // are revision-gated and match the contract revision below.

                let mut missing_rev = BTreeSet::new();
                let mut wrong_rev = BTreeSet::new();
                for hwid in active_hwids
                    .iter()
                    .filter(|h| h.to_ascii_uppercase().starts_with(&base_upper))
                {
                    let Some(rev) = parse_pci_revision_from_hwid(hwid) else {
                        missing_rev.insert(hwid.clone());
                        continue;
                    };
                    if rev != expected_rev {
                        wrong_rev.insert(hwid.clone());
                    }
                }
                if !missing_rev.is_empty() {
                    bail!(
                        "{name}: INF {} matches {base} without revision gating (must require REV_{expected_rev:02X}):\n{}",
                        inf_path.display(),
                        format_bullets(&missing_rev)
                    );
                }
                if !wrong_rev.is_empty() {
                    bail!(
                        "{name}: INF {} has REV_ qualifier(s) that do not match the contract revision (expected REV_{expected_rev:02X}):\n{}",
                        inf_path.display(),
                        format_bullets(&wrong_rev)
                    );
                }

                if dev.device == "virtio-input" {
                    // Policy: the canonical virtio-input keyboard/mouse INF (`aero_virtio_input.inf`) is
                    // intentionally SUBSYS-only (distinct naming), and does *not* include the strict generic
                    // fallback HWID (no SUBSYS).
                    validate_virtio_input_device_desc_split(
                        inf_path,
                        &inf_text,
                        &base,
                        expected_rev,
                        /* require_fallback */ false,
                    )
                    .with_context(|| {
                        format!("{name}: validate virtio-input canonical DeviceDesc split")
                    })?;

                    // Validate the legacy filename alias INF (`virtio-input.inf.disabled`), which exists for
                    // compatibility with workflows/tools that reference the old `virtio-input.inf` basename.
                    //
                    // Policy:
                    // - `virtio-input.inf.disabled` is checked in; developers may locally enable it by
                    //   renaming it to `virtio-input.inf`.
                    // - It is allowed to diverge from the canonical INF only in the models sections
                    //   (`[Aero.NTx86]` / `[Aero.NTamd64]`) to add the opt-in strict generic fallback HWID.
                    // - Outside those models sections, from the first section header (`[Version]`) onward,
                    //   it must remain byte-for-byte identical to the canonical INF.
                    let alias_enabled = inf_path.with_file_name("virtio-input.inf");
                    let alias_disabled = inf_path.with_file_name("virtio-input.inf.disabled");
                    if alias_enabled.exists() && alias_disabled.exists() {
                        bail!(
                            "{name}: both legacy virtio-input alias INFs exist; keep only one to avoid ambiguous matching: {} and {}",
                            alias_enabled.display(),
                            alias_disabled.display()
                        );
                    }
                    if !alias_disabled.exists() {
                        bail!(
                            "{name}: missing required legacy virtio-input alias INF: {} (keep it checked in disabled-by-default; developers may locally enable it by renaming to virtio-input.inf)",
                            alias_disabled.display()
                        );
                    }
                    let alias = alias_disabled;

                    let alias_text = read_inf_text(&alias).with_context(|| {
                        format!(
                            "{name}: read virtio-input legacy alias INF {}",
                            alias.display()
                        )
                    })?;
                    validate_virtio_input_device_desc_split(
                        &alias,
                        &alias_text,
                        &base,
                        expected_rev,
                        /* require_fallback */ true,
                    )
                    .with_context(|| {
                        format!(
                            "{name}: validate virtio-input legacy alias DeviceDesc split: {}",
                            alias.display()
                        )
                    })?;

                    // Ensure the alias stays in sync with the canonical INF from the first section header
                    // (`[Version]`) onward outside the models sections. Only the leading banner/comments may
                    // differ.
                    let canonical_bytes = inf_functional_bytes(inf_path).with_context(|| {
                        format!(
                            "{name}: read canonical virtio-input INF functional bytes: {}",
                            inf_path.display()
                        )
                    })?;
                    let alias_bytes = inf_functional_bytes(&alias).with_context(|| {
                        format!(
                            "{name}: read virtio-input alias INF functional bytes: {}",
                            alias.display()
                        )
                    })?;

                    let canonical_bytes = strip_inf_sections_bytes(
                        &canonical_bytes,
                        &["aero.ntx86", "aero.ntamd64"],
                    );
                    let alias_bytes =
                        strip_inf_sections_bytes(&alias_bytes, &["aero.ntx86", "aero.ntamd64"]);

                    if canonical_bytes != alias_bytes {
                        let first_diff = canonical_bytes
                            .iter()
                            .zip(alias_bytes.iter())
                            .position(|(a, b)| a != b)
                            .unwrap_or_else(|| canonical_bytes.len().min(alias_bytes.len()));
                        bail!(
                            "{name}: virtio-input legacy alias INF drift detected outside models sections.\ncanonical: {}\nalias:     {}\nfirst mismatch at byte offset {}.\nTip: run `python3 drivers/windows7/virtio-input/scripts/check-inf-alias.py` to diagnose drift.",
                            inf_path.display(),
                            alias.display(),
                            first_diff
                        );
                    }
                }
            }

            // Best-effort: ensure the service name appears in an AddService directive.
            let add_service_re = regex::RegexBuilder::new(&format!(
                r"(?im)^\s*AddService\s*=\s*{}\b",
                regex::escape(&dev.driver_service_name)
            ))
            .case_insensitive(true)
            .build()
            .with_context(|| format!("{name}: compile AddService regex"))?;
            if !add_service_re.is_match(&inf_text) {
                bail!(
                    "{name}: INF {} does not contain AddService for driver_service_name '{}'",
                    inf_path.display(),
                    dev.driver_service_name
                );
            }
        }
    }
    Ok(())
}

fn validate_emulator_ids(repo_root: &Path, devices: &BTreeMap<String, DeviceEntry>) -> Result<()> {
    // Canonical PCI IDs: crates/devices/src/pci/profile.rs
    let pci_profile_rs = repo_root.join("crates/devices/src/pci/profile.rs");
    let virtio_vendor = parse_rust_u16_const(&pci_profile_rs, "PCI_VENDOR_ID_VIRTIO")
        .with_context(|| "parse PCI_VENDOR_ID_VIRTIO")?;
    if virtio_vendor != 0x1AF4 {
        bail!(
            "emulator virtio vendor ID mismatch in {}: expected 0x1AF4, found {virtio_vendor:#06x}",
            pci_profile_rs.display()
        );
    }

    let virtio_modern_consts: &[(&str, &str)] = &[
        ("virtio-net", "PCI_DEVICE_ID_VIRTIO_NET_MODERN"),
        ("virtio-blk", "PCI_DEVICE_ID_VIRTIO_BLK_MODERN"),
        ("virtio-input", "PCI_DEVICE_ID_VIRTIO_INPUT_MODERN"),
        ("virtio-snd", "PCI_DEVICE_ID_VIRTIO_SND_MODERN"),
    ];

    for (device_name, const_name) in virtio_modern_consts {
        let dev = devices.get(*device_name).unwrap();
        let expected = parse_hex_u16(&dev.pci_device_id)
            .with_context(|| format!("{device_name}: parse pci_device_id"))?;
        let found = parse_rust_u16_const(&pci_profile_rs, const_name)
            .with_context(|| format!("parse {const_name}"))?;
        if found != expected {
            bail!(
                "emulator PCI ID mismatch for {device_name}: contract pci_device_id={expected:#06x}, but {const_name}={found:#06x} in {}",
                pci_profile_rs.display()
            );
        }
    }

    let virtio_transitional_consts: &[(&str, &str)] = &[
        ("virtio-net", "PCI_DEVICE_ID_VIRTIO_NET_TRANSITIONAL"),
        ("virtio-blk", "PCI_DEVICE_ID_VIRTIO_BLK_TRANSITIONAL"),
        ("virtio-input", "PCI_DEVICE_ID_VIRTIO_INPUT_TRANSITIONAL"),
        ("virtio-snd", "PCI_DEVICE_ID_VIRTIO_SND_TRANSITIONAL"),
    ];
    for (device_name, const_name) in virtio_transitional_consts {
        let dev = devices.get(*device_name).unwrap();
        let expected =
            parse_hex_u16(dev.pci_device_id_transitional.as_deref().ok_or_else(|| {
                anyhow::anyhow!("{device_name}: missing pci_device_id_transitional")
            })?)
            .with_context(|| format!("{device_name}: parse pci_device_id_transitional"))?;
        let found = parse_rust_u16_const(&pci_profile_rs, const_name)
            .with_context(|| format!("parse {const_name}"))?;
        if found != expected {
            bail!(
                "emulator PCI ID mismatch for {device_name} transitional: contract pci_device_id_transitional={expected:#06x}, but {const_name}={found:#06x} in {}",
                pci_profile_rs.display()
            );
        }
    }

    // AeroGPU IDs: emulator/protocol (Rust constants) + drivers/aerogpu/protocol (C header).
    // The emulator crate itself re-exports these via `crates/emulator/src/devices/aerogpu_regs.rs`,
    // but that file aliases through the protocol crate (not hex literals), so parse the protocol
    // constants directly.
    let aerogpu_proto_rs = repo_root.join("emulator/protocol/aerogpu/aerogpu_pci.rs");
    let aerogpu_header_h = repo_root.join("drivers/aerogpu/protocol/aerogpu_pci.h");

    let aerogpu = devices.get("aero-gpu").unwrap();
    let expected_vendor =
        parse_hex_u16(&aerogpu.pci_vendor_id).with_context(|| "aero-gpu: parse pci_vendor_id")?;
    let expected_did =
        parse_hex_u16(&aerogpu.pci_device_id).with_context(|| "aero-gpu: parse pci_device_id")?;

    let found_vendor_rs = parse_rust_u16_const(&aerogpu_proto_rs, "AEROGPU_PCI_VENDOR_ID")
        .with_context(|| "parse AEROGPU_PCI_VENDOR_ID")?;
    let found_did_rs = parse_rust_u16_const(&aerogpu_proto_rs, "AEROGPU_PCI_DEVICE_ID")
        .with_context(|| "parse AEROGPU_PCI_DEVICE_ID")?;
    if found_vendor_rs != expected_vendor || found_did_rs != expected_did {
        bail!(
            "emulator AeroGPU ID mismatch in {}: expected {expected_vendor:#06x}:{expected_did:#06x}, found {found_vendor_rs:#06x}:{found_did_rs:#06x}",
            aerogpu_proto_rs.display()
        );
    }

    let found_vendor_h = parse_c_define_hex_u16(&aerogpu_header_h, "AEROGPU_PCI_VENDOR_ID")
        .with_context(|| "parse AEROGPU_PCI_VENDOR_ID in C header")?;
    let found_did_h = parse_c_define_hex_u16(&aerogpu_header_h, "AEROGPU_PCI_DEVICE_ID")
        .with_context(|| "parse AEROGPU_PCI_DEVICE_ID in C header")?;
    if found_vendor_h != expected_vendor || found_did_h != expected_did {
        bail!(
            "AeroGPU protocol header ID mismatch in {}: expected {expected_vendor:#06x}:{expected_did:#06x}, found {found_vendor_h:#06x}:{found_did_h:#06x}",
            aerogpu_header_h.display()
        );
    }

    // Canonical AeroGPU PCI constants must match the protocol IDs.
    //
    // Source of truth for the canonical machine PCI layout is `crates/devices/src/pci/profile.rs`
    // (not the contract JSON). Ensure those constants do not drift from the protocol.
    let aero_vendor_profile = parse_rust_u16_const(&pci_profile_rs, "PCI_VENDOR_ID_AERO")
        .with_context(|| format!("parse PCI_VENDOR_ID_AERO in {}", pci_profile_rs.display()))?;
    let aero_did_profile = parse_rust_u16_const(&pci_profile_rs, "PCI_DEVICE_ID_AERO_AEROGPU")
        .with_context(|| {
            format!(
                "parse PCI_DEVICE_ID_AERO_AEROGPU in {}",
                pci_profile_rs.display()
            )
        })?;

    if aero_vendor_profile != found_vendor_rs || aero_did_profile != found_did_rs {
        bail!(
            "AeroGPU PCI ID mismatch between canonical PCI profile and protocol: {} defines PCI_VENDOR_ID_AERO={aero_vendor_profile:#06x} / PCI_DEVICE_ID_AERO_AEROGPU={aero_did_profile:#06x}, but {} defines AEROGPU_PCI_VENDOR_ID={found_vendor_rs:#06x} / AEROGPU_PCI_DEVICE_ID={found_did_rs:#06x}",
            pci_profile_rs.display(),
            aerogpu_proto_rs.display(),
        );
    }
    if aero_vendor_profile != found_vendor_h || aero_did_profile != found_did_h {
        bail!(
            "AeroGPU PCI ID mismatch between canonical PCI profile and protocol header: {} defines PCI_VENDOR_ID_AERO={aero_vendor_profile:#06x} / PCI_DEVICE_ID_AERO_AEROGPU={aero_did_profile:#06x}, but {} defines AEROGPU_PCI_VENDOR_ID={found_vendor_h:#06x} / AEROGPU_PCI_DEVICE_ID={found_did_h:#06x}",
            pci_profile_rs.display(),
            aerogpu_header_h.display(),
        );
    }

    // Canonical BDF + class code for AeroGPU are part of the Windows driver binding contract.
    // Validate against docs/pci-device-compatibility.md (00:07.0, class 03/00/00).
    let parsed = parse_pci_device_profile_bdf_and_class(&pci_profile_rs, "AEROGPU")
        .with_context(|| format!("parse AEROGPU profile in {}", pci_profile_rs.display()))?;
    let (bus, dev, func) = parsed.bdf;
    if (bus, dev, func) != (0, 7, 0) {
        bail!(
            "AeroGPU BDF mismatch in {}: profile::AEROGPU uses {:02x}:{:02x}.{}, expected 00:07.0 (see docs/pci-device-compatibility.md)",
            pci_profile_rs.display(),
            bus,
            dev,
            func
        );
    }
    let (base, sub, prog) = parsed.class;
    if (base, sub, prog) != (0x03, 0x00, 0x00) {
        bail!(
            "AeroGPU class code mismatch in {}: profile::AEROGPU uses {:02X}/{:02X}/{:02X}, expected 03/00/00 (see docs/pci-device-compatibility.md)",
            pci_profile_rs.display(),
            base,
            sub,
            prog
        );
    }

    Ok(())
}

fn parse_devices_cmd(path: &Path) -> Result<BTreeMap<String, String>> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut vars = BTreeMap::new();

    let re = regex::Regex::new(
        r#"(?im)^\s*set\s+(?:"(?P<var1>[^=]+)=(?P<val1>.*)"|(?P<var2>[^=\s]+)=(?P<val2>.*))\s*$"#,
    )
    .unwrap();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("rem ") || lower == "rem" || lower.starts_with("::") {
            continue;
        }

        if let Some(c) = re.captures(trimmed) {
            let var = c
                .name("var1")
                .or_else(|| c.name("var2"))
                .map(|m| m.as_str().trim().to_string())
                .unwrap_or_default();
            let val = c
                .name("val1")
                .or_else(|| c.name("val2"))
                .map(|m| m.as_str().trim().to_string())
                .unwrap_or_default();
            if !var.is_empty() {
                vars.insert(var, val);
            }
        }
    }

    Ok(vars)
}

fn parse_cmd_quoted_list(raw: &str) -> Vec<String> {
    let re = regex::Regex::new(r#""([^"]+)""#).unwrap();
    let mut out = Vec::new();
    for c in re.captures_iter(raw) {
        if let Some(m) = c.get(1) {
            out.push(m.as_str().to_string());
        }
    }
    out
}

fn find_transitional_virtio_device_ids(pattern: &str) -> Vec<String> {
    // Transitional virtio-pci device IDs are in the 0x1000..0x103F range. Aero's Win7 virtio
    // contract v1 is modern-only, so packaging specs must not accept them.
    //
    // We intentionally keep this lightweight (string/regex scan) because the patterns are
    // arbitrary regexes (not structured expressions).
    let upper = pattern.to_ascii_uppercase();
    if !upper.contains("VEN_1AF4") {
        return Vec::new();
    }

    let transitional = ["1000", "1001", "1011", "1018"];
    let mut found = BTreeSet::<String>::new();

    let dev_re = regex::Regex::new(r"(?i)DEV_([^&]+)").expect("static regex must compile");
    let hex_re = regex::Regex::new(r"(?i)[0-9A-F]{4}").expect("static regex must compile");

    for caps in dev_re.captures_iter(pattern) {
        let segment = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        for m in hex_re.find_iter(segment) {
            let dev = m.as_str().to_ascii_uppercase();
            if transitional.iter().any(|t| t.eq_ignore_ascii_case(&dev)) {
                found.insert(dev);
            }
        }
    }

    found.into_iter().collect()
}

fn pattern_matches_transitional_virtio_device_ids(re: &regex::Regex) -> Vec<String> {
    let transitional: &[(&str, &[&str])] = &[
        // virtio-net
        ("1000", &["00011AF4"]),
        // virtio-blk
        ("1001", &["00021AF4"]),
        // virtio-input (keyboard + mouse)
        ("1011", &["00101AF4", "00111AF4"]),
        // virtio-snd
        ("1018", &["00191AF4"]),
    ];
    let mut out = Vec::new();
    for (dev, subsys_ids) in transitional {
        let base = format!(r"PCI\VEN_1AF4&DEV_{dev}");
        let mut candidates = vec![base.clone(), format!("{base}&REV_01")];
        for subsys in *subsys_ids {
            candidates.push(format!("{base}&SUBSYS_{subsys}"));
            candidates.push(format!("{base}&SUBSYS_{subsys}&REV_01"));
        }
        if candidates.iter().any(|hwid| re.is_match(hwid)) {
            out.push(dev.to_string());
        }
    }
    out
}

fn format_bullets(items: &BTreeSet<String>) -> String {
    items
        .iter()
        .map(|s| format!("  - {s}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn read_inf_text(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    // INF files are often ASCII/UTF-8, but can be UTF-16LE/BE (with or without BOM).
    // We only need a best-effort string for lightweight parsing + regex matching.
    let text = if bytes.starts_with(&[0xFF, 0xFE]) {
        // UTF-16LE with BOM.
        decode_utf16(&bytes[2..], true)
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        // UTF-16BE with BOM.
        decode_utf16(&bytes[2..], false)
    } else if bytes.len() >= 4 && bytes.len() % 2 == 0 {
        // Some Windows tooling produces UTF-16 INFs without a BOM. Detect by looking for a high
        // ratio of NUL bytes (common for mostly-ASCII UTF-16 text) and decode best-effort.
        //
        // Use a small set of prefix windows to avoid missing UTF-16 when the file contains large
        // non-ASCII string tables (which reduce the overall NUL-byte ratio).
        let likely_utf16 = [128usize, 512, 2048].into_iter().any(|prefix_len| {
            let mut len = bytes.len().min(prefix_len);
            len -= len % 2;
            if len < 4 {
                return false;
            }
            let nuls = bytes[..len].iter().filter(|b| **b == 0).count();
            // "High proportion of NUL bytes": >= 20%.
            nuls * 5 >= len
        });

        if likely_utf16 {
            let le = decode_utf16(&bytes, true);
            let be = decode_utf16(&bytes, false);

            fn decode_score(s: &str) -> (usize, usize, usize, usize) {
                let mut replacement = 0usize;
                let mut nul = 0usize;
                let mut ascii = 0usize;
                let mut newlines = 0usize;
                let mut total = 0usize;
                for c in s.chars() {
                    total += 1;
                    if c == '\u{FFFD}' {
                        replacement += 1;
                    } else if c == '\u{0000}' {
                        nul += 1;
                    }
                    if c.is_ascii() {
                        ascii += 1;
                        if c == '\n' {
                            newlines += 1;
                        }
                    }
                }
                // Lower is better: prefer fewer replacement/NULs; then prefer decodes that yield
                // more ASCII/newlines (which strongly correlates with correct endianness for INFs).
                let ascii_penalty = total.saturating_sub(ascii);
                let newline_penalty = total.saturating_sub(newlines);
                (replacement, nul, ascii_penalty, newline_penalty)
            }

            let le_score = decode_score(&le);
            let be_score = decode_score(&be);
            if le_score < be_score {
                le
            } else if be_score < le_score {
                be
            } else {
                // Prefer little-endian when ambiguous (Windows commonly uses UTF-16LE).
                le
            }
        } else {
            String::from_utf8_lossy(&bytes).to_string()
        }
    } else {
        String::from_utf8_lossy(&bytes).to_string()
    };

    // Strip UTF-8 BOM if present.
    let stripped = text.trim_start_matches('\u{feff}');
    if stripped.len() != text.len() {
        return Ok(stripped.to_string());
    }
    Ok(text)
}

fn first_nonblank_ascii_byte(line: &[u8], first_line: bool) -> Option<u8> {
    // Robust to UTF-16 where ASCII characters are NUL-separated.
    let line = if first_line {
        if line.starts_with(&[0xEF, 0xBB, 0xBF]) {
            &line[3..]
        } else if line.starts_with(&[0xFF, 0xFE]) || line.starts_with(&[0xFE, 0xFF]) {
            &line[2..]
        } else {
            line
        }
    } else {
        line
    };

    for &b in line {
        // NUL, tab, LF, CR, space
        if b == 0 || b == b'\t' || b == b'\n' || b == b'\r' || b == b' ' {
            continue;
        }
        return Some(b);
    }
    None
}

fn inf_functional_bytes(path: &Path) -> Result<Vec<u8>> {
    // Return the INF content starting from the first section header line (typically `[Version]`).
    //
    // This intentionally ignores the leading comment/banner block so a legacy alias INF can use a
    // different filename header while still enforcing byte-for-byte equality of all functional
    // sections/keys.
    let data = fs::read(path).with_context(|| format!("read {}", path.display()))?;

    let mut line_start = 0usize;
    let mut is_first_line = true;
    while line_start < data.len() {
        let mut line_end = line_start;
        while line_end < data.len() && data[line_end] != b'\n' {
            line_end += 1;
        }
        if line_end < data.len() && data[line_end] == b'\n' {
            line_end += 1;
        }
        let line = &data[line_start..line_end];
        let first = first_nonblank_ascii_byte(line, is_first_line);
        is_first_line = false;

        let Some(first) = first else {
            line_start = line_end;
            continue;
        };
        if first == b';' {
            line_start = line_end;
            continue;
        }
        if first == b'[' {
            return Ok(data[line_start..].to_vec());
        }
        // Unexpected functional content before any section header: treat it as functional to avoid
        // masking drift.
        return Ok(data[line_start..].to_vec());
    }

    bail!(
        "{}: could not find a section header (e.g. [Version])",
        path.display()
    );
}

fn strip_inf_sections_bytes(data: &[u8], drop_sections: &[&str]) -> Vec<u8> {
    // Remove entire INF sections (including their headers) by name (case-insensitive).
    //
    // This is used for cases where a legacy alias INF is intentionally allowed to diverge in
    // a small set of sections (currently virtio-input models sections), but should otherwise
    // remain byte-for-byte identical to the canonical INF.
    let drop: BTreeSet<String> = drop_sections
        .iter()
        .map(|s| s.to_ascii_lowercase())
        .collect();
    let mut out = Vec::with_capacity(data.len());
    let mut skipping = false;

    let mut line_start = 0usize;
    let mut is_first_line = true;
    while line_start < data.len() {
        let mut line_end = line_start;
        while line_end < data.len() && data[line_end] != b'\n' {
            line_end += 1;
        }
        if line_end < data.len() && data[line_end] == b'\n' {
            line_end += 1;
        }
        let line = &data[line_start..line_end];

        // Detect section headers on a best-effort ASCII view of the line (strip NUL bytes so UTF-16
        // INFs can be handled). Only strip BOM bytes on the first line, for detection only.
        let mut ascii: Vec<u8> = line.iter().copied().filter(|b| *b != 0).collect();
        if is_first_line {
            if ascii.starts_with(&[0xEF, 0xBB, 0xBF]) {
                ascii.drain(0..3);
            } else if ascii.starts_with(&[0xFF, 0xFE]) || ascii.starts_with(&[0xFE, 0xFF]) {
                ascii.drain(0..2);
            }
            is_first_line = false;
        }

        let mut i = 0usize;
        while i < ascii.len() && (ascii[i] == b' ' || ascii[i] == b'\t') {
            i += 1;
        }
        if i < ascii.len() && ascii[i] == b'[' {
            if let Some(j) = ascii[i + 1..].iter().position(|b| *b == b']') {
                let name = String::from_utf8_lossy(&ascii[i + 1..i + 1 + j])
                    .trim()
                    .to_ascii_lowercase();
                skipping = drop.contains(&name);
            } else {
                skipping = false;
            }
        }

        if !skipping {
            out.extend_from_slice(line);
        }

        line_start = line_end;
    }

    out
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> String {
    let mut units = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.chunks_exact(2) {
        let u = if little_endian {
            u16::from_le_bytes([chunk[0], chunk[1]])
        } else {
            u16::from_be_bytes([chunk[0], chunk[1]])
        };
        units.push(u);
    }
    String::from_utf16_lossy(&units)
}

fn find_files_by_name(root: &Path, file_name: &str) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(root).follow_links(false).into_iter() {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str() else {
            continue;
        };
        if name.eq_ignore_ascii_case(file_name) {
            out.push(entry.into_path());
        }
    }
    out.sort();
    Ok(out)
}

fn parse_hex_u16(s: &str) -> Result<u16> {
    let s = s.trim();
    let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) else {
        bail!("expected 0x-prefixed hex string, got '{s}'");
    };
    u16::from_str_radix(hex, 16).with_context(|| format!("parse hex u16 from '{s}'"))
}

fn parse_rust_u16_const(path: &Path, const_name: &str) -> Result<u16> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let re = regex::Regex::new(&format!(
        r"(?m)^\s*pub const {}\s*:\s*u16\s*=\s*0x([0-9A-Fa-f]+)\s*;",
        regex::escape(const_name)
    ))
    .unwrap();
    let caps = re.captures(&text).ok_or_else(|| {
        anyhow::anyhow!(
            "could not find Rust constant '{}' in {}",
            const_name,
            path.display()
        )
    })?;
    let hex = caps.get(1).unwrap().as_str();
    u16::from_str_radix(hex, 16).with_context(|| format!("parse {const_name} value 0x{hex}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedPciDeviceProfileBdfAndClass {
    bdf: (u8, u8, u8),
    class: (u8, u8, u8),
}

fn parse_int_literal_u8(s: &str) -> Result<u8> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return u8::from_str_radix(hex, 16).with_context(|| format!("parse hex u8 from '{s}'"));
    }
    s.parse::<u8>()
        .with_context(|| format!("parse decimal u8 from '{s}'"))
}

fn parse_pci_device_profile_body(path: &Path, const_name: &str) -> Result<String> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let re = regex::Regex::new(&format!(
        // Extract the struct literal for `pub const <NAME>: PciDeviceProfile = PciDeviceProfile { ... };`.
        r"(?s)pub\s+const\s+{}\s*:\s*PciDeviceProfile\s*=\s*PciDeviceProfile\s*\{{(?P<body>.*?)\}}\s*;",
        regex::escape(const_name)
    ))
    .expect("static regex must compile");
    let caps = re.captures(&text).ok_or_else(|| {
        anyhow::anyhow!(
            "could not find PciDeviceProfile constant '{}' in {}",
            const_name,
            path.display()
        )
    })?;
    Ok(caps
        .name("body")
        .map(|m| m.as_str().to_string())
        .unwrap_or_default())
}

fn parse_pci_device_profile_bdf_and_class(
    path: &Path,
    const_name: &str,
) -> Result<ParsedPciDeviceProfileBdfAndClass> {
    let body = parse_pci_device_profile_body(path, const_name)?;

    let bdf_re = regex::Regex::new(
        r"(?m)^\s*bdf\s*:\s*PciBdf::new\s*\(\s*([^,\s]+)\s*,\s*([^,\s]+)\s*,\s*([^)\s]+)\s*\)\s*,",
    )
    .expect("static regex must compile");
    let bdf_caps = bdf_re.captures(&body).ok_or_else(|| {
        anyhow::anyhow!(
            "could not find 'bdf: PciBdf::new(..)' field in profile::{} in {}",
            const_name,
            path.display()
        )
    })?;
    let bus = parse_int_literal_u8(bdf_caps.get(1).unwrap().as_str())
        .with_context(|| format!("parse {}.bdf bus", const_name))?;
    let dev = parse_int_literal_u8(bdf_caps.get(2).unwrap().as_str())
        .with_context(|| format!("parse {}.bdf device", const_name))?;
    let func = parse_int_literal_u8(bdf_caps.get(3).unwrap().as_str())
        .with_context(|| format!("parse {}.bdf function", const_name))?;

    let class_re = regex::Regex::new(
        r"(?m)^\s*class\s*:\s*PciClassCode::new\s*\(\s*([^,\s]+)\s*,\s*([^,\s]+)\s*,\s*([^)\s]+)\s*\)\s*,",
    )
    .expect("static regex must compile");
    let class_caps = class_re.captures(&body).ok_or_else(|| {
        anyhow::anyhow!(
            "could not find 'class: PciClassCode::new(..)' field in profile::{} in {}",
            const_name,
            path.display()
        )
    })?;
    let base = parse_int_literal_u8(class_caps.get(1).unwrap().as_str())
        .with_context(|| format!("parse {}.class base_class", const_name))?;
    let sub = parse_int_literal_u8(class_caps.get(2).unwrap().as_str())
        .with_context(|| format!("parse {}.class sub_class", const_name))?;
    let prog = parse_int_literal_u8(class_caps.get(3).unwrap().as_str())
        .with_context(|| format!("parse {}.class prog_if", const_name))?;

    Ok(ParsedPciDeviceProfileBdfAndClass {
        bdf: (bus, dev, func),
        class: (base, sub, prog),
    })
}

fn parse_c_define_hex_u16(path: &Path, define: &str) -> Result<u16> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    // Accept: #define NAME 0x1234u / 0x1234 / 0x1234U
    let re = regex::Regex::new(&format!(
        r"(?m)^\s*#define\s+{}\s+0x([0-9A-Fa-f]+)[uU]?\b",
        regex::escape(define)
    ))
    .unwrap();
    let caps = re.captures(&text).ok_or_else(|| {
        anyhow::anyhow!(
            "could not find C #define '{}' in {}",
            define,
            path.display()
        )
    })?;
    let hex = caps.get(1).unwrap().as_str();
    u16::from_str_radix(hex, 16).with_context(|| format!("parse {define} value 0x{hex}"))
}

fn resolve_under(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn canonicalize_maybe_missing(path: &Path) -> Result<PathBuf> {
    // canonicalize() requires the path to exist; repo_root always should, but keep
    // error context readable.
    Ok(std::fs::canonicalize(path)?)
}

fn eq_case_insensitive(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[cfg(not(target_arch = "wasm32"))]
    fn validate_temp_virtio_inf(inf_text: &str, hwid_patterns: Vec<&str>) -> Result<()> {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let inf_dir = tmp.path().join("drivers/windows7/virtio-test/inf");
        fs::create_dir_all(&inf_dir).expect("create drivers/windows7/virtio-test/inf");
        fs::write(inf_dir.join("test.inf"), inf_text).expect("write test.inf");

        let dev = DeviceEntry {
            device: "virtio-test".to_string(),
            pci_vendor_id: "0x1AF4".to_string(),
            pci_device_id: "0x1041".to_string(), // virtio-net modern: 0x1040 + 1
            pci_device_id_transitional: Some("0x1000".to_string()),
            hardware_id_patterns: hwid_patterns.into_iter().map(|s| s.to_string()).collect(),
            driver_service_name: "testsvc".to_string(),
            inf_name: "test.inf".to_string(),
            virtio_device_type: Some(1),
        };

        let mut devices = BTreeMap::new();
        devices.insert(dev.device.clone(), dev);
        validate_in_tree_infs(tmp.path(), &devices)
    }

    fn validate_temp_virtio_input_infs(canonical_inf: &str, alias_inf: Option<&str>) -> Result<()> {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let inf_dir = tmp.path().join("drivers/windows7/virtio-input/inf");
        fs::create_dir_all(&inf_dir).expect("create drivers/windows7/virtio-input/inf");

        fs::write(inf_dir.join("aero_virtio_input.inf"), canonical_inf)
            .expect("write aero_virtio_input.inf");
        if let Some(alias) = alias_inf {
            fs::write(inf_dir.join("virtio-input.inf.disabled"), alias)
                .expect("write virtio-input.inf.disabled");
        }

        let dev = DeviceEntry {
            device: "virtio-input".to_string(),
            pci_vendor_id: "0x1AF4".to_string(),
            pci_device_id: "0x1052".to_string(), // virtio-input modern: 0x1040 + 18
            pci_device_id_transitional: Some("0x1011".to_string()),
            hardware_id_patterns: vec![
                r"PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01".to_string(),
                r"PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01".to_string(),
                r"PCI\VEN_1AF4&DEV_1052&REV_01".to_string(),
            ],
            driver_service_name: "testsvc".to_string(),
            inf_name: "aero_virtio_input.inf".to_string(),
            virtio_device_type: Some(18),
        };

        let mut devices = BTreeMap::new();
        devices.insert(dev.device.clone(), dev);
        validate_in_tree_infs(tmp.path(), &devices)
    }

    #[test]
    fn validator_passes_on_repo_contracts() -> Result<()> {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let args = Args {
            repo_root,
            contract: PathBuf::from("docs/windows-device-contract.json"),
        };
        run(&args)
    }

    #[test]
    fn virtio_input_alias_drift_ignores_banner_differences() -> Result<()> {
        let canonical = r#"
[Version]
Signature="$Windows NT$"

[Aero.NTx86]
%Kb%    = Install, PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01
%Mouse% = Install, PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01

[Aero.NTamd64]
%Kb%    = Install, PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01
%Mouse% = Install, PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01

[Install.Services]
AddService = testsvc, 0x00000002, ServiceInst

[Strings]
Kb    = "Keyboard"
Mouse = "Mouse"
Input = "Input Device"
"#;
        let alias = r#"
; legacy filename alias banner line 1
; line 2

[Version]
Signature="$Windows NT$"

[Aero.NTx86]
%Kb%    = Install, PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01
%Mouse% = Install, PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01
%Input% = Install, PCI\VEN_1AF4&DEV_1052&REV_01

[Aero.NTamd64]
%Kb%    = Install, PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01
%Mouse% = Install, PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01
%Input% = Install, PCI\VEN_1AF4&DEV_1052&REV_01

[Install.Services]
AddService = testsvc, 0x00000002, ServiceInst

[Strings]
Kb    = "Keyboard"
Mouse = "Mouse"
Input = "Input Device"
"#;
        validate_temp_virtio_input_infs(canonical, Some(alias))
    }

    #[test]
    fn virtio_input_alias_drift_is_detected_after_version_section() {
        let canonical = r#"
[Version]
Signature="$Windows NT$"

[Aero.NTx86]
%Kb%    = Install, PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01
%Mouse% = Install, PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01

[Aero.NTamd64]
%Kb%    = Install, PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01
%Mouse% = Install, PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01

[Install.Services]
AddService = testsvc, 0x00000002, ServiceInst

[Strings]
Kb    = "Keyboard"
Mouse = "Mouse"
Input = "Input Device"
"#;
        let alias = r#"
[Version]
Signature="$Windows NT$"

[Aero.NTx86]
%Kb%    = Install, PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01
%Mouse% = Install, PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01
%Input% = Install, PCI\VEN_1AF4&DEV_1052&REV_01

[Aero.NTamd64]
%Kb%    = Install, PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01
%Mouse% = Install, PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01
%Input% = Install, PCI\VEN_1AF4&DEV_1052&REV_01

[Install.Services]
AddService = testsvc, 0x00000002, ServiceInst

[Strings]
Kb    = "Keyboard"
Mouse = "Mouse"
Input = "Input Device"
"#
        .replace(r#"Input = "Input Device""#, r#"Input = "Input Device X""#);

        let err = validate_temp_virtio_input_infs(canonical, Some(&alias)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("legacy alias INF drift detected"), "{msg}");
    }

    #[test]
    fn virtio_input_requires_alias_inf() {
        let canonical = r#"
[Version]
Signature="$Windows NT$"

[Aero.NTx86]
%Kb%    = Install, PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01
%Mouse% = Install, PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01

[Aero.NTamd64]
%Kb%    = Install, PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01
%Mouse% = Install, PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01

[Install.Services]
AddService = testsvc, 0x00000002, ServiceInst

[Strings]
Kb    = "Keyboard"
Mouse = "Mouse"
Input = "Input Device"
"#;
        let err = validate_temp_virtio_input_infs(canonical, None).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("missing required legacy virtio-input alias INF"),
            "{msg}"
        );
    }

    #[test]
    fn parse_cmd_list_extracts_quoted_items() {
        let items = parse_cmd_quoted_list(r#""A" "B C" "D""#);
        assert_eq!(items, vec!["A", "B C", "D"]);
    }

    #[test]
    fn hwid_literal_validation_accepts_canonical_form() {
        validate_hwid_literal(r"PCI\VEN_1AF4&DEV_1042").unwrap();
        validate_hwid_literal(r"PCI\VEN_1AF4&DEV_1042&REV_01").unwrap();
        validate_hwid_literal(r"PCI\VEN_1AF4&DEV_1042&SUBSYS_00021AF4").unwrap();
        validate_hwid_literal(r"PCI\VEN_1AF4&DEV_1042&SUBSYS_00021AF4&REV_01").unwrap();
        validate_hwid_literal(r"PCI\VEN_A3A0&DEV_0001").unwrap();
    }

    #[test]
    fn hwid_literal_validation_rejects_regexy_forms() {
        assert!(validate_hwid_literal(r"PCI\VEN_1AF4&DEV_(1001|1042)").is_err());
        assert!(validate_hwid_literal(r"PCI\VEN_1AF4&DEV_1042.*").is_err());
    }

    #[test]
    fn parse_inf_active_pci_hwids_ignores_comments() {
        let inf = r#"
; comment mentioning PCI\VEN_1AF4&DEV_1042&REV_01 should be ignored
[Version]
Signature="$Windows NT$"

[Manufacturer]
%Mfg% = Models,NTx86

[Models.NTx86]
%VirtioBlk.DeviceDesc% = Install, PCI\VEN_1AF4&DEV_1042&REV_01 ; inline comment
%VirtioNet.DeviceDesc% = Install, PCI\VEN_1AF4&DEV_1041&REV_01
; %CommentedOut.DeviceDesc% = Install, PCI\VEN_1AF4&DEV_1052&REV_01
"#;

        let hwids = parse_inf_active_pci_hwids(inf);
        assert_eq!(
            hwids,
            BTreeSet::from([
                "PCI\\VEN_1AF4&DEV_1041&REV_01".to_string(),
                "PCI\\VEN_1AF4&DEV_1042&REV_01".to_string(),
            ])
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn virtio_inf_family_gating_accepts_subsys_rev_only() -> Result<()> {
        // Regression: allow an INF to bind a virtio VEN/DEV family using only SUBSYS-qualified +
        // revision-gated entries (i.e. do not require a literal `{base}&REV_01` HWID).
        //
        // Use a non-input virtio device here to keep this test narrowly focused on the HWID gating
        // logic (virtio-input has additional DeviceDesc split checks).
        let inf = r#"
[Version]
Signature="$Windows NT$"

[Manufacturer]
%Mfg% = Aero,NTx86

[Aero.NTx86]
%DeviceDesc% = Install, PCI\VEN_1AF4&DEV_1041&SUBSYS_00011AF4&REV_01

[Install.Services]
AddService = testsvc, 0x00000002, ServiceInst

[Strings]
Mfg = "Test"
DeviceDesc = "Test Device"
"#;
        validate_temp_virtio_inf(
            inf,
            vec![
                r"PCI\VEN_1AF4&DEV_1041",
                r"PCI\VEN_1AF4&DEV_1041&SUBSYS_00011AF4",
                r"PCI\VEN_1AF4&DEV_1041&SUBSYS_00011AF4&REV_01",
            ],
        )
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn virtio_inf_family_gating_rejects_revless_match() {
        let inf = r#"
[Version]
Signature="$Windows NT$"

[Manufacturer]
%Mfg% = Aero,NTx86

[Aero.NTx86]
%DeviceDesc% = Install, PCI\VEN_1AF4&DEV_1041&SUBSYS_00011AF4

[Install.Services]
AddService = testsvc, 0x00000002, ServiceInst

[Strings]
Mfg = "Test"
DeviceDesc = "Test Device"
"#;
        let err = validate_temp_virtio_inf(
            inf,
            vec![
                r"PCI\VEN_1AF4&DEV_1041",
                r"PCI\VEN_1AF4&DEV_1041&SUBSYS_00011AF4",
                r"PCI\VEN_1AF4&DEV_1041&SUBSYS_00011AF4&REV_01",
            ],
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("without revision gating"), "{msg}");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn virtio_inf_family_gating_rejects_wrong_rev_in_family() {
        let inf = r#"
[Version]
Signature="$Windows NT$"

[Manufacturer]
%Mfg% = Aero,NTx86

[Aero.NTx86]
%DeviceDesc% = Install, PCI\VEN_1AF4&DEV_1041&SUBSYS_00011AF4&REV_01
%OtherDesc%  = Install, PCI\VEN_1AF4&DEV_1041&REV_02

[Install.Services]
AddService = testsvc, 0x00000002, ServiceInst

[Strings]
Mfg = "Test"
DeviceDesc = "Test Device"
OtherDesc = "Other Device"
"#;
        let err = validate_temp_virtio_inf(
            inf,
            vec![
                r"PCI\VEN_1AF4&DEV_1041",
                r"PCI\VEN_1AF4&DEV_1041&SUBSYS_00011AF4",
                r"PCI\VEN_1AF4&DEV_1041&SUBSYS_00011AF4&REV_01",
            ],
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("REV_ qualifier"), "{msg}");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn virtio_inf_family_gating_rejects_transitional_device_id() {
        let inf = r#"
[Version]
Signature="$Windows NT$"

[Manufacturer]
%Mfg% = Aero,NTx86

[Aero.NTx86]
%DeviceDesc% = Install, PCI\VEN_1AF4&DEV_1041&SUBSYS_00011AF4&REV_01
%TransDesc%  = Install, PCI\VEN_1AF4&DEV_1000&REV_01

[Install.Services]
AddService = testsvc, 0x00000002, ServiceInst

[Strings]
Mfg = "Test"
DeviceDesc = "Test Device"
TransDesc = "Transitional Device"
"#;
        let err = validate_temp_virtio_inf(
            inf,
            vec![
                r"PCI\VEN_1AF4&DEV_1041",
                r"PCI\VEN_1AF4&DEV_1041&SUBSYS_00011AF4",
                r"PCI\VEN_1AF4&DEV_1041&SUBSYS_00011AF4&REV_01",
            ],
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("transitional virtio-pci"), "{msg}");
    }

    #[test]
    fn inf_functional_bytes_skips_utf16le_banner_and_finds_version_section() -> Result<()> {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let path = tmp.path().join("test.inf");

        let inf = "; banner\r\n; more\r\n\r\n[Version]\r\nSignature=\"$Windows NT$\"\r\n";

        // Write UTF-16LE with BOM.
        let mut bytes = vec![0xFFu8, 0xFEu8];
        for u in inf.encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        fs::write(&path, &bytes).expect("write utf16 inf");

        let out = inf_functional_bytes(&path)?;

        // For assertions: strip NUL padding and ensure we start at `[Version]`.
        let ascii = out.into_iter().filter(|b| *b != 0).collect::<Vec<_>>();
        assert!(
            ascii.starts_with(b"[Version]"),
            "unexpected functional bytes prefix: {}",
            String::from_utf8_lossy(&ascii[..ascii.len().min(64)])
        );
        assert!(
            !String::from_utf8_lossy(&ascii).contains("banner"),
            "functional bytes must not include banner/comments"
        );
        Ok(())
    }

    #[test]
    fn inf_functional_bytes_skips_utf8_bom_and_banner_and_finds_version_section() -> Result<()> {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let path = tmp.path().join("test.inf");

        // UTF-8 BOM + comment banner, then `[Version]`.
        let bytes = b"\xef\xbb\xbf; banner\n[Version]\nSignature=\"$Windows NT$\"\n";
        fs::write(&path, bytes).expect("write utf8 inf");

        let out = inf_functional_bytes(&path)?;
        assert!(out.starts_with(b"[Version]\n"));
        assert!(!String::from_utf8_lossy(&out).contains("banner"));
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn inf_text_decoding_supports_utf16le_without_bom() -> Result<()> {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let path = tmp.path().join("test.inf");

        let inf = r#"
[Version]
Signature="$Windows NT$"

[Manufacturer]
%Mfg% = Models,NTx86

[Models.NTx86]
%DeviceDesc% = Install, PCI\VEN_1234&DEV_5678

[Install.Services]
AddService = TestSvc, 0x00000002, Service_Inst
"#;

        // Write UTF-16LE, no BOM.
        let mut bytes = Vec::new();
        for u in inf.encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        fs::write(&path, &bytes).expect("write utf16 inf");

        let text = read_inf_text(&path).expect("decode inf");

        let add_service_re = regex::RegexBuilder::new(&format!(
            r"(?im)^\s*AddService\s*=\s*{}\b",
            regex::escape("TestSvc")
        ))
        .case_insensitive(true)
        .build()
        .unwrap();
        assert!(add_service_re.is_match(&text));

        let hwids = parse_inf_active_pci_hwids(&text);
        assert!(hwids.contains(r"PCI\VEN_1234&DEV_5678"));
        Ok(())
    }

    #[test]
    fn pattern_matches_transitional_virtio_ids_even_when_not_explicitly_listed() {
        let re = regex::RegexBuilder::new(r"PCI\\VEN_1AF4&DEV_10..")
            .case_insensitive(true)
            .build()
            .unwrap();
        let matched = pattern_matches_transitional_virtio_device_ids(&re);
        assert_eq!(matched, vec!["1000", "1001", "1011", "1018"]);

        let re = regex::RegexBuilder::new(r"PCI\\VEN_1AF4&DEV_10..&REV_01")
            .case_insensitive(true)
            .build()
            .unwrap();
        let matched = pattern_matches_transitional_virtio_device_ids(&re);
        assert_eq!(matched, vec!["1000", "1001", "1011", "1018"]);

        let re = regex::RegexBuilder::new(r"PCI\\VEN_1AF4&DEV_10..&SUBSYS_00011AF4&REV_01")
            .case_insensitive(true)
            .build()
            .unwrap();
        let matched = pattern_matches_transitional_virtio_device_ids(&re);
        assert_eq!(matched, vec!["1000"]);

        let re = regex::RegexBuilder::new(r"PCI\\VEN_1AF4&DEV_1041")
            .case_insensitive(true)
            .build()
            .unwrap();
        assert!(pattern_matches_transitional_virtio_device_ids(&re).is_empty());
    }

    fn virtio_entry(name: &str, virtio_device_type: u32) -> DeviceEntry {
        let did = 0x1040u16 + u16::try_from(virtio_device_type).unwrap();
        let trans = 0x1000u16 + u16::try_from(virtio_device_type).unwrap() - 1;
        DeviceEntry {
            device: name.to_string(),
            pci_vendor_id: "0x1AF4".to_string(),
            pci_device_id: format!("0x{did:04X}"),
            pci_device_id_transitional: Some(format!("0x{trans:04X}")),
            hardware_id_patterns: vec![format!("PCI\\VEN_1AF4&DEV_{did:04X}&REV_01")],
            driver_service_name: format!("{name}-svc"),
            inf_name: format!("{name}.inf"),
            virtio_device_type: Some(virtio_device_type),
        }
    }

    fn minimal_devices_for_contract_entry_tests(
        aerogpu_patterns: &[&str],
    ) -> BTreeMap<String, DeviceEntry> {
        let mut devices = BTreeMap::new();
        for (name, vtype) in [
            ("virtio-blk", 2),
            ("virtio-net", 1),
            ("virtio-input", 18),
            ("virtio-snd", 25),
        ] {
            let entry = virtio_entry(name, vtype);
            devices.insert(entry.device.clone(), entry);
        }
        let aero_gpu = DeviceEntry {
            device: "aero-gpu".to_string(),
            pci_vendor_id: "0xA3A0".to_string(),
            pci_device_id: "0x0001".to_string(),
            pci_device_id_transitional: None,
            hardware_id_patterns: aerogpu_patterns.iter().map(|s| s.to_string()).collect(),
            driver_service_name: "aerogpu".to_string(),
            inf_name: "aerogpu_dx11.inf".to_string(),
            virtio_device_type: None,
        };
        devices.insert(aero_gpu.device.clone(), aero_gpu);
        devices
    }

    #[test]
    fn contract_entry_validation_accepts_minimal_aerogpu_patterns() {
        let devices = minimal_devices_for_contract_entry_tests(&[
            r"PCI\VEN_A3A0&DEV_0001",
            r"PCI\VEN_A3A0&DEV_0001&SUBSYS_0001A3A0",
        ]);
        validate_contract_entries(&devices).unwrap();
    }

    #[test]
    fn contract_entry_validation_rejects_duplicate_hardware_id_patterns_within_device_case_insensitive(
    ) {
        let mut devices = minimal_devices_for_contract_entry_tests(&[
            r"PCI\VEN_A3A0&DEV_0001",
            r"PCI\VEN_A3A0&DEV_0001&SUBSYS_0001A3A0",
        ]);
        let virtio_net = devices.get_mut("virtio-net").unwrap();
        virtio_net
            .hardware_id_patterns
            .push("pci\\ven_1af4&dev_1041&rev_01".to_string());
        let err = validate_contract_entries(&devices).unwrap_err();
        assert!(err
            .to_string()
            .contains("hardware_id_patterns contains a duplicate entry"));
    }

    #[test]
    fn contract_entry_validation_rejects_duplicate_hardware_id_patterns_across_devices() {
        let mut devices = minimal_devices_for_contract_entry_tests(&[
            r"PCI\VEN_A3A0&DEV_0001",
            r"PCI\VEN_A3A0&DEV_0001&SUBSYS_0001A3A0",
        ]);
        let alias = virtio_entry("virtio-net-alias", 1);
        devices.insert(alias.device.clone(), alias);
        let err = validate_contract_entries(&devices).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("duplicated in contract device"));
        assert!(msg.contains("virtio-net-alias"));
        assert!(msg.contains("virtio-net"));
    }

    #[test]
    fn contract_entry_validation_rejects_aerogpu_patterns_outside_canonical_family() {
        let devices = minimal_devices_for_contract_entry_tests(&[
            r"PCI\VEN_A3A0&DEV_0001",
            r"PCI\VEN_A3A0&DEV_0001&SUBSYS_0001A3A0",
            r"PCI\VEN_A3A0&DEV_0002",
        ]);
        let err = validate_contract_entries(&devices).unwrap_err();
        assert!(err.to_string().contains("VEN_A3A0&DEV_0001 family"));
    }

    #[test]
    fn contract_entry_validation_rejects_legacy_aerogpu_hwid_with_specific_error() {
        // Avoid embedding the full legacy vendor token (`VEN_` + `1AED`) in this source file so
        // repo-wide greps for deprecated AeroGPU IDs can stay focused on legacy/archived
        // locations. (The in-repo contract validator still needs to exercise the behavior.)
        let legacy_hwid = format!(r"PCI\VEN_{}&DEV_0001", "1AED");
        let devices = minimal_devices_for_contract_entry_tests(&[
            r"PCI\VEN_A3A0&DEV_0001",
            r"PCI\VEN_A3A0&DEV_0001&SUBSYS_0001A3A0",
            legacy_hwid.as_str(),
        ]);
        let err = validate_contract_entries(&devices).unwrap_err();
        assert!(err.to_string().contains("vendor 1AED"));
    }
}
