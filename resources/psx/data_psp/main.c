/*
 * DATA.PSP for the PS1 EBOOTs rom-converto builds: the main module the PSP
 * runs for a PBP whose PARAM.SFO category is ME.
 *
 * It does one thing: scePopsMan NID 0x29B3FB24 loads POPS for the PBP at
 * argv[0] in mode 1. On failure, NID 0x0090B2C8 returns to the XMB with
 * 0x80000004. Built with pspsdk (BSD), no libc.
 */
#include <pspkernel.h>

PSP_MODULE_INFO("RomConvertoPopsLoader", PSP_MODULE_USER, 1, 0);

#define POPS_LOAD_MODE 1
#define POPS_LOAD_FAILED ((int)0x80000004)
#define LOADER_THREAD_NAME "pops_loader"
#define LOADER_THREAD_PRIORITY 0x20
#define LOADER_THREAD_STACK_SIZE 0x4000

int scePopsManLoadModule(const char *pbp_path, int mode);
void scePopsManExitVSHKernel(int error);

/* The firmware passes the module arguments, argv[0] being the PBP path, as argp. */
static int loader_thread(SceSize args, void *argp)
{
    const char *pbp_path = args > 0 ? (const char *)argp : "";

    if (scePopsManLoadModule(pbp_path, POPS_LOAD_MODE) < 0) {
        scePopsManExitVSHKernel(POPS_LOAD_FAILED);
    }
    return sceKernelExitDeleteThread(0);
}

int module_start(SceSize args, void *argp)
{
    SceUID thread = sceKernelCreateThread(LOADER_THREAD_NAME, loader_thread, LOADER_THREAD_PRIORITY,
                                          LOADER_THREAD_STACK_SIZE, PSP_THREAD_ATTR_USER, NULL);
    if (thread < 0) {
        return thread;
    }
    return sceKernelStartThread(thread, args, argp);
}
