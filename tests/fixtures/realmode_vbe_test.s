/* Tiny real-mode VBE exerciser.
 *
 * This is intentionally small and self-contained so it can be loaded as a flat
 * binary by a harness that enters 16-bit real mode and starts execution at
 * offset 0.
 *
 * NOTE: The Rust test suite currently calls the BIOS handlers directly.
 * This fixture exists to enable future end-to-end execution tests.
 */

.code16
.global _start

_start:
    xorw %ax, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss
    movw $0x7C00, %sp

    /* Request VBE2 structure. */
    movw $0x0500, %di
    movw $0x5642, (%di)      /* 'VB' */
    movw $0x4532, 2(%di)     /* 'E2' */

    /* AX=4F00h: Controller Info */
    movw $0x4F00, %ax
    int  $0x10

    /* If failed, hang. */
    cmpw $0x004F, %ax
    jne  hang

    /* AX=4F02h: Set mode 0x118 with LFB bit. */
    movw $0x4F02, %ax
    movw $0x4118, %cx
    int  $0x10

hang:
    hlt
    jmp hang

