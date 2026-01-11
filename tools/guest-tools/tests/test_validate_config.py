#!/usr/bin/env python3

import importlib.util
import io
import json
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path


def _load_validator_module():
    repo_root = Path(__file__).resolve().parents[3]
    validator_path = repo_root / "tools/guest-tools/validate_config.py"
    spec = importlib.util.spec_from_file_location("validate_config", validator_path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Failed to import validator module from: {validator_path}")
    module = importlib.util.module_from_spec(spec)
    # dataclasses (used by validate_config.py) expects the defining module to be
    # present in sys.modules while the class body executes.
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


validate_config = _load_validator_module()


def _contract_device(device_name: str):
    contract_path = validate_config.REPO_ROOT / "docs/windows-device-contract.json"
    contract = validate_config.load_windows_device_contract(contract_path)
    try:
        return contract.devices[device_name]
    except KeyError as e:
        raise AssertionError(f"missing {device_name!r} entry in contract: {contract_path}") from e


def _quote_items(items) -> str:
    return " ".join(f'"{item}"' for item in items)


def _ven_dev_regex_from_hwid(hwid: str) -> str:
    # The contract lists full HWIDs including optional suffixes like SUBSYS/REV. The
    # packaging spec regexes usually match only the vendor/device portion.
    parts = hwid.split("&")
    if len(parts) < 2:
        raise AssertionError(f"unexpected HWID format: {hwid!r}")
    base = "&".join(parts[:2])
    return base.replace("\\", "\\\\")


class ValidateConfigTests(unittest.TestCase):
    def test_parse_quoted_list(self) -> None:
        self.assertEqual(validate_config._parse_quoted_list('"A" "B"'), ("A", "B"))
        self.assertEqual(validate_config._parse_quoted_list("A"), ("A",))
        self.assertEqual(validate_config._parse_quoted_list(""), ())

    def test_optional_validation_only_when_driver_declared(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-validate-config-") as tmp:
            tmp_path = Path(tmp)
            devices_cmd = tmp_path / "devices.cmd"
            virtio_blk = _contract_device("virtio-blk")
            virtio_net = _contract_device("virtio-net")
            devices_cmd.write_text(
                "\n".join(
                    [
                        f'set "AERO_VIRTIO_BLK_SERVICE={virtio_blk.driver_service_name}"',
                        f"set AERO_VIRTIO_BLK_HWIDS={_quote_items(virtio_blk.hardware_id_patterns)}",
                        f"set AERO_VIRTIO_NET_HWIDS={_quote_items(virtio_net.hardware_id_patterns)}",
                    ]
                ),
                encoding="utf-8",
            )

            # Spec only declares required drivers; missing optional HWID lists in devices.cmd
            # should not trigger any errors.
            spec_path = tmp_path / "spec.json"
            spec_path.write_text(
                json.dumps(
                    {
                        "drivers": [
                            {
                                "name": "viostor",
                                "required": True,
                                "expected_hardware_ids": [_ven_dev_regex_from_hwid(virtio_blk.hardware_id_patterns[0])],
                            },
                            {
                                "name": "netkvm",
                                "required": True,
                                "expected_hardware_ids": [_ven_dev_regex_from_hwid(virtio_net.hardware_id_patterns[0])],
                            },
                        ]
                    }
                ),
                encoding="utf-8",
            )

            devices = validate_config.load_devices_cmd(devices_cmd)
            expected = validate_config.load_packaging_spec(spec_path)
            with redirect_stdout(io.StringIO()):
                validate_config.validate(devices, spec_path, expected)

    def test_optional_driver_requires_devices_cmd_hwid_list(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-validate-config-") as tmp:
            tmp_path = Path(tmp)
            devices_cmd = tmp_path / "devices.cmd"
            virtio_blk = _contract_device("virtio-blk")
            virtio_net = _contract_device("virtio-net")
            devices_cmd.write_text(
                "\n".join(
                    [
                        f'set "AERO_VIRTIO_BLK_SERVICE={virtio_blk.driver_service_name}"',
                        f"set AERO_VIRTIO_BLK_HWIDS={_quote_items(virtio_blk.hardware_id_patterns)}",
                        f"set AERO_VIRTIO_NET_HWIDS={_quote_items(virtio_net.hardware_id_patterns)}",
                    ]
                ),
                encoding="utf-8",
            )

            spec_path = tmp_path / "spec.json"
            spec_path.write_text(
                json.dumps(
                    {
                        "drivers": [
                            {
                                "name": "viostor",
                                "required": True,
                                "expected_hardware_ids": [_ven_dev_regex_from_hwid(virtio_blk.hardware_id_patterns[0])],
                            },
                            {
                                "name": "netkvm",
                                "required": True,
                                "expected_hardware_ids": [_ven_dev_regex_from_hwid(virtio_net.hardware_id_patterns[0])],
                            },
                            {
                                "name": "vioinput",
                                "required": False,
                                "expected_hardware_ids": [r"PCI\\VEN_1AF4&DEV_1011"],
                            },
                        ]
                    }
                ),
                encoding="utf-8",
            )

            devices = validate_config.load_devices_cmd(devices_cmd)
            expected = validate_config.load_packaging_spec(spec_path)

            with self.assertRaises(validate_config.ValidationError) as ctx:
                with redirect_stdout(io.StringIO()):
                    validate_config.validate(devices, spec_path, expected)

            self.assertIn("AERO_VIRTIO_INPUT_HWIDS", str(ctx.exception))

    def test_regex_matching_is_case_insensitive(self) -> None:
        # Spec regexes should match HWIDs regardless of case.
        match = validate_config._find_first_match(
            patterns=[r"pci\\ven_1af4&dev_1041"],
            hwids=["PCI\\VEN_1AF4&DEV_1041"],
        )
        self.assertIsNotNone(match)

    def test_aerogpu_driver_name_alias_is_normalized(self) -> None:
        # Guest Tools historically used `aero-gpu` as the AeroGPU driver directory name.
        # Validate that the spec validator normalizes the legacy dashed form to the
        # canonical `aerogpu` name.
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-validate-config-") as tmp:
            tmp_path = Path(tmp)
            devices_cmd = tmp_path / "devices.cmd"
            virtio_blk = _contract_device("virtio-blk")
            virtio_net = _contract_device("virtio-net")
            aerogpu = _contract_device("aero-gpu")
            devices_cmd.write_text(
                "\n".join(
                    [
                        f'set "AERO_VIRTIO_BLK_SERVICE={virtio_blk.driver_service_name}"',
                        f"set AERO_VIRTIO_BLK_HWIDS={_quote_items(virtio_blk.hardware_id_patterns)}",
                        f"set AERO_VIRTIO_NET_HWIDS={_quote_items(virtio_net.hardware_id_patterns)}",
                        f"set AERO_GPU_HWIDS={_quote_items(aerogpu.hardware_id_patterns)}",
                    ]
                ),
                encoding="utf-8",
            )

            spec_path = tmp_path / "spec.json"
            spec_path.write_text(
                json.dumps(
                    {
                        "drivers": [
                            {
                                "name": "aero-gpu",
                                "required": True,
                                "expected_hardware_ids": [],
                                "expected_hardware_ids_from_devices_cmd_var": "AERO_GPU_HWIDS",
                            }
                        ]
                    }
                ),
                encoding="utf-8",
            )

            devices = validate_config.load_devices_cmd(devices_cmd)
            expected = validate_config.load_packaging_spec(spec_path)
            self.assertIn("aerogpu", expected)
            self.assertNotIn("aero-gpu", expected)

            with redirect_stdout(io.StringIO()):
                validate_config.validate(devices, spec_path, expected)

    def test_win7_signed_spec_allows_empty_expected_hwid_patterns(self) -> None:
        # `win7-signed.json` intentionally does not pin HWIDs. The validator should
        # still accept it (after enforcing required driver entries and the
        # boot-critical contract checks).
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-validate-config-") as tmp:
            tmp_path = Path(tmp)
            devices_cmd = tmp_path / "devices.cmd"

            virtio_blk = _contract_device("virtio-blk")
            virtio_net = _contract_device("virtio-net")
            virtio_input = _contract_device("virtio-input")
            virtio_snd = _contract_device("virtio-snd")
            aerogpu = _contract_device("aero-gpu")

            devices_cmd.write_text(
                "\n".join(
                    [
                        f'set "AERO_VIRTIO_BLK_SERVICE={virtio_blk.driver_service_name}"',
                        f"set AERO_VIRTIO_BLK_HWIDS={_quote_items(virtio_blk.hardware_id_patterns)}",
                        f"set AERO_VIRTIO_NET_HWIDS={_quote_items(virtio_net.hardware_id_patterns)}",
                        f"set AERO_VIRTIO_INPUT_HWIDS={_quote_items(virtio_input.hardware_id_patterns)}",
                        f"set AERO_VIRTIO_SND_HWIDS={_quote_items(virtio_snd.hardware_id_patterns)}",
                        f"set AERO_GPU_HWIDS={_quote_items(aerogpu.hardware_id_patterns)}",
                    ]
                ),
                encoding="utf-8",
            )

            spec_path = tmp_path / "win7-signed.json"
            spec_path.write_text(
                json.dumps(
                    {
                        "drivers": [
                            {"name": "aerogpu", "required": True, "expected_hardware_ids": []},
                            {"name": "virtio-blk", "required": True, "expected_hardware_ids": []},
                            {"name": "virtio-net", "required": True, "expected_hardware_ids": []},
                            {"name": "virtio-input", "required": True, "expected_hardware_ids": []},
                            {"name": "virtio-snd", "required": False, "expected_hardware_ids": []},
                        ]
                    }
                ),
                encoding="utf-8",
            )

            devices = validate_config.load_devices_cmd(devices_cmd)
            expected = validate_config.load_packaging_spec(spec_path)
            with redirect_stdout(io.StringIO()):
                validate_config.validate(devices, spec_path, expected)

    def test_windows_device_contract_override_supports_virtio_win_services(self) -> None:
        # The in-repo Guest Tools config uses Aero in-tree service names (aero_virtio_blk, etc), but
        # virtio-win Guest Tools packaging needs to validate against upstream service names
        # (viostor/netkvm). The validator should support selecting the contract variant.
        virtio_contract_path = validate_config.REPO_ROOT / "docs/windows-device-contract-virtio-win.json"
        virtio_contract = validate_config.load_windows_device_contract(virtio_contract_path)
        virtio_blk = virtio_contract.devices["virtio-blk"]
        virtio_net = virtio_contract.devices["virtio-net"]

        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-validate-config-") as tmp:
            tmp_path = Path(tmp)
            devices_cmd = tmp_path / "devices.cmd"
            devices_cmd.write_text(
                "\n".join(
                    [
                        f'set "AERO_VIRTIO_BLK_SERVICE={virtio_blk.driver_service_name}"',
                        f"set AERO_VIRTIO_BLK_HWIDS={_quote_items(virtio_blk.hardware_id_patterns)}",
                        f"set AERO_VIRTIO_NET_HWIDS={_quote_items(virtio_net.hardware_id_patterns)}",
                    ]
                ),
                encoding="utf-8",
            )

            spec_path = tmp_path / "win7-virtio-win.json"
            spec_path.write_text(
                json.dumps(
                    {
                        "drivers": [
                            {
                                "name": "viostor",
                                "required": True,
                                "expected_hardware_ids": [_ven_dev_regex_from_hwid(virtio_blk.hardware_id_patterns[0])],
                            },
                            {
                                "name": "netkvm",
                                "required": True,
                                "expected_hardware_ids": [_ven_dev_regex_from_hwid(virtio_net.hardware_id_patterns[0])],
                            },
                        ]
                    }
                ),
                encoding="utf-8",
            )

            devices = validate_config.load_devices_cmd(devices_cmd)
            expected = validate_config.load_packaging_spec(spec_path)

            # Default contract is the canonical Aero one, which should reject viostor.
            with self.assertRaises(validate_config.ValidationError):
                with redirect_stdout(io.StringIO()):
                    validate_config.validate(devices, spec_path, expected)

            # But the virtio-win contract should accept it.
            with redirect_stdout(io.StringIO()):
                validate_config.validate(
                    devices,
                    spec_path,
                    expected,
                    windows_device_contract=virtio_contract_path,
                )

    def test_windows_device_contract_rejects_transitional_virtio_device_ids(self) -> None:
        # The machine-readable device contract is expected to track AERO-W7-VIRTIO v1, which is
        # modern-only (0x1040+ device IDs) and revision-gated (REV_01).
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-validate-config-") as tmp:
            tmp_path = Path(tmp)
            contract_path = tmp_path / "contract.json"
            contract_path.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "contract_name": "test",
                        "contract_version": "0.0.0",
                        "devices": [
                            {
                                "device": "virtio-net",
                                "pci_vendor_id": "0x1AF4",
                                "pci_device_id": "0x1000",
                                "hardware_id_patterns": ["PCI\\VEN_1AF4&DEV_1000&REV_01"],
                                "driver_service_name": "svc",
                                "inf_name": "x.inf",
                                "virtio_device_type": 1,
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )

            with self.assertRaises(validate_config.ValidationError) as ctx:
                validate_config.load_windows_device_contract(contract_path)

            self.assertIn("0x1040 + virtio_device_type", str(ctx.exception))

    def test_aero_spec_rejects_transitional_virtio_ids(self) -> None:
        # Aero virtio contract v1 is modern-only, so we intentionally reject transitional
        # virtio-pci IDs in the Aero packaging spec.
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-validate-config-") as tmp:
            tmp_path = Path(tmp)
            devices_cmd = tmp_path / "devices.cmd"
            virtio_blk = _contract_device("virtio-blk")
            virtio_net = _contract_device("virtio-net")
            virtio_input = _contract_device("virtio-input")
            aerogpu = _contract_device("aero-gpu")

            devices_cmd.write_text(
                "\n".join(
                    [
                        f'set "AERO_VIRTIO_BLK_SERVICE={virtio_blk.driver_service_name}"',
                        f"set AERO_VIRTIO_BLK_HWIDS={_quote_items(virtio_blk.hardware_id_patterns)}",
                        f"set AERO_VIRTIO_NET_HWIDS={_quote_items(virtio_net.hardware_id_patterns)}",
                        f"set AERO_VIRTIO_INPUT_HWIDS={_quote_items(virtio_input.hardware_id_patterns)}",
                        f"set AERO_GPU_HWIDS={_quote_items(aerogpu.hardware_id_patterns)}",
                    ]
                ),
                encoding="utf-8",
            )

            spec_path = tmp_path / "win7-aero-guest-tools.json"
            spec_path.write_text(
                json.dumps(
                    {
                        "drivers": [
                            {
                                "name": "aerogpu",
                                "required": True,
                                "expected_hardware_ids": [],
                                "expected_hardware_ids_from_devices_cmd_var": "AERO_GPU_HWIDS",
                            },
                            {
                                "name": "virtio-blk",
                                "required": True,
                                "expected_hardware_ids": [_ven_dev_regex_from_hwid(virtio_blk.hardware_id_patterns[0])],
                            },
                            {
                                "name": "virtio-net",
                                "required": True,
                                "expected_hardware_ids": [r"PCI\\VEN_1AF4&DEV_(1000|1041)"],
                            },
                            {
                                "name": "virtio-input",
                                "required": True,
                                "expected_hardware_ids": [_ven_dev_regex_from_hwid(virtio_input.hardware_id_patterns[0])],
                            },
                        ]
                    }
                ),
                encoding="utf-8",
            )

            devices = validate_config.load_devices_cmd(devices_cmd)
            expected = validate_config.load_packaging_spec(spec_path)
            with self.assertRaises(validate_config.ValidationError) as ctx:
                with redirect_stdout(io.StringIO()):
                    validate_config.validate(devices, spec_path, expected)

            self.assertIn("transitional virtio pci ids", str(ctx.exception).lower())
            self.assertIn("1AF4:1000", str(ctx.exception))

    def test_aero_spec_rejects_regex_matching_transitional_ids(self) -> None:
        # Transitional-ID rejection should apply even when the regex does not explicitly list
        # the transitional DEV_XXXX (e.g. `DEV_10..`).
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-validate-config-") as tmp:
            tmp_path = Path(tmp)
            devices_cmd = tmp_path / "devices.cmd"
            virtio_blk = _contract_device("virtio-blk")
            virtio_net = _contract_device("virtio-net")
            virtio_input = _contract_device("virtio-input")
            aerogpu = _contract_device("aero-gpu")

            devices_cmd.write_text(
                "\n".join(
                    [
                        f'set "AERO_VIRTIO_BLK_SERVICE={virtio_blk.driver_service_name}"',
                        f"set AERO_VIRTIO_BLK_HWIDS={_quote_items(virtio_blk.hardware_id_patterns)}",
                        f"set AERO_VIRTIO_NET_HWIDS={_quote_items(virtio_net.hardware_id_patterns)}",
                        f"set AERO_VIRTIO_INPUT_HWIDS={_quote_items(virtio_input.hardware_id_patterns)}",
                        f"set AERO_GPU_HWIDS={_quote_items(aerogpu.hardware_id_patterns)}",
                    ]
                ),
                encoding="utf-8",
            )

            spec_path = tmp_path / "win7-aero-guest-tools.json"
            spec_path.write_text(
                json.dumps(
                    {
                        "drivers": [
                            {
                                "name": "aerogpu",
                                "required": True,
                                "expected_hardware_ids": [],
                                "expected_hardware_ids_from_devices_cmd_var": "AERO_GPU_HWIDS",
                            },
                            {
                                "name": "virtio-blk",
                                "required": True,
                                "expected_hardware_ids": [_ven_dev_regex_from_hwid(virtio_blk.hardware_id_patterns[0])],
                            },
                            {
                                "name": "virtio-net",
                                "required": True,
                                # Matches both modern (1041) and transitional (1000) -> must be rejected.
                                "expected_hardware_ids": [r"PCI\\VEN_1AF4&DEV_10.."],
                            },
                            {
                                "name": "virtio-input",
                                "required": True,
                                "expected_hardware_ids": [_ven_dev_regex_from_hwid(virtio_input.hardware_id_patterns[0])],
                            },
                        ]
                    }
                ),
                encoding="utf-8",
            )

            devices = validate_config.load_devices_cmd(devices_cmd)
            expected = validate_config.load_packaging_spec(spec_path)
            with self.assertRaises(validate_config.ValidationError) as ctx:
                with redirect_stdout(io.StringIO()):
                    validate_config.validate(devices, spec_path, expected)

            self.assertIn("transitional virtio pci ids", str(ctx.exception).lower())
            self.assertIn("1AF4:1000", str(ctx.exception))

    def test_aero_spec_rejects_regex_matching_transitional_ids_with_rev(self) -> None:
        # Some regressions may reintroduce broad regexes that include `&REV_01`. Ensure we still
        # detect them as allowing transitional IDs.
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-validate-config-") as tmp:
            tmp_path = Path(tmp)
            devices_cmd = tmp_path / "devices.cmd"
            virtio_blk = _contract_device("virtio-blk")
            virtio_net = _contract_device("virtio-net")
            virtio_input = _contract_device("virtio-input")
            aerogpu = _contract_device("aero-gpu")

            devices_cmd.write_text(
                "\n".join(
                    [
                        f'set "AERO_VIRTIO_BLK_SERVICE={virtio_blk.driver_service_name}"',
                        f"set AERO_VIRTIO_BLK_HWIDS={_quote_items(virtio_blk.hardware_id_patterns)}",
                        f"set AERO_VIRTIO_NET_HWIDS={_quote_items(virtio_net.hardware_id_patterns)}",
                        f"set AERO_VIRTIO_INPUT_HWIDS={_quote_items(virtio_input.hardware_id_patterns)}",
                        f"set AERO_GPU_HWIDS={_quote_items(aerogpu.hardware_id_patterns)}",
                    ]
                ),
                encoding="utf-8",
            )

            spec_path = tmp_path / "win7-aero-guest-tools.json"
            spec_path.write_text(
                json.dumps(
                    {
                        "drivers": [
                            {
                                "name": "aerogpu",
                                "required": True,
                                "expected_hardware_ids": [],
                                "expected_hardware_ids_from_devices_cmd_var": "AERO_GPU_HWIDS",
                            },
                            {
                                "name": "virtio-blk",
                                "required": True,
                                "expected_hardware_ids": [_ven_dev_regex_from_hwid(virtio_blk.hardware_id_patterns[0])],
                            },
                            {
                                "name": "virtio-net",
                                "required": True,
                                # Matches both modern (1041) and transitional (1000) when REV_01 is present -> must be rejected.
                                "expected_hardware_ids": [r"PCI\\VEN_1AF4&DEV_10..&REV_01"],
                            },
                            {
                                "name": "virtio-input",
                                "required": True,
                                "expected_hardware_ids": [_ven_dev_regex_from_hwid(virtio_input.hardware_id_patterns[0])],
                            },
                        ]
                    }
                ),
                encoding="utf-8",
            )

            devices = validate_config.load_devices_cmd(devices_cmd)
            expected = validate_config.load_packaging_spec(spec_path)
            with self.assertRaises(validate_config.ValidationError) as ctx:
                with redirect_stdout(io.StringIO()):
                    validate_config.validate(devices, spec_path, expected)

            self.assertIn("transitional virtio pci ids", str(ctx.exception).lower())
            self.assertIn("1AF4:1000", str(ctx.exception))

    def test_virtio_win_spec_rejects_transitional_virtio_ids(self) -> None:
        # The virtio-win packaging specs used by Aero are also modern-only: transitional virtio-pci
        # IDs must not be accepted.
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-validate-config-") as tmp:
            tmp_path = Path(tmp)
            devices_cmd = tmp_path / "devices.cmd"
            virtio_blk = _contract_device("virtio-blk")
            virtio_net = _contract_device("virtio-net")

            devices_cmd.write_text(
                "\n".join(
                    [
                        f'set "AERO_VIRTIO_BLK_SERVICE={virtio_blk.driver_service_name}"',
                        f"set AERO_VIRTIO_BLK_HWIDS={_quote_items(virtio_blk.hardware_id_patterns)}",
                        f"set AERO_VIRTIO_NET_HWIDS={_quote_items(virtio_net.hardware_id_patterns)}",
                    ]
                ),
                encoding="utf-8",
            )

            spec_path = tmp_path / "win7-virtio-win.json"
            spec_path.write_text(
                json.dumps(
                    {
                        "drivers": [
                            {
                                "name": "viostor",
                                "required": True,
                                "expected_hardware_ids": [_ven_dev_regex_from_hwid(virtio_blk.hardware_id_patterns[0])],
                            },
                            {
                                "name": "netkvm",
                                "required": True,
                                "expected_hardware_ids": [r"PCI\\VEN_1AF4&DEV_(1000|1041)"],
                            },
                        ]
                    }
                ),
                encoding="utf-8",
            )

            devices = validate_config.load_devices_cmd(devices_cmd)
            expected = validate_config.load_packaging_spec(spec_path)
            with self.assertRaises(validate_config.ValidationError) as ctx:
                with redirect_stdout(io.StringIO()):
                    validate_config.validate(devices, spec_path, expected)

            self.assertIn("transitional virtio pci ids", str(ctx.exception).lower())
            self.assertIn("1AF4:1000", str(ctx.exception))

    def test_aero_guest_tools_spec_rejects_transitional_virtio_input_id(self) -> None:
        # Aero-facing specs must not allow transitional virtio-pci IDs for virtio-input.
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-validate-config-") as tmp:
            tmp_path = Path(tmp)
            devices_cmd = tmp_path / "devices.cmd"
            virtio_blk = _contract_device("virtio-blk")
            virtio_net = _contract_device("virtio-net")
            virtio_input = _contract_device("virtio-input")
            aerogpu = _contract_device("aero-gpu")

            devices_cmd.write_text(
                "\n".join(
                    [
                        f'set "AERO_VIRTIO_BLK_SERVICE={virtio_blk.driver_service_name}"',
                        f"set AERO_VIRTIO_BLK_HWIDS={_quote_items(virtio_blk.hardware_id_patterns)}",
                        f"set AERO_VIRTIO_NET_HWIDS={_quote_items(virtio_net.hardware_id_patterns)}",
                        f"set AERO_VIRTIO_INPUT_HWIDS={_quote_items(virtio_input.hardware_id_patterns)}",
                        f"set AERO_GPU_HWIDS={_quote_items(aerogpu.hardware_id_patterns)}",
                    ]
                ),
                encoding="utf-8",
            )

            spec_path = tmp_path / "win7-aero-guest-tools.json"
            spec_path.write_text(
                json.dumps(
                    {
                        "drivers": [
                            {
                                "name": "aerogpu",
                                "required": True,
                                "expected_hardware_ids": [_ven_dev_regex_from_hwid(aerogpu.hardware_id_patterns[0])],
                            },
                            {
                                "name": "virtio-blk",
                                "required": True,
                                "expected_hardware_ids": [_ven_dev_regex_from_hwid(virtio_blk.hardware_id_patterns[0])],
                            },
                            {
                                "name": "virtio-net",
                                "required": True,
                                "expected_hardware_ids": [_ven_dev_regex_from_hwid(virtio_net.hardware_id_patterns[0])],
                            },
                            {
                                "name": "virtio-input",
                                "required": True,
                                # Transitional ID (1011) must not be accepted by Aero-facing specs.
                                "expected_hardware_ids": [r"PCI\\VEN_1AF4&DEV_(1011|1052)"],
                            },
                        ]
                    }
                ),
                encoding="utf-8",
            )

            devices = validate_config.load_devices_cmd(devices_cmd)
            expected = validate_config.load_packaging_spec(spec_path)
            with self.assertRaises(validate_config.ValidationError) as ctx:
                with redirect_stdout(io.StringIO()):
                    validate_config.validate(devices, spec_path, expected)

            self.assertIn("1AF4:1011", str(ctx.exception))

    def test_virtio_full_spec_rejects_transitional_virtio_snd_id(self) -> None:
        # Upstream virtio-win packaging (full profile) is also Aero-facing: it must not allow
        # transitional virtio-snd IDs.
        with tempfile.TemporaryDirectory(prefix="aero-guest-tools-validate-config-") as tmp:
            tmp_path = Path(tmp)
            devices_cmd = tmp_path / "devices.cmd"
            virtio_blk = _contract_device("virtio-blk")
            virtio_net = _contract_device("virtio-net")
            virtio_input = _contract_device("virtio-input")
            virtio_snd = _contract_device("virtio-snd")

            devices_cmd.write_text(
                "\n".join(
                    [
                        f'set "AERO_VIRTIO_BLK_SERVICE={virtio_blk.driver_service_name}"',
                        f"set AERO_VIRTIO_BLK_HWIDS={_quote_items(virtio_blk.hardware_id_patterns)}",
                        f"set AERO_VIRTIO_NET_HWIDS={_quote_items(virtio_net.hardware_id_patterns)}",
                        f"set AERO_VIRTIO_INPUT_HWIDS={_quote_items(virtio_input.hardware_id_patterns)}",
                        f"set AERO_VIRTIO_SND_HWIDS={_quote_items(virtio_snd.hardware_id_patterns)}",
                    ]
                ),
                encoding="utf-8",
            )

            spec_path = tmp_path / "win7-virtio-full.json"
            spec_path.write_text(
                json.dumps(
                    {
                        "drivers": [
                            {
                                "name": "viostor",
                                "required": True,
                                "expected_hardware_ids": [_ven_dev_regex_from_hwid(virtio_blk.hardware_id_patterns[0])],
                            },
                            {
                                "name": "netkvm",
                                "required": True,
                                "expected_hardware_ids": [_ven_dev_regex_from_hwid(virtio_net.hardware_id_patterns[0])],
                            },
                            {
                                "name": "viosnd",
                                "required": False,
                                # Transitional ID (1018) must not be accepted by Aero-facing specs.
                                "expected_hardware_ids": [r"PCI\\VEN_1AF4&DEV_(1018|1059)"],
                            },
                        ]
                    }
                ),
                encoding="utf-8",
            )

            devices = validate_config.load_devices_cmd(devices_cmd)
            expected = validate_config.load_packaging_spec(spec_path)
            with self.assertRaises(validate_config.ValidationError) as ctx:
                with redirect_stdout(io.StringIO()):
                    validate_config.validate(devices, spec_path, expected)

            self.assertIn("1AF4:1018", str(ctx.exception))

if __name__ == "__main__":
    unittest.main()
