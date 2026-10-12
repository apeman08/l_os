/* Static executable for the user window; one PT_LOAD per distinct permission. */
ENTRY(_start)

SECTIONS {
    . = 0x700000000000;

    .text   ALIGN(4096) : { *(.text .text.*) }
    .rodata ALIGN(4096) : { *(.rodata .rodata.*) }
    .data   ALIGN(4096) : { *(.data .data.*) *(.got .got.*) }
    .bss    ALIGN(4096) : { *(.bss .bss.*) *(COMMON) }

    /DISCARD/ : { *(.eh_frame*) *(.comment) *(.note*) }
}
