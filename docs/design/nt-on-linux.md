# NT on Linux: a probe for running real Windows DLLs without a VM

Status: proposed, 2026-10-27. Asks for one decision after the pitch week:
fund a probe of one to two weeks. Nothing here is built or measured.

## Idea

Run Microsoft's own user-mode DLLs (`ntdll`, `kernel32`, `user32`, and the
rest, taken from a Windows install the user is licensed for) as Linux
processes. Linux supplies the NT kernel's side of the interface. This is
WSL1 in reverse: no VM, no Windows kernel, and Windows-level compatibility
for any app the translated syscalls cover.

```text
Windows app (.exe)
└─ Microsoft's user-mode DLLs, unmodified
   └─ syscall instruction (NT syscall numbers of one pinned Windows build)
      └─ trapped by Syscall User Dispatch
         └─ panoptes-nt: NT semantics on Linux
            ├─ files, processes, threads, memory, registry
            ├─ sync primitives through ntsync
            ├─ win32k: windows, messages, GDI, input, mapped to Wayland
            └─ DirectX through DXVK and VKD3D, to Vulkan
```

Wine translates at the DLL layer instead and reimplements the DLLs. The
probe tests whether running the real DLLs is practical and gives better
compatibility.

## What Linux already provides

| Piece | Role | Status |
|---|---|---|
| Syscall User Dispatch, `prctl(PR_SET_SYSCALL_USER_DISPATCH)` | Traps syscalls made from chosen code regions into a userspace handler | Mainline since Linux 5.11, added for Wine and Proton |
| ntsync, `/dev/ntsync` | NT mutexes, events, semaphores, and wait-for-multiple in the kernel | Mainline, added for Wine (version to confirm) |
| Published NT syscall tables | Syscall numbers per Windows build, from public research | Exist; one pinned build makes numbers stable |
| DXVK, VKD3D | Direct3D 9 to 12 on Vulkan | Mature; shipped in Proton |

## Hard parts

1. win32k. The NT kernel has roughly 470 syscalls; the GUI subsystem adds
   over a thousand for windows, messages, GDI, and input. Mapping them onto
   Wayland is most of the work. The probe skips the GUI.
2. Semantics. Numbers are easy; matching each call's behavior in every case
   the real DLLs rely on is the long tail.
3. Churn. Windows builds change numbers and internals. Each release pins
   one build; a new build is a port.
4. Law. Reverse engineering for interoperability is generally protected
   (US: 17 USC 1201(f); EU: Directive 2009/24/EC, Article 6). Microsoft's
   DLLs cannot be redistributed, so users supply them. Counsel reviews
   this before any release.

## The probe

| Step | Passes when |
|---|---|
| 1. Load a console `.exe` with the real `ntdll` and `kernel32` from one pinned build, trapping every syscall | The process reaches `main` and every syscall is logged with its arguments |
| 2. Implement the calls a "hello world" and a file-copy tool make | Both run and produce the same output as on Windows |
| 3. Conformance harness: run each traced call on real Windows and on panoptes-nt, diff the results | The harness runs unattended; LLM agents write stubs and tests against it |
| 4. Count | The number of syscalls, and of their argument cases, that a target app class needs |

Step 4 decides the next step: the count of win32k calls a simple GUI app
needs sets the size of the GUI project.

## Why LLMs change the cost

Most of the work is mechanical and checkable: document behavior from traces,
generate stubs, and write differential tests against real Windows. A tight
diff loop makes agent-written code verifiable. Estimate, not measured: a
small focused team could reach useful GUI coverage in one to two years,
against a decade for Wine's volunteers.

## Decision requested

Fund the probe after the pitch, or drop it. Until then, Windows apps run
under Wine as ordinary kiosk processes (E14 measures one).
