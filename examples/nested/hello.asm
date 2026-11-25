; hello.asm - a whole virtual machine in one 512-byte boot sector, booted by QEMU inside
; Windows. It writes to the first serial port what it runs on (CPUID: the hypervisor bit and the
; hypervisor's 12-character signature), counts to 2^30 and says how long that took (the speed
; of its CPU), and makes QEMU exit with code 33 (isa-debug-exit: (0x10 << 1) | 1).
;
;   nasm -f bin -o hello.img hello.asm     (build.sh does it)

bits 16
org 0x7c00

start:
    cli
    xor ax, ax
    mov ds, ax
    mov ss, ax
    mov sp, 0x7c00
    cld
    ; COM1: 115200 baud, 8N1
    mov dx, 0x3fb
    mov al, 0x80
    out dx, al
    mov dx, 0x3f8
    mov al, 1
    out dx, al
    inc dx
    xor al, al
    out dx, al
    mov dx, 0x3fb
    mov al, 3
    out dx, al

    mov si, hello
    call puts
    ; CPUID.1:ECX bit 31 = under a hypervisor; leaf 0x40000000 = its signature in EBX ECX EDX
    mov si, nohv
    mov eax, 1
    cpuid
    bt ecx, 31
    jnc .say
    mov eax, 0x40000000
    cpuid
    mov [sig], ebx
    mov [sig + 4], ecx
    mov [sig + 8], edx
    ; a shorter signature ("KVMKVMKVM") is padded with zeros: make them spaces
    mov si, sig
    mov cx, 12
.pad:
    cmp byte [si], 0
    jne .next
    mov byte [si], ' '
.next:
    inc si
    loop .pad
    mov si, hv
.say:
    call puts

    ; 2^30 rounds of a two-instruction loop, timed with the BIOS clock (18.2 ticks a second):
    ; this part shows the CPU's speed, apart from the I/O that starting a VM is made of
    sti
    xor ah, ah
    int 0x1a
    mov [t0], dx
    mov ecx, 0x40000000
.spin:
    dec ecx
    jnz .spin
    xor ah, ah
    int 0x1a
    sub dx, [t0]
    mov [t0], dx
    mov si, done
    call puts
    mov ax, [t0]
    call putn
    mov si, ticks
    call puts

    ; isa-debug-exit: QEMU exits with (0x10 << 1) | 1 = 33
    mov dx, 0xf4
    mov al, 0x10
    out dx, al
.halt:
    hlt
    jmp .halt

; write the 0-terminated string at DS:SI to COM1
puts:
    lodsb
    test al, al
    jz .ret
    mov bl, al
    mov dx, 0x3fd
.busy:
    in al, dx
    test al, 0x20
    jz .busy
    mov dx, 0x3f8
    mov al, bl
    out dx, al
    jmp puts
.ret:
    ret

; write AX in decimal to COM1
putn:
    mov bx, 10
    xor cx, cx
.div:
    xor dx, dx
    div bx
    push dx
    inc cx
    test ax, ax
    jnz .div
.digit:
    pop ax
    add al, '0'
    mov [one], al
    mov si, one
    push cx
    call puts
    pop cx
    loop .digit
    ret

hello db "NESTED hello from a VM inside Windows", 13, 10, 0
hv    db "NESTED hypervisor: "
sig   times 12 db 0
      db 13, 10, 0
nohv  db "NESTED hypervisor: none", 13, 10, 0
done  db "NESTED counted to 2^30 in ", 0
ticks db " BIOS ticks (55 ms each)", 13, 10, 0
one   db 0, 0
t0    dw 0

times 510 - ($ - $$) db 0
dw 0xaa55
