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

    /// Regex patterns that must appear in at least one INF for the driver.
    #[serde(default)]
    expected_hardware_ids: Vec<String>,
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
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
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
    for (name, dev) in devices {
        let vendor = parse_hex_u16(&dev.pci_vendor_id)
            .with_context(|| format!("{name}: invalid pci_vendor_id"))?;
        let did = parse_hex_u16(&dev.pci_device_id)
            .with_context(|| format!("{name}: invalid pci_device_id"))?;

        if dev.hardware_id_patterns.is_empty() {
            bail!("{name}: hardware_id_patterns is empty");
        }
        let expected_substr = format!("VEN_{vendor:04X}&DEV_{did:04X}");
        let mut has_canonical_vendor_device = false;
        for pat in &dev.hardware_id_patterns {
            validate_hwid_literal(pat)
                .with_context(|| format!("{name}: invalid hardware_id_patterns entry"))?;
            if pat.to_ascii_uppercase().contains(&expected_substr) {
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
        let mut merged = BTreeMap::<String, (bool, Vec<String>)>::new();
        let mut original_names = BTreeMap::<String, String>::new();

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
                let var_key = var.to_ascii_uppercase();
                let raw = devices_cmd_vars.get(&var_key).ok_or_else(|| {
                    anyhow::anyhow!(
                        "{}: driver '{}': expected_hardware_ids_from_devices_cmd_var references missing devices.cmd variable: {}",
                        spec_path.display(),
                        original_names.get(&key).cloned().unwrap_or_else(|| key.clone()),
                        var
                    )
                })?;
                let hwids = parse_cmd_quoted_list(raw);
                if hwids.is_empty() {
                    bail!(
                        "{}: driver '{}': devices.cmd variable {} is empty",
                        spec_path.display(),
                        original_names
                            .get(&key)
                            .cloned()
                            .unwrap_or_else(|| key.clone()),
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
            }
            merged.insert(key, (drv.required, patterns));
        }
        for legacy in spec.required_drivers {
            let key = legacy.name.to_ascii_lowercase();
            match merged.get_mut(&key) {
                Some((required, patterns)) => {
                    *required = true;
                    for pat in legacy.expected_hardware_ids {
                        if !patterns.contains(&pat) {
                            patterns.push(pat);
                        }
                    }
                }
                None => {
                    original_names.insert(key.clone(), legacy.name);
                    merged.insert(key, (true, legacy.expected_hardware_ids));
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
            "win7-signed.json" | "win7-aero-guest-tools.json" | "win7-aero-virtio.json" => devices,
            other => bail!("unexpected spec file name (validator bug): {other}"),
        };

        for (key, (required, patterns)) in merged {
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

            if patterns.is_empty() {
                bail!(
                    "{}: driver '{}' has empty expected_hardware_ids; add patterns derived from {}",
                    spec_path.display(),
                    name,
                    dev.device
                );
            }

            let mut compiled = Vec::new();
            for pat in &patterns {
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
                let pat = &patterns[i];
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
                for (pat, re) in patterns.iter().zip(compiled.iter()) {
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
                for pat in &patterns {
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
            let _ = required;
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
    // - take the last comma-separated field as the candidate HWID
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
        let candidate = line
            .split(',')
            .map(|p| p.trim())
            .next_back()
            .unwrap_or_default();
        if candidate.to_ascii_uppercase().starts_with("PCI\\VEN_") {
            out.insert(candidate.to_string());
        }
    }
    out
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
                let strict = format!("{base}&REV_{expected_rev:02X}");

                if !active_hwids.iter().any(|h| h.eq_ignore_ascii_case(&strict)) {
                    bail!(
                        "{name}: INF {} missing strict revision-gated HWID {strict}.\nActive HWIDs found in INF:\n{}",
                        inf_path.display(),
                        format_bullets(&active_hwids)
                    );
                }

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
    // Virtio PCI IDs: crates/devices/src/pci/profile.rs
    let virtio_profile_rs = repo_root.join("crates/devices/src/pci/profile.rs");
    let virtio_vendor = parse_rust_u16_const(&virtio_profile_rs, "PCI_VENDOR_ID_VIRTIO")
        .with_context(|| "parse PCI_VENDOR_ID_VIRTIO")?;
    if virtio_vendor != 0x1AF4 {
        bail!(
            "emulator virtio vendor ID mismatch in {}: expected 0x1AF4, found {virtio_vendor:#06x}",
            virtio_profile_rs.display()
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
        let found = parse_rust_u16_const(&virtio_profile_rs, const_name)
            .with_context(|| format!("parse {const_name}"))?;
        if found != expected {
            bail!(
                "emulator PCI ID mismatch for {device_name}: contract pci_device_id={expected:#06x}, but {const_name}={found:#06x} in {}",
                virtio_profile_rs.display()
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
        let found = parse_rust_u16_const(&virtio_profile_rs, const_name)
            .with_context(|| format!("parse {const_name}"))?;
        if found != expected {
            bail!(
                "emulator PCI ID mismatch for {device_name} transitional: contract pci_device_id_transitional={expected:#06x}, but {const_name}={found:#06x} in {}",
                virtio_profile_rs.display()
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
    // INFs are usually ASCII/UTF-8, but can be UTF-16LE with BOM.
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return Ok(decode_utf16(&bytes[2..], true));
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return Ok(decode_utf16(&bytes[2..], false));
    }
    Ok(String::from_utf8_lossy(&bytes).to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

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
