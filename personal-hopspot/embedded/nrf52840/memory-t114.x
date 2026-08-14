/* Heltec Mesh Node T114 Rev. 2.x with the existing Adafruit serial-DFU
   bootloader and S140-v6 recovery image left untouched.

   The bootloader starts applications at 0x26000 and occupies flash from
   0xEC000 upward. Prns owns only 0x26000..0xE9000. The final three pages stay
   clear for a future radio-profile pair and the node identity at 0xEB000.

   This target does not enable the resident SoftDevice. RAM below 0x20006000
   remains reserved so the recovery layout matches the qualified board. */
/* BENCH ADAPTATION, NOT FOR UPSTREAM. faro-t114 on this bench was re-bootloadered to S140 7.3.0
   on 2026-08-08, so its bootloader starts applications at 0x27000, not the stock 0x26000. The
   origin moves up one 4 KiB page and the region shrinks by the same page, so the END of the
   application area stays exactly where mark-ik put it and the storage and identity offsets above
   it are untouched. Revert both numbers before comparing against the qualified stock build. */
APPLICATION_FLASH_ORIGIN = 0x00027000;
APPLICATION_FLASH_BYTES = 0xC2000;
APPLICATION_RAM_ORIGIN = 0x20006000;
APPLICATION_RAM_BYTES = 0x3A000;
MIN_RUNTIME_STACK_BYTES = 68K;

MEMORY
{
  FLASH : ORIGIN = APPLICATION_FLASH_ORIGIN, LENGTH = APPLICATION_FLASH_BYTES
  RAM   : ORIGIN = APPLICATION_RAM_ORIGIN, LENGTH = APPLICATION_RAM_BYTES
}

ASSERT(
  ORIGIN(RAM) + LENGTH(RAM) - _stack_end >= MIN_RUNTIME_STACK_BYTES,
  "T114 Prns static memory leaves too little runtime stack"
)
