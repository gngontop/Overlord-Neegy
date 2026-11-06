# Changelog
All notable changes to this project will be documented in this file.

## [Unreleased]

### Fixed

* Hardened the loader stub's AMSI, WLDP and ETW bypasses. They now patch exact,
  minimal byte sequences instead of copying compiled C "stub" functions whose
  length was calculated from adjacent function addresses.
* Bounded the AMSI signature scan and CLR `.data` scan so they can no longer
  read outside the mapped image.
* Added a patchless ETW bypass: a VEH plus execute hardware breakpoints on
  `NtTraceEvent` (falling back to `EtwEventWrite`). The handler fakes the
  function return, so no bytes are written to ntdll or any other module. The
  breakpoint is installed on the current thread and, when `NtGetNextThread` is
  available, on every other thread in the process. If debug registers are
  already occupied, the old inline syscall-stub patch is used as a fallback.
* Changed the inline ETW fallback to patch the lower-level `NtTraceEvent`
  syscall stub first, with `EtwEventWrite` only as a fallback. This avoids the
  very common `EtwEventWrite` prologue signature.
* Kept the x86 ETW synthetic return as `ret 14h`: the first argument
  (`TRACEHANDLE`/`REGHANDLE`) is 64-bit, so the four logical arguments occupy
  20 bytes on the stack.
* Fixed an out-of-bounds read/overflow when a DLL name shorter than four
  characters (or close to the buffer limit) is resolved in `xGetLibAddress`.
* Fixed a double free when `build_instance()` failed after `build_module()`.
* The loader now wipes the instance/module keys, MAC inputs, HTTP credentials,
  evasion API names and bypass flags as soon as they are no longer needed.
* `DonutDelete()` zeroes module, instance and shellcode buffers before freeing
  them and resets the associated lengths.
* When AMSI/WLDP/ETW bypassing is enabled, the instance is always encrypted.
  This prevents `-e 1`/`-e 2` from leaving plaintext `amsi`, `AmsiScanBuffer`,
  `EtwEventWrite`, etc. in the generated stub.

### Changed

* The mingw loader templates are built with tighter code-size flags and the
  default WLDP bypass is now `BYPASS_WLDP_B` (the previous no-op `_A` variant
  did not actually disable WLDP).

## [v1.0]

### Added

* Added module overloading for native PE payloads. Hides them in MEM_IMAGE memory backed by a decoy file on disk
* If donut is set to mutate native PE headers, then for module overloads it will overwrite the payload's PE headers with those of the decoy file to fool some detections of module overloading
* Added an option to block indefinitely after running the payload
* Added Dockerfile to create a docker image for generating donut shellcode
* Added support for binaries without relocation information or with certain edge cases for relocation information
* Added custom GetProcAddress and LoadLibrary replacement functions that will only call those Win32 API calls as fallbacks
* Better documentation for debugging, designing with, and integrating Donut.
* Added moduler bypass system for ETW
* Added option for preserving or overwriting PE headers of native payloads
* Added an inject_local.exe that runs shellcode in the current process for testing purposes
* C# output generator
* Python output generator
* UUID string output generator

### Changed

* The -y switch now uses the value provided as an offset from the base address of the process's executable module. So, for example, if you have injected donut into an infected, legitimate PE file, then once donut completes it will create a thread at the provided offset of that legitimate PE, allowing it to resume its legitimate execution and do what it was supposed to do in the first place.
* Fixed some issues with the MingW makefile (#96)
* Fixed and improved all makefiles
* Fixed the Python module

## [v0.9.3]

### Added

* The -e switch can be used to disable entropy and/or encryption. Options are: 1=none, 2=generate random names, 3=generate random names + use symmetric encryption.
* The -z switch tells the builder to compress the input file. 1=none, 2=aPLib. On Windows, a further three algorithms are supported, which are 3=LZNT1, 4=Xpress and 5=Xpress Huffman.
* The -f switch specifies the output format for loader. 1=binary, 2=base64, 3=c, 4=ruby, 5=python, 6=powershell, 7=c# and 8=hex. On Windows, Base64 strings are copied to the clipboard.
* The -t switch tells the loader to run unmanaged entrypoint for EXE as a thread. This also attempts to intercept exit-related API in Import Address Table by replacing their pointers with the address of RtlExitUserThread.
* The -n switch can be used to specify name of module for HTTP staging. If entropy is enabled, this is generated randomly.
* The -s switch specifies the HTTP server to download module from.
* The -y switch tells loader to create a new thread for the loader and continues executing at a specific address or Original Entry Point (OEP). The address should be provided as a string in hexadecimal format.
* The -x switch can be used to specify how loader terminates. 1=exit thread, 2=exit process.
* The -p switch is used to specify parameters to .NET method, DLL function or command line for an unmanaged EXE file. Wrap multiple parameters inside quotations.
* The -w switch tells the loader to convert parameters to UNICODE before passing to unmanaged DLL function.
* C# generator by n1xbyte: https://github.com/n1xbyte/donutCS
* Go generator by awgh https://github.com/Binject/go-donut

### Changed

* Command line is no longer parsed using semi-colon or comma as a token. The -p switch now accepts a string with all parameters enclosed in quotation marks. For .NET DLL/EXE, these are separated by the loader using CommandLineToArgvW. For unmanaged DLL, the string is passed to the DLL function without any modification.
* The -u switch to specify URL for HTTP stager is replaced with -s switch to prepare for a DNS stager.
* The -f switch to specify input file is now used to specify output format of loader.

### Removed

* XSL files are no longer supported.
* Code stub for calling DLL function with multiple arguments.
