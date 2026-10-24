# The test fixtures, AArch64 ELFs: one architecture for both engines' decompilers,
# and bnx answers about the same binaries. `make` builds them where the
# cross compiler is; `make prebuilt` refreshes bin/, which hosts without one
# (macOS) copy from. bin/ keeps the sources it was built from, so a stale
# copy is refused rather than tested against.
CC = aarch64-linux-gnu-gcc
CFLAGS ?= -O1

prebuilt:
	$(CC) $(CFLAGS) -o bin/fw.bin fw.c
	$(CC) $(CFLAGS) -o bin/demo demo.c
	cp fw.c demo.c bin/

.PHONY: prebuilt
