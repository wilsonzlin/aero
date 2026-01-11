# WebHID → HID report descriptor synthesis (Windows 7 contract)

## Background / why this exists

When we use the browser’s **WebHID** API to talk to a physical HID device, we do **not** get access to the device’s raw HID **report descriptor bytes**.

Instead, WebHID exposes a structured view of the descriptor:

- `HIDDevice.collections` → a tree of `HIDCollectionInfo`
- Each `HIDCollectionInfo` has `inputReports` / `outputReports` / `featureReports`
- Each `HIDReportInfo` has a `reportId` and a list of `HIDReportItem`s

To present that physical device to the Windows 7 guest as a USB HID device (or to run any code that expects descriptor bytes), we synthesize a **semantically equivalent** HID report descriptor from the WebHID metadata.

This document defines the **contract** for that synthesis:

- how the WebHID data model maps onto HID report descriptor items
- how main-item flags are derived
- the encoding rules we follow (short items, minimal-size payloads, signed encoding)
- validation rules and known limitations (including ordering loss)

Windows 7 note: the output is intentionally “boring HID 1.11” to maximize compatibility with `hidclass.sys` / `hidparse.sys` on Windows 7.

Implementation references:

- Browser normalization: `web/src/hid/webhid_normalize.ts`
- Rust synthesis: `crates/emulator/src/io/usb/hid/webhid.rs`

For the end-to-end “real device” passthrough architecture (main thread owns the
`HIDDevice`, worker models UHCI + a generic HID device), see
[`docs/webhid-webusb-passthrough.md`](./webhid-webusb-passthrough.md).

---

## High-level algorithm (deterministic descriptor emission)

We emit a descriptor by walking the WebHID collection tree.

For each `HIDCollectionInfo`:

1. Emit the collection “header” items:
   - `Usage Page` (from `usagePage`)
   - `Usage` (from `usage`)
   - `Collection(type)` (from `type`)
2. Inside the collection, emit the report definitions in a deterministic grouping:
   - all `inputReports`, then all `outputReports`, then all `featureReports`
   - within a given `HIDReportInfo`, preserve the order of `items` (this defines bit/field layout)
3. Recurse into `children` (depth-first), then emit `End Collection`.

