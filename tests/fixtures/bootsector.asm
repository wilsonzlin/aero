; Minimal boot sector used for integration/E2E tests.
;
; Behavior:
;   - Switch to VGA mode 13h (320x200x256)
;   - Paint a deterministic pattern into VRAM
;   - Print a sentinel string to COM1 for serial-based assertions
;   - Halt in a tight loop
;
; Build:
;   bash ./scripts/build-bootsector.sh
;
bits 16
org 0x7c00

start:
  cli
  xor ax, ax
  mov ds, ax
  mov es, ax
  mov ss, ax
  mov sp, 0x7c00
  sti

  ; VGA 320x200x256
  mov ax, 0x0013
  int 0x10

  ; Fill top half with color 0x11, bottom half with 0x22.
  mov ax, 0xA000
  mov es, ax
  xor di, di
  mov cx, 320*100
  mov al, 0x11
  rep stosb
  mov cx, 320*100
  mov al, 0x22
  rep stosb

  ; Initialize COM1 (16550) at 115200 8N1 and write sentinel string.
  mov dx, 0x3F9
  xor al, al
  out dx, al          ; disable interrupts

  mov dx, 0x3FB
  mov al, 0x80
  out dx, al          ; DLAB on

  mov dx, 0x3F8
  mov al, 0x01
  out dx, al          ; divisor lo

  mov dx, 0x3F9
  xor al, al
  out dx, al          ; divisor hi

  mov dx, 0x3FB
  mov al, 0x03
  out dx, al          ; 8 bits, no parity, one stop bit

  mov dx, 0x3FA
  mov al, 0xC7
  out dx, al          ; enable FIFO, clear, 14-byte threshold

  mov dx, 0x3FC
  mov al, 0x0B
  out dx, al          ; IRQs enabled, RTS/DSR set

  mov si, msg
.send:
  lodsb
  test al, al
  jz .done
  mov bl, al
.wait:
  mov dx, 0x3FD
  in al, dx
  test al, 0x20
  jz .wait
  mov dx, 0x3F8
  mov al, bl
  out dx, al
  jmp .send

.done:
  hlt
  jmp .done

msg db "AERO_BOOTSECTOR_OK", 13, 10, 0

times 510-($-$$) db 0
dw 0xAA55
