/*
 * Clean-room DSDT for Aero.
 *
 * This is a human-readable reference intended to stay in sync with the
 * generated AML fixture at `crates/firmware/acpi/dsdt.aml`.
 *
 * NOTE: The shipped `dsdt.aml` is generated from Rust code in
 * `crates/firmware/src/bin/gen_dsdt.rs` (which uses `aero-acpi`).
 *
 * Regenerate deterministic in-repo fixtures (preferred):
 *
 *     cargo xtask fixtures
 *
 * Or regenerate just the legacy PCI DSDT AML blob (`crates/firmware/acpi/dsdt.aml`) with:
 *
 *     cargo run -p firmware --bin gen_dsdt --locked
 *
 * Keep this ASL in sync with the generated AML if you make changes.
 *
 * To get a baseline DSL for comparison, disassemble the shipped fixture:
 *
 *     iasl -d crates/firmware/acpi/dsdt.aml
 */

DefinitionBlock ("dsdt.aml", "DSDT", 2, "AERO  ", "AEROACPI", 0x00000001)
{
    Name (PICM, Zero)

    /*
     * IMCR (Interrupt Mode Configuration Register)
     *
     * Some chipsets provide an IMCR at ports 0x22/0x23 that switches ISA IRQ
     * routing between the legacy 8259 PIC and the APIC/IOAPIC.
     *
     * Writing bit0 to IMCR register 0x70 selects:
     *   0 = PIC (legacy)
     *   1 = APIC (I/O APIC)
     */
    OperationRegion (IMCR, SystemIO, 0x22, 0x02)
    Field (IMCR, ByteAcc, NoLock, Preserve)
    {
        IMCS, 8, // select port (0x22)
        IMCD, 8  // data port (0x23)
    }

    Method (_PIC, 1, NotSerialized)
    {
        PICM = Arg0
        IMCS = 0x70
        IMCD = (Arg0 & One)
    }

    Method (_PTS, 1, NotSerialized)
    {
    }

    Method (_WAK, 1, NotSerialized)
    {
        Return (Package (0x02) { Zero, Zero })
    }

    Scope (_SB)
    {
        /*
         * Motherboard resources device.
         *
         * Reserving the fixed-function ACPI PM I/O ports (and reset port)
         * prevents OS resource allocators from treating them as free PCI I/O
         * space.
         */
        Device (SYS0)
        {
            Name (_HID, EisaId ("PNP0C02"))
            Name (_UID, Zero)
            Name (_STA, 0x0F)
            Name (_CRS, ResourceTemplate ()
            {
                // FADT SMI command port used for the ACPI enable handshake.
                IO (Decode16, 0x00B2, 0x00B2, 0x01, 0x01)

                // ACPI fixed-feature PM blocks.
                IO (Decode16, 0x0400, 0x0400, 0x01, 0x04) // PM1a_EVT_BLK
                IO (Decode16, 0x0404, 0x0404, 0x01, 0x02) // PM1a_CNT_BLK
                IO (Decode16, 0x0408, 0x0408, 0x01, 0x04) // PM_TMR_BLK
                IO (Decode16, 0x0420, 0x0420, 0x01, 0x08) // GPE0_BLK

                // Reset port used by the FADT ResetReg.
                IO (Decode16, 0x0CF9, 0x0CF9, 0x01, 0x01)
            })
        }

        Device (PWRB)
        {
            Name (_HID, EisaId ("PNP0C0C"))
            Name (_UID, Zero)
            Name (_STA, 0x0F)
        }

        Device (SLPB)
        {
            Name (_HID, EisaId ("PNP0C0E"))
            Name (_UID, Zero)
            Name (_STA, 0x0F)
        }

        Device (PCI0)
        {
            Name (_HID, EisaId ("PNP0A03"))
            Name (_UID, Zero)
            Name (_BBN, Zero)
            Name (_SEG, Zero)

            /*
             * PCI host bridge resource windows.
             *
             * This matches `aero-acpi`'s default bridge model (I/O ports for
             * config mechanism 1, full bus range, and a single 32-bit MMIO
             * aperture for PCI BAR allocation).
             */
            Name (_CRS, ResourceTemplate ()
            {
                WordBusNumber (ResourceProducer, MinFixed, MaxFixed, PosDecode,
                    0x0000,             // Granularity
                    0x0000,             // Range Minimum
                    0x00FF,             // Range Maximum
                    0x0000,             // Translation Offset
                    0x0100,             // Length
                    ,, )

                // PCI config mechanism 1 (0xCF8..0xCFF).
                IO (Decode16, 0x0CF8, 0x0CF8, 0x01, 0x08)

                // PCI I/O space (excluding 0xCF8..0xCFF).
                WordIO (ResourceProducer, MinFixed, MaxFixed, PosDecode, EntireRange,
                    0x0000,             // Granularity
                    0x0000,             // Range Minimum
                    0x0CF7,             // Range Maximum
                    0x0000,             // Translation Offset
                    0x0CF8,             // Length
                    ,, , TypeStatic, DenseTranslation)
                WordIO (ResourceProducer, MinFixed, MaxFixed, PosDecode, EntireRange,
                    0x0000,             // Granularity
                    0x0D00,             // Range Minimum
                    0xFFFF,             // Range Maximum
                    0x0000,             // Translation Offset
                    0xF300,             // Length
                    ,, , TypeStatic, DenseTranslation)

                // PCI MMIO window.
                DWordMemory (ResourceProducer, PosDecode, MinFixed, MaxFixed, Cacheable, ReadWrite,
                    0x00000000,         // Granularity
                    0xC0000000,         // Range Minimum
                    0xFEBFFFFF,         // Range Maximum
                    0x00000000,         // Translation Offset
                    0x3EC00000,         // Length
                    ,, , AddressRangeMemory, TypeStatic)
            })

            /*
             * PCI interrupt routing table.
             *
             * We use conventional PCI INTx swizzling:
             *   PIRQ = (Device + Pin) % 4
             * and map PIRQs to GSIs:
              *   A → 10, B → 11, C → 12, D → 13
              */
            Name (_PRT, Package (0x7C)
            {
                Package () { 0x0001FFFF, 0, Zero, 11 },
                Package () { 0x0001FFFF, 1, Zero, 12 },
                Package () { 0x0001FFFF, 2, Zero, 13 },
                Package () { 0x0001FFFF, 3, Zero, 10 },
                Package () { 0x0002FFFF, 0, Zero, 12 },
                Package () { 0x0002FFFF, 1, Zero, 13 },
                Package () { 0x0002FFFF, 2, Zero, 10 },
                Package () { 0x0002FFFF, 3, Zero, 11 },
                Package () { 0x0003FFFF, 0, Zero, 13 },
                Package () { 0x0003FFFF, 1, Zero, 10 },
                Package () { 0x0003FFFF, 2, Zero, 11 },
                Package () { 0x0003FFFF, 3, Zero, 12 },
                Package () { 0x0004FFFF, 0, Zero, 10 },
                Package () { 0x0004FFFF, 1, Zero, 11 },
                Package () { 0x0004FFFF, 2, Zero, 12 },
                Package () { 0x0004FFFF, 3, Zero, 13 },
                Package () { 0x0005FFFF, 0, Zero, 11 },
                Package () { 0x0005FFFF, 1, Zero, 12 },
                Package () { 0x0005FFFF, 2, Zero, 13 },
                Package () { 0x0005FFFF, 3, Zero, 10 },
                Package () { 0x0006FFFF, 0, Zero, 12 },
                Package () { 0x0006FFFF, 1, Zero, 13 },
                Package () { 0x0006FFFF, 2, Zero, 10 },
                Package () { 0x0006FFFF, 3, Zero, 11 },
                Package () { 0x0007FFFF, 0, Zero, 13 },
                Package () { 0x0007FFFF, 1, Zero, 10 },
                Package () { 0x0007FFFF, 2, Zero, 11 },
                Package () { 0x0007FFFF, 3, Zero, 12 },
                Package () { 0x0008FFFF, 0, Zero, 10 },
                Package () { 0x0008FFFF, 1, Zero, 11 },
                Package () { 0x0008FFFF, 2, Zero, 12 },
                Package () { 0x0008FFFF, 3, Zero, 13 },
                Package () { 0x0009FFFF, 0, Zero, 11 },
                Package () { 0x0009FFFF, 1, Zero, 12 },
                Package () { 0x0009FFFF, 2, Zero, 13 },
                Package () { 0x0009FFFF, 3, Zero, 10 },
                Package () { 0x000AFFFF, 0, Zero, 12 },
                Package () { 0x000AFFFF, 1, Zero, 13 },
                Package () { 0x000AFFFF, 2, Zero, 10 },
                Package () { 0x000AFFFF, 3, Zero, 11 },
                Package () { 0x000BFFFF, 0, Zero, 13 },
                Package () { 0x000BFFFF, 1, Zero, 10 },
                Package () { 0x000BFFFF, 2, Zero, 11 },
                Package () { 0x000BFFFF, 3, Zero, 12 },
                Package () { 0x000CFFFF, 0, Zero, 10 },
                Package () { 0x000CFFFF, 1, Zero, 11 },
                Package () { 0x000CFFFF, 2, Zero, 12 },
                Package () { 0x000CFFFF, 3, Zero, 13 },
                Package () { 0x000DFFFF, 0, Zero, 11 },
                Package () { 0x000DFFFF, 1, Zero, 12 },
                Package () { 0x000DFFFF, 2, Zero, 13 },
                Package () { 0x000DFFFF, 3, Zero, 10 },
                Package () { 0x000EFFFF, 0, Zero, 12 },
                Package () { 0x000EFFFF, 1, Zero, 13 },
                Package () { 0x000EFFFF, 2, Zero, 10 },
                Package () { 0x000EFFFF, 3, Zero, 11 },
                Package () { 0x000FFFFF, 0, Zero, 13 },
                Package () { 0x000FFFFF, 1, Zero, 10 },
                Package () { 0x000FFFFF, 2, Zero, 11 },
                Package () { 0x000FFFFF, 3, Zero, 12 },
                Package () { 0x0010FFFF, 0, Zero, 10 },
                Package () { 0x0010FFFF, 1, Zero, 11 },
                Package () { 0x0010FFFF, 2, Zero, 12 },
                Package () { 0x0010FFFF, 3, Zero, 13 },
                Package () { 0x0011FFFF, 0, Zero, 11 },
                Package () { 0x0011FFFF, 1, Zero, 12 },
                Package () { 0x0011FFFF, 2, Zero, 13 },
                Package () { 0x0011FFFF, 3, Zero, 10 },
                Package () { 0x0012FFFF, 0, Zero, 12 },
                Package () { 0x0012FFFF, 1, Zero, 13 },
                Package () { 0x0012FFFF, 2, Zero, 10 },
                Package () { 0x0012FFFF, 3, Zero, 11 },
                Package () { 0x0013FFFF, 0, Zero, 13 },
                Package () { 0x0013FFFF, 1, Zero, 10 },
                Package () { 0x0013FFFF, 2, Zero, 11 },
                Package () { 0x0013FFFF, 3, Zero, 12 },
                Package () { 0x0014FFFF, 0, Zero, 10 },
                Package () { 0x0014FFFF, 1, Zero, 11 },
                Package () { 0x0014FFFF, 2, Zero, 12 },
                Package () { 0x0014FFFF, 3, Zero, 13 },
                Package () { 0x0015FFFF, 0, Zero, 11 },
                Package () { 0x0015FFFF, 1, Zero, 12 },
                Package () { 0x0015FFFF, 2, Zero, 13 },
                Package () { 0x0015FFFF, 3, Zero, 10 },
                Package () { 0x0016FFFF, 0, Zero, 12 },
                Package () { 0x0016FFFF, 1, Zero, 13 },
                Package () { 0x0016FFFF, 2, Zero, 10 },
                Package () { 0x0016FFFF, 3, Zero, 11 },
                Package () { 0x0017FFFF, 0, Zero, 13 },
                Package () { 0x0017FFFF, 1, Zero, 10 },
                Package () { 0x0017FFFF, 2, Zero, 11 },
                Package () { 0x0017FFFF, 3, Zero, 12 },
                Package () { 0x0018FFFF, 0, Zero, 10 },
                Package () { 0x0018FFFF, 1, Zero, 11 },
                Package () { 0x0018FFFF, 2, Zero, 12 },
                Package () { 0x0018FFFF, 3, Zero, 13 },
                Package () { 0x0019FFFF, 0, Zero, 11 },
                Package () { 0x0019FFFF, 1, Zero, 12 },
                Package () { 0x0019FFFF, 2, Zero, 13 },
                Package () { 0x0019FFFF, 3, Zero, 10 },
                Package () { 0x001AFFFF, 0, Zero, 12 },
                Package () { 0x001AFFFF, 1, Zero, 13 },
                Package () { 0x001AFFFF, 2, Zero, 10 },
                Package () { 0x001AFFFF, 3, Zero, 11 },
                Package () { 0x001BFFFF, 0, Zero, 13 },
                Package () { 0x001BFFFF, 1, Zero, 10 },
                Package () { 0x001BFFFF, 2, Zero, 11 },
                Package () { 0x001BFFFF, 3, Zero, 12 },
                Package () { 0x001CFFFF, 0, Zero, 10 },
                Package () { 0x001CFFFF, 1, Zero, 11 },
                Package () { 0x001CFFFF, 2, Zero, 12 },
                Package () { 0x001CFFFF, 3, Zero, 13 },
                Package () { 0x001DFFFF, 0, Zero, 11 },
                Package () { 0x001DFFFF, 1, Zero, 12 },
                Package () { 0x001DFFFF, 2, Zero, 13 },
                Package () { 0x001DFFFF, 3, Zero, 10 },
                Package () { 0x001EFFFF, 0, Zero, 12 },
                Package () { 0x001EFFFF, 1, Zero, 13 },
                Package () { 0x001EFFFF, 2, Zero, 10 },
                Package () { 0x001EFFFF, 3, Zero, 11 },
                 Package () { 0x001FFFFF, 0, Zero, 13 },
                 Package () { 0x001FFFFF, 1, Zero, 10 },
                 Package () { 0x001FFFFF, 2, Zero, 11 },
                 Package () { 0x001FFFFF, 3, Zero, 12 },
            })
        }

        Device (HPET)
        {
            Name (_HID, EisaId ("PNP0103"))
            Name (_UID, Zero)
            Name (_STA, 0x0F)
            Name (_CRS, ResourceTemplate ()
            {
                Memory32Fixed (ReadWrite, 0xFED00000, 0x00000400)
            })
        }

        Device (RTC)
        {
            Name (_HID, EisaId ("PNP0B00"))
            Name (_UID, Zero)
            Name (_STA, 0x0F)
            Name (_CRS, ResourceTemplate ()
            {
                IO (Decode16, 0x0070, 0x0070, 0x01, 0x02)
                IRQNoFlags () { 8 }
            })
        }

        Device (TIMR)
        {
            Name (_HID, EisaId ("PNP0100"))
            Name (_UID, Zero)
            Name (_STA, 0x0F)
            Name (_CRS, ResourceTemplate ()
            {
                IO (Decode16, 0x0040, 0x0040, 0x01, 0x04)
                IRQNoFlags () { 0 }
            })
        }
    }

    // CPU objects (default generator config emits CPU0 only).
    Scope (_PR)
    {
        Device (CPU0)
        {
            Name (_HID, "ACPI0007")
            Name (_UID, Zero)
            Name (_STA, 0x0F)
        }
    }

    Name (_S1, Package (0x02) { One, One })
    Name (_S3, Package (0x02) { 0x03, 0x03 })
    Name (_S4, Package (0x02) { 0x04, 0x04 })
    Name (_S5, Package (0x02) { 0x05, 0x05 })
}
