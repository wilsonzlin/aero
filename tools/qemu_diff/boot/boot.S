.code16
.intel_syntax noprefix

.section .text
.global _start
_start:
    cli
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x7c00
    cld

    mov [boot_drive], dl

    # Read 4 sectors starting at CHS (0,0,2) into PAYLOAD_ADDR.
    mov bx, 0x8000
    mov ah, 0x02        # INT 13h: read sectors
    mov al, 4
    mov ch, 0x00        # cylinder
    mov dh, 0x00        # head
    mov cl, 0x02        # sector (1-based, sector 1 is boot)
    mov dl, [boot_drive]
    int 0x13
    jc disk_error

    # BIOS calls are not required to preserve segment registers; reset to a flat layout.
    xor ax, ax
    mov ds, ax
    mov es, ax
    cld

    # Initialize scratch memory region (MEM_BASE..MEM_BASE+256).
    mov si, 0x801c
    mov di, 0x0500
    mov cx, 256
    rep movsb

    # Copy code bytes to CODE_BASE.
    mov cx, word ptr [PAYLOAD_ADDR + OFF_CODE_LEN]
    mov si, 0x811c
    mov di, 0x0700
    rep movsb

    # Load initial registers/flags.
    mov ax, word ptr [PAYLOAD_ADDR + OFF_AX]
    mov bx, word ptr [PAYLOAD_ADDR + OFF_BX]
    mov cx, word ptr [PAYLOAD_ADDR + OFF_CX]
    mov dx, word ptr [PAYLOAD_ADDR + OFF_DX]
    mov si, word ptr [PAYLOAD_ADDR + OFF_SI]
    mov di, word ptr [PAYLOAD_ADDR + OFF_DI]
    mov bp, word ptr [PAYLOAD_ADDR + OFF_BP]
    mov sp, word ptr [PAYLOAD_ADDR + OFF_SP]
    push word ptr [PAYLOAD_ADDR + OFF_FLAGS]
    popf

    # Execute the snippet. The snippet must terminate with RET.
    call CODE_BASE

    # Save registers to payload buffer (reusing input buffer).
    mov word ptr [PAYLOAD_ADDR + OFF_AX], ax
    mov word ptr [PAYLOAD_ADDR + OFF_BX], bx
    mov word ptr [PAYLOAD_ADDR + OFF_CX], cx
    mov word ptr [PAYLOAD_ADDR + OFF_DX], dx
    mov word ptr [PAYLOAD_ADDR + OFF_SI], si
    mov word ptr [PAYLOAD_ADDR + OFF_DI], di
    mov word ptr [PAYLOAD_ADDR + OFF_BP], bp
    mov word ptr [PAYLOAD_ADDR + OFF_SP], sp
    pushf
    pop ax
    mov word ptr [PAYLOAD_ADDR + OFF_FLAGS], ax
    cld

    # Compute 32-bit FNV-1a hash of MEM_INIT_LEN bytes at MEM_BASE.
    mov si, 0x0500
    mov cx, 256
    mov eax, 0x811c9dc5
.hash_loop:
    xor al, byte ptr [si]
    imul eax, eax, 0x01000193
    inc si
    loop .hash_loop
    mov dword ptr [PAYLOAD_ADDR + OFF_MEM_HASH], eax

    # Emit one line on the debug console (port 0xE9).
    lea si, [msg_prefix]
    call print_str

    call emit_regs_and_hash

    # Exit QEMU (isa-debug-exit triggers on word writes).
    mov dx, 0xf4
    xor ax, ax
    out dx, ax

hang:
    hlt
    jmp hang

disk_error:
    lea si, [msg_disk_error]
    call print_str
    mov dx, 0xf4
    mov ax, 1
    out dx, ax
    jmp hang

# --- Output helpers ---------------------------------------------------------