Because WebHID does not expose the original descriptor byte stream, this grouping may not match the device’s original interleaving of “report items vs child collections”. See [Known limitations](#known-limitations).

---

## Data model → report descriptor mapping

### Collections (`HIDCollectionInfo`)

`HIDCollectionInfo.usagePage/usage/type/children` maps to:

```
Usage Page (usagePage)
Usage (usage)
Collection (type)
  …contents…
End Collection
```

Notes:

- `type` is emitted as the 1-byte collection type value used by the HID specification (e.g. `Application`, `Physical`, …).
- We **do not** emit `Push`/`Pop`; each report item is emitted with the global state it needs (see below).

Collection type codes (as used by the WebHID normalization layer and by HID):

| WebHID `type` | `Collection(...)` byte |
| --- | ---: |
| `physical` | `0x00` |
| `application` | `0x01` |
| `logical` | `0x02` |
| `report` | `0x03` |
| `namedArray` | `0x04` |
| `usageSwitch` | `0x05` |
| `usageModifier` | `0x06` |

### Reports (`HIDReportInfo`)

Each WebHID report group (`inputReports` / `outputReports` / `featureReports`) maps to a sequence of main items inside the current collection.

#### Report ID

`HIDReportInfo.reportId` maps to the global `Report ID` item:

- `reportId == 0`: **omit** `Report ID` entirely (descriptor has no report IDs; report bytes have no leading report-id byte).
- `reportId != 0`: emit `Report ID (reportId)` before the first main item of that report.

See [Validation rules](#validation-rules) for the “mixed 0/non-zero” policy.

### Report items (`HIDReportItem`)

Each `HIDReportItem` maps to:

1. A set of **global** items (apply to the next main item):
    - `Usage Page` (`item.usagePage`)
    - `Report Size` (`item.reportSize`)
    - `Report Count` (`item.reportCount`)
    - `Logical Minimum` / `Logical Maximum` (`item.logicalMinimum` / `item.logicalMaximum`)
    - `Physical Minimum` / `Physical Maximum` (`item.physicalMinimum` / `item.physicalMaximum`)
    - `Unit Exponent` (`item.unitExponent`)
    - `Unit` (`item.unit`)
2. A set of **local** items (consumed by the next main item only):
    - either a `Usage` list (`item.usages`)
    - or `Usage Minimum` / `Usage Maximum` (`item.usageMinimum` / `item.usageMaximum`)
3. The **main item** itself:
    - `Input(flags)` for input reports
    - `Output(flags)` for output reports
    - `Feature(flags)` for feature reports

We treat the WebHID `HIDReportInfo` that the item came from as the authoritative “main item kind” (`Input` vs `Output` vs `Feature`).

WebHID also exposes less-common HID locals (`strings`, `designators`) and related min/max fields. These are currently ignored by synthesis (see [Known limitations](#known-limitations)).

---

## Main item flags

HID main items (`Input`/`Output`/`Feature`) have a bitfield payload.

The synthesis treats these flags as a single `u16` and emits either a 1-byte or 2-byte payload:

- if `flags <= 0xFF`: emit 1 byte
- otherwise: emit 2 bytes (little-endian)

Implementation note: the current Rust synthesizer (`crates/emulator/src/io/usb/hid/webhid.rs`) packs main-item flags into a single `u16` and reuses the same encoding for `Input`, `Output`, and `Feature`. This mostly matches common HID descriptors, but it is not a perfect reconstruction in the rare case where an **Input** item uses the “Buffered Bytes” form (see below).

### Bit layout (LSB = bit 0)

Bits are defined by the HID specification as:

| Bit | Meaning when 0 | Meaning when 1 |
| --- | --- | --- |
| 0 | Data | Constant |
| 1 | Array | Variable |
| 2 | Absolute | Relative |
| 3 | No Wrap | Wrap |
| 4 | Linear | Non Linear |
| 5 | Preferred State | No Preferred |
| 6 | No Null Position | Null State |
| 7 (Input) | Bitfield | Buffered Bytes |
| 7 (Output/Feature) | Non Volatile | Volatile |
| 8 (Output/Feature) | Bitfield | Buffered Bytes |

### Derivation from WebHID booleans

The current synthesis output uses the following mapping (matching `webhid.rs`):

| WebHID property | HID bit |
| --- | ---: |
| `isConstant` | 0 |
| `isArray` (inverted: `!isArray` means Variable) | 1 |
| `isRelative` (or equivalently `!isAbsolute`) | 2 |
| `isWrapped` | 3 |
| `isLinear` (inverted: `!isLinear` means Non Linear) | 4 |
| `hasPreferredState` (inverted: `!hasPreferredState` means No Preferred) | 5 |
| `hasNull` | 6 |
| `isVolatile` | 7 |
| `isBufferedBytes` | 8 |

This corresponds to the standard Output/Feature flag layout. For `Input` items, the HID specification uses **bit 7** for “Buffered Bytes” instead of volatility, so an Input item with `isBufferedBytes === true` is not reconstructed in the spec-canonical way (we will emit a 2-byte payload with bit 8 set).

---

## Encoding rules

### Short items only

We only emit **short items** (the normal HID item prefix with 0/1/2/4-byte payload).

- We never emit **long items** (`0xFE …`) because they are rare in practice and are a common source of compatibility problems.

### Minimal payload size selection

HID short items support payload sizes of `{ 0, 1, 2, 4 }` bytes.

For numeric values we emit, we choose the minimal payload size among `{ 1, 2, 4 }` bytes that can represent the value:

- Unsigned fields (e.g. `Usage Page`, `Usage`, `Report Size`, `Report Count`, `Report ID`) use the smallest unsigned width.
- Signed fields (`Logical Min/Max`, `Physical Min/Max`) use the smallest *signed* width that can represent the value.
- **Unit Exponent** (`0x55`) is **special** in HID 1.11: it is a **4-bit signed value** (`-8..=7`) stored in the **low nibble** of a **single byte** (high nibble reserved and emitted as `0`).

All payloads are encoded **little-endian**.

### Signed encoding

Signed values are encoded in two’s complement **except Unit Exponent**.

Example: `Logical Minimum (-1)` uses a 1-byte payload: `0xFF`.

Unit Exponent encoding (HID 1.11):

- `Unit Exponent (-1)` → `0x55 0x0F` (not `0x55 0xFF`)
- `Unit Exponent (-2)` → `0x55 0x0E`

---

## Validation rules

The synthesis step validates the WebHID metadata before emitting bytes.

### Report ID range

- `reportId` MUST be either:
  - `0` (meaning “no report IDs used in this descriptor”), or
  - in the inclusive range `1..=255`.

### Usage range sanity

When using `Usage Minimum` / `Usage Maximum`:

- `usageMax` MUST be `>= usageMin`.
- The range length (`usageMax - usageMin + 1`) SHOULD be consistent with `reportCount` when used for variable fields (common case: `reportCount == rangeLen`).

### Unit Exponent range

- `unitExponent` MUST be in the inclusive range `-8..=7` (HID 1.11 4-bit signed field).

### Mixed reportId 0/non-zero policy

Windows HID stacks (including Windows 7) treat “report IDs are present” as an interface-wide decision:

- If **any** report uses a non-zero report ID, then **all** reports are expected to include a report ID byte in the transmitted report data.

Policy: the synthesis emits `Report ID` items exactly as provided by WebHID (omitting it when `reportId == 0`) and does not attempt to “fix up” mixed usage. Callers should treat mixed `0`/non-zero report IDs as a metadata error unless they have a device-specific reason to allow it.

---

## Known limitations

- **Ordering loss / canonicalization**
  - WebHID does not give us the original report descriptor byte stream.
  - In particular, it may not preserve the exact ordering/interleaving of:
    - report main items vs nested child collection declarations
    - global item “state machine” usage (`Push`/`Pop`, reusing globals across items, etc.)
  - The synthesis produces a deterministic “canonical” ordering (reports grouped before children) and explicitly re-emits globals per report item.
- **Not all HID tags are supported**
  - We currently do not synthesize less-common local/global tags such as:
    - Designators
    - Strings
    - Delimiters
    - Long Items
  - These can be added if/when we encounter real devices that require them for correct behavior.

---

## Example: synthesized boot-mouse style descriptor (structure)

Given WebHID metadata corresponding to a typical 3-button relative mouse with wheel, the synthesized descriptor is structurally:

```
Usage Page (Generic Desktop)
Usage (Mouse)
Collection (Application)
  Usage (Pointer)
  Collection (Physical)
    Report ID (1)                ; omitted if reportId == 0

    Usage Page (Button)
    Usage Min (Button 1)
    Usage Max (Button 3)
    Logical Min (0)
    Logical Max (1)
    Report Count (3)
    Report Size (1)
    Input (Data, Variable, Absolute)

    Report Count (1)
    Report Size (5)
    Input (Constant)             ; padding

    Usage Page (Generic Desktop)
    Usage (X)
    Usage (Y)
    Usage (Wheel)
    Logical Min (-127)
    Logical Max (127)
    Report Count (3)
    Report Size (8)
    Input (Data, Variable, Relative)
  End Collection
End Collection
```

This is the shape Windows 7 expects for a conventional HID mouse (and is representative of how the synthesis expands WebHID report items into explicit global/local/main items).
