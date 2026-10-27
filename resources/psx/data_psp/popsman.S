/* scePopsMan imports; pspsdk ships no stub for this library. */
	.set noreorder

#include "pspimport.s"

	IMPORT_START	"scePopsMan",0x40090000
	IMPORT_FUNC	"scePopsMan",0x29B3FB24,scePopsManLoadModule
	IMPORT_FUNC	"scePopsMan",0x0090B2C8,scePopsManExitVSHKernel
