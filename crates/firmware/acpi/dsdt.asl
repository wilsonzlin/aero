/*
 * Clean-room DSDT for Aero.
 *
 * This is intentionally minimal: it declares only the devices and methods
 * required for Windows 7 to enumerate a basic ACPI PC.
 *
 * NOTE: The shipped `dsdt.aml` is generated from Rust code in
 * `crates/firmware/src/bin/gen_dsdt.rs` (which uses `aero-acpi`). Keep this
 * ASL in sync with the generated AML if you make changes.
 */

DefinitionBlock ("dsdt.aml", "DSDT", 2, "AERO  ", "AEROACPI", 0x00000001)
{
    Name (_S5, Package (0x02) { 0x05, 0x05 })

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
        IMCD, 8, // data port (0x23)
    }

    Method (_PIC, 1, NotSerialized)
    {
        Store (Arg0, PICM)
        Store (0x70, IMCS)
        And (Arg0, One, IMCD)
    }

    Scope (\_SB_)
    {
        Device (PCI0)
        {
            Name (_HID, "PNP0A03")
            Name (_UID, Zero)
            Name (_ADR, Zero)

            /*
             * PCI interrupt routing table.
             *
             * We use conventional PCI INTx swizzling:
             *   PIRQ = (Device + Pin) % 4
             * and map PIRQs to GSIs:
             *   A → 10, B → 11, C → 12, D → 13
             */
             Name (_PRT, Package ()
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
            Name (_HID, "PNP0103")
            Name (_UID, Zero)
            Name (_CRS, ResourceTemplate ()
            {
                Memory32Fixed (ReadWrite, 0xFED00000, 0x00000400)
            })
        }

        Device (RTC)
        {
            Name (_HID, "PNP0B00")
            Name (_UID, Zero)
            Name (_CRS, ResourceTemplate ()
            {
                IO (Decode16, 0x0070, 0x0070, 0x01, 0x02)
                IRQNoFlags () { 8 }
            })
        }

        Device (TIMR)
        {
            Name (_HID, "PNP0100")
            Name (_UID, Zero)
            Name (_CRS, ResourceTemplate ()
            {
                IO (Decode16, 0x0040, 0x0040, 0x01, 0x04)
                IRQNoFlags () { 0 }
            })
        }
    }
}
