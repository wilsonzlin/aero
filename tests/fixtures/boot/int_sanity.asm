BITS 16
ORG 0x7C00

; Synthetic boot sector historically used by BIOS interrupt integration tests.
;
; It exercises a small subset of BIOS interrupts and writes observable results
; into low RAM so the host-side test harness can assert behaviour without a
; full CPU emulator.

start:
    mov ax, 0
    mov ds, ax
    mov es, ax

    ; INT 10h teletype: prints 'A' (captured by the test bus serial sink).
    mov ah, 0x0E
    mov al, 'A'
    int 0x10

    ; INT 15h E820: write first entry to ES:DI = 0000:0600.
    mov di, 0x0600
    mov eax, 0xE820
    mov edx, 0x534D4150        ; 'SMAP'
    mov ecx, 24
    mov ebx, 0
    int 0x15

    ; INT 16h read key: store AX at 0000:0510.
    mov ah, 0x00
    int 0x16
    mov [0x0510], ax

    ; INT 13h read sector: read CHS (0,0,2) into 0000:0700.
    mov bx, 0x0700
    mov ax, 0x0201            ; AH=2 read, AL=1 sector
    mov cx, 0x0002            ; CL=2 sector (boot is sector 1)
    mov dx, 0x0080            ; DL=0x80
    int 0x13
    mov al, [0x0700]
    mov [0x0520], al

    ; Signature for the host-side runner.
    mov word [0x0530], 'OK'

    hlt

hang:
    jmp hang

TIMES 510-($-$$) DB 0
DW 0xAA55
