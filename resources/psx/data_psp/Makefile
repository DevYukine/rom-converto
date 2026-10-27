# A plain-ELF user PRX built with pspsdk's build_prx.mak (no crt0, no libc).
PSPSDK := $(shell psp-config --pspsdk-path)

TARGET = DATA
OBJS = main.o popsman.o $(PSPSDK)/lib/prxexports.o

CFLAGS = -Os -G0 -std=c11 -Wall -Wextra -Werror
ASFLAGS = $(CFLAGS)
LIBS = -lpspuser

include $(PSPSDK)/lib/build_prx.mak