emit_regs_and_hash:
    # AX
    lea si, [msg_ax]
    call print_str
    mov ax, word ptr [PAYLOAD_ADDR + OFF_AX]
    call print_hex16

    lea si, [msg_bx]
    call print_str
    mov ax, word ptr [PAYLOAD_ADDR + OFF_BX]
    call print_hex16

    lea si, [msg_cx]
    call print_str
    mov ax, word ptr [PAYLOAD_ADDR + OFF_CX]
    call print_hex16

    lea si, [msg_dx]
    call print_str
    mov ax, word ptr [PAYLOAD_ADDR + OFF_DX]
    call print_hex16

    lea si, [msg_si]
    call print_str
    mov ax, word ptr [PAYLOAD_ADDR + OFF_SI]
    call print_hex16

    lea si, [msg_di]
    call print_str
    mov ax, word ptr [PAYLOAD_ADDR + OFF_DI]
    call print_hex16

    lea si, [msg_bp]
    call print_str
    mov ax, word ptr [PAYLOAD_ADDR + OFF_BP]
    call print_hex16

    lea si, [msg_sp]
    call print_str
    mov ax, word ptr [PAYLOAD_ADDR + OFF_SP]
    call print_hex16

    lea si, [msg_fl]
    call print_str
    mov ax, word ptr [PAYLOAD_ADDR + OFF_FLAGS]
    call print_hex16

    lea si, [msg_mh]
    call print_str
    mov eax, dword ptr [PAYLOAD_ADDR + OFF_MEM_HASH]
    call print_hex32

    mov al, 0x0A
    call putc
    ret

putc:
    out 0xe9, al
    ret

print_str:
    # DS:SI = 0-terminated string
.str_loop:
    lodsb
    test al, al
    jz .str_done
    call putc
    jmp .str_loop
.str_done:
    ret

print_hex_nibble:
    # AL = 0..15
    cmp al, 10
    jl .digit
    add al, 'A' - 10
    jmp putc
.digit:
    add al, '0'
    jmp putc

print_hex16:
    push ax
    push dx
    mov dx, ax
    mov cx, 4
.hex16_loop:
    rol dx, 4
    mov al, dl
    and al, 0x0f
    call print_hex_nibble
    loop .hex16_loop
    pop dx
    pop ax
    ret

print_hex32:
    push eax
    push edx
    mov edx, eax
    mov cx, 8
.hex32_loop:
    rol edx, 4
    mov al, dl
    and al, 0x0f
    call print_hex_nibble
    loop .hex32_loop
    pop edx
    pop eax
    ret

# --- Data ------------------------------------------------------------------

msg_prefix:
    .ascii "AERODIFF"
    .byte 0
msg_disk_error:
    .ascii "AERODIFF DISK_ERROR\n"
    .byte 0

msg_ax: .ascii " AX=" # leading space for easy tokenization
    .byte 0
msg_bx: .ascii " BX="
    .byte 0
msg_cx: .ascii " CX="
    .byte 0
msg_dx: .ascii " DX="
    .byte 0
msg_si: .ascii " SI="
    .byte 0
msg_di: .ascii " DI="
    .byte 0
msg_bp: .ascii " BP="
    .byte 0
msg_sp: .ascii " SP="
    .byte 0
msg_fl: .ascii " FL="
    .byte 0
msg_mh: .ascii " MH="
    .byte 0

boot_drive:
    .byte 0

# --- Layout constants -------------------------------------------------------

.equ PAYLOAD_ADDR,    0x8000
.equ CODE_BASE,       0x0700
.equ MEM_BASE,        0x0500

.equ PAYLOAD_SECTORS, 4

.equ OFF_AX,          0
.equ OFF_BX,          2
.equ OFF_CX,          4
.equ OFF_DX,          6
.equ OFF_SI,          8
.equ OFF_DI,          10
.equ OFF_BP,          12
.equ OFF_SP,          14
.equ OFF_FLAGS,       16
.equ OFF_CODE_LEN,    24
.equ OFF_MEM_INIT,    28
.equ MEM_INIT_LEN,    256
.equ OFF_CODE,        OFF_MEM_INIT + MEM_INIT_LEN
.equ OFF_MEM_HASH,    OFF_CODE + 0x0400

    # Pad the boot sector and add the signature.
    .org 510
    .word 0xaa55
