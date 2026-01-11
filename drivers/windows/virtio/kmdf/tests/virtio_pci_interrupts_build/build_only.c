#include "virtio_pci_interrupts.h"

/*
 * Build-only compilation unit.
 *
 * This file intentionally does not reference any driver-specific headers beyond
 * the canonical interrupt helper. If this compiles under the Win7 WDK/KMDF
 * environment, the helper is at least syntactically compatible with the
 * supported toolchain and headers.
 */

VOID VirtioPciInterruptsBuildOnlyAnchor(VOID) {}

