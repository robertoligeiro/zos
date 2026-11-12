# ZOS - Zombie OS: Project Plan, Research and Execution Record

Updated: **November 11, 2026 (Pacific time)**.

This root-level document consolidates the original implementation plan, work
already executed, USB/laptop investigations, source and licensing research,
and the deferred shell/desktop implementation prompts. The original plan and
the complete previously researched roadmap are retained below.

## Read this status before executing any prompt

- **Product:** ZOS - Zombie OS, an original Rust x86-64 microkernel.
- **Repository:** the repository root; current integration branch:
  `main`. No remote is configured and nothing has been published.
- **Accepted source baseline:** `e5495c6`, following tags `phase-1` through
  `phase-9`. Those tags are historical checkpoints, not claims that all
  physical PCs work.
- **Current source:** includes the unfinished generic-PC hardware work and
  this consolidated plan, committed to `main` at the user's explicit request.
  Saving that work does not promote it to a verified hardware checkpoint.
- **Last generated USB package:** the previously accepted 34-scratchpad
  diagnostic build, not the generic-PC draft. Its local binaries were later
  removed at the user's cleanup request; metadata and test evidence remain.
  There is currently no complete local boot image or USB staging package.
- **Policy gate:** implementation, new boot-package generation and deployment
  of the generic-PC work remain paused pending source/license and applicable
  organizational policy clearance. The user separately authorized this local
  documentation/consolidation commit; that is not compliance approval.
- **Deferred:** Shell 2.0, windowing and desktop applications have not been
  implemented or released for implementation.

The status and gates in this execution record supersede stale status statements
in the historical plan below. In particular, the physical 34-scratchpad retest
has now succeeded at controller initialization, but USB3 device enumeration and
integrated laptop input have not been demonstrated on that laptop.

## Execution record: completed foundations

### Steps 0-1: native portable tooling, naming and local persistence

Executed:

- Created the local Git repository and Rust workspace, using ZOS branding rather
  than the original plan's `mkos` placeholder.
- Selected portable Windows tooling instead of WSL/system installation:
  Rust/Rustup/Cargo, the GNU Windows host linker, QEMU, Limine and extraction
  tools under ignored `.tools`.
- Pinned Rust `nightly-2025-03-01`, the `x86_64-unknown-none` target and required
  components. Separated host test builds from freestanding user builds.
- Added `tools\bootstrap.ps1` and the Python build/image/run/test interface.
  Bootstrap does not silently elevate or change the machine-wide PATH.
- Fixed the user's first build failure: `spin = "=0.9.8"` was yanked. Pinned
  `=0.9.9`, committed `Cargo.lock`, and made Cargo builds/checks use `--locked`.
- Established per-phase local commits/tags and a complete-history source bundle
  at `build\zos-source.bundle`. Tool installations, images and generated
  acceptance evidence stay outside tracked source.

Initial commits: `2c19f52` (workspace), `2bce050` (boot source/tooling),
`213e298` (dependency and acceptance-runner fixes). No remote was added.
Earlier script denials were not bypassed; the user subsequently installed
the portable environment.

### Step 2: BIOS/UEFI boot and observable CPU initialization

Executed:

- Built a higher-half `no_std`/`no_main` x86-64 kernel with Limine boot requests
  for framebuffer, memory map, HHDM, executable address and firmware identity.
- Implemented COM1 serial logging and RGB32 framebuffer text, using embedded
  bitmap glyphs expanded to 8x16 pixels.
- Installed kernel/user GDT descriptors, TSS, privilege and double-fault stacks,
  exception handlers, PIC setup and a 100 Hz PIT.
- Added deterministic boot, divide-by-zero and page-fault acceptance images.
- Implemented a standard-library Python GPT/FAT32 boot-image builder with
  checked structures and BIOS plus UEFI boot files.
- Fixed acceptance VM termination and replaced a blocked localhost QMP listener
  with sequential, bounded QMP requests over subprocess pipes.

Verified: 10 host tests, formatter/Clippy, all six BIOS/UEFI boot/fault cases,
and both normal framebuffers, including visual inspection. Tagged `phase-2`
at `213e298`. Deliberate fault tests are not unexpected kernel failures.

### Step 3: physical memory, address spaces and heap

Executed:

- Added a bitmap frame allocator using only Limine-marked usable RAM, reserving
  allocator metadata, boot responses and still-live bootloader memory.
- Took ownership of page-table hierarchies; implemented 4 KiB
  map/unmap/protect, private lower-half process mappings, and shared
  supervisor-only kernel mappings.
- Added an 8 MiB coalescing kernel heap and usable/free/used memory reporting.
- Enforced ownership distinctions between allocated frames and borrowed
  device mappings; enabled relevant write-protection/NX checks.

Verified allocator/mapping/heap behavior and protection/fault acceptance in
BIOS and UEFI. Persisted as `7017502`, tag `phase-3`.

### Step 4: threads, ring 3, checked syscalls and synchronous IPC

Executed:

- Added ELF loading, private user mappings, 32 KiB kernel stacks, 64 KiB user
  stacks with guard gaps, and ring-3 entry/SYSCALL/SYSRET.
- Implemented preemptive priority scheduling with round-robin peers and
  single-CPU interrupt-safe scheduler state.
- Added bounded channels, authenticated Send/Receive/Reply, closure/error
  behavior, dependency-cycle rejection and transitive priority inheritance.
- Added `libos`, checked user-buffer copying, narrow I/O/IRQ capabilities, and
  an echo-server/client demonstration.
- Kept user faults contained to the offending process/thread rather than
  crashing the kernel.

Verified process execution, IPC, priority/isolation behavior and user-fault
containment in both firmware modes. Persisted as `4eeebdc`, tag `phase-4`.

### Step 5: framebuffer console, PS/2 input and editable TTY

Executed:

- Split framebuffer console, input and TTY into ring-3 services.
- Kept the console as sole writable framebuffer owner; added scrolling,
  colors, text cursor and a pointer/crosshair with button feedback.
- Implemented scan-code-set-1 PS/2 keyboard decoding, modifiers/extended keys,
  a three-byte PS/2 mouse, bounded controller drains and explicit errors.
- Added insertion/deletion, cursor navigation and eight-entry history with
  draft restoration. Input was initially an echo demonstration, not a shell.
- Restricted IRQ1/IRQ12 and ports 0x60/0x64 to the authorized input process.

Verified host input/rendering logic and native BIOS/UEFI input/capability
regressions. Persisted as `a62a974`, tag `phase-5`.

### Step 6: persistent block service, fs-simple and VFS

Executed:

- Added separate ring-3 secondary-master ATA PIO block, filesystem and VFS
  servers. Primary/boot-disk ATA ports remained denied.
- Implemented a custom bounded hierarchical filesystem with copy-on-write data,
  two checksummed metadata checkpoints and ordered flushes.
- Added host formatting for explicitly named generated data volumes; never
  reformatted an existing user volume just to obtain a test fixture.
- Added path lookup, owned file descriptors and read/write/create/delete/
  rename operations behind bounded service protocols.
- Extended capability-bearing service IPC to 1024 bytes while retaining
  64-byte ordinary-process and debug-write bounds.
- Added corruption, size/inode-limit, crash/torn-write and restart-persistence
  acceptance, including transaction-boundary fault injection.

Verified patterned data across complete QEMU exit/relaunch, not just an
in-guest reset. Acceptance included 34 host tests, 13 Clippy configurations
and 22 named BIOS/UEFI cases, with framebuffer review. Persisted as `238b304`,
tag `phase-6`.

### Step 7: process manager and actual interactive shell

Executed:

- Added a ring-3 process manager with path-based ELF spawn, kill, wait/status,
  generation-checked reusable PIDs/channels and process/memory reporting.
- Added authenticated lifecycle RPCs, checked argument startup packets,
  automatic VFS descriptor cleanup and full process-resource reclamation.
- Implemented a native Rust shell with quotes/escapes, `;`, `>`, `>>`, background
  spawn, and 120-character editable input. It is not GNU Bash.
- Delivered `help clear ls cd pwd mkdir rmdir touch cat echo rm cp mv free ps
  kill wait uname reboot halt`, using existing command dispatch and VFS.
- Verified the requested `mkdir a; cd a; echo hi > f; cat f; ps; free` session,
  append, filenames containing spaces, parser errors, editing/history,
  background processes, stale handles and memory recovery.
- Tested actual guest reboot and kernel halt after filesystem synchronization.
  The user also personally booted UEFI and exercised `ls`, `help` and `ps`.

Verified 43 host tests, 14 Clippy configurations and 24 named BIOS/UEFI cases,
including persisted file contents and eight reviewed captures. Persisted as
`dba23aa`, tag `phase-7`. Unsupported commands such as `more`/`dir` are not
silently treated as implemented utilities.

### Step 8: QEMU xHCI input and persistent USB storage

Executed under the user's QEMU-first, no-physical-drive-write scope:

- Added PCI xHCI discovery, checked uncached/NX BAR mappings, driver-owned DMA
  pages, controlled bus mastering and teardown ordering in the kernel.
- Implemented xHCI command/event/transfer rings, cycle/link handling,
  32/64-byte contexts, enumeration, boot HID keyboard/mouse and BOT/SCSI
  storage in the existing ring-3 input service.
- Brokered USB block operations to the existing storage service without moving
  filesystem or device policy into the kernel.
- Fixed controller initialization ordering: reset, enable DMA, program
  controller structures, then run.
- Fixed the Input -> Shell -> FS -> Block -> Input IPC cycle with restricted
  `SendReady`: queue input until a recipient is receiving rather than blocking
  storage progress. Verified queued keyboard events during real file copying.
- Added `--usb`, separate `build\data-usb.img`, actual restart persistence,
  ring-wrap and DMA-capability proofs, negative-device tests and disconnect
  reporting. Existing PS/2/ATA boot remained available.

Verified 49 host tests, 16 Clippy configurations, 40 named BIOS/UEFI cases and
14 reviewed framebuffers; user data volumes were preserved. The user also
personally booted the QEMU USB shell. Persisted as `608c348`, tag `phase-8`.
This established an emulator baseline, not generic physical-PC compatibility.

### Step 9: read-only laptop diagnostic and physical USB preparation

Executed:

- Added an explicit `usb-diagnostic` build that starts only console and input:
  no shell, process manager, mounted filesystem or block service.
- Added bounded firmware-ownership handoff, scratchpad support, capability/
  raw-register/port reporting, storage capacity discovery and first-sector
  reads. Initially restricted physical support to Intel `8086:4ded`.
- Enforced a diagnostic SCSI allowlist before submission. Writes, flushes and
  all block RPCs are denied; arbitrary reported capacity does not enable
  writable filesystem mounting.
- Added BIOS/UEFI diagnostic cases for USB boot, strictly read-only media,
  missing storage, empty controller, unsupported HID and missing controller.
- Investigated a bundled EDK2 USB mass-storage firmware fault on write-protected
  media. UEFI USB-boot acceptance uses a disposable snapshot overlay; an
  independent case boots IDE and inserts strictly read-only USB after firmware
  exits. Neither workaround enables ZOS storage writes.
- Added a five-file hashed UEFI diagnostic package and
  `tools\prepare-usb.ps1`: explicit disk selection, preview/WhatIf, typed erase
  confirmation, target-identity checks, 512 MiB FAT32 boot volume, exact file
  copying and readback verification. Remaining space stays unallocated.
- Added mocked PowerShell tests. The assistant's script-execution path was
  denied; the user ran those tests externally and supplied exit code 0.
  Physical preparation was user-operated, not performed by the assistant.

Persisted diagnostic baseline: `8670397`, tag `phase-9`. Subsequent USB-script
fixes were committed separately:

- Removed duplicate `ErrorAction` declaration from an advanced-function test
  mock; PowerShell already supplies that common parameter.
- `aa6f921`: fixed preparation of disks already initialized after partition
  clearing; do not unconditionally call `Initialize-Disk`.
- `602d6cb`: do not recreate an existing drive root while copying package files.
- Retained RAW-disk handling, refusal/preview checks and identity/hash tests.
  A successful mock suite is not evidence that real media was written safely.

The script deliberately refuses ambiguous/non-USB/system/boot/changed targets.
Disk numbers are not stable: always identify the current intended USB disk
before a separately authorized write. No ZOS installer for internal disks exists.

## Physical-laptop investigation and observed results

### Early rejection: make the failing capability observable

The first physical photo showed `Unsupported` before capability information.
Two early rejection paths were found: excessive scratchpads and DMA pages
outside the driver's 32-bit physical-address range.

Commit `3da1127` moved capability reporting before DMA allocation, printed raw
HCSPARAMS values, added the explicit `DmaAddress` diagnostic and bounded DMA
tests, and made native acceptance check that ordering. Verified 20 host tests,
formatter/Clippy, 12 diagnostic firmware cases, two normal USB-shell boots,
14 reviewed framebuffers, package identity and unchanged user volumes.
Evidence: `build\intel-early-probe-acceptance.json`.

### Photographed 34-scratchpad requirement

The next photo reported:

```text
XHCI CAP: version=0x110 ports=14
XHCI RAW: hcs1=0x0e000840 hcs2=0x14200054
XHCI REJECT: scratchpad count exceeds 32
```

Decoding the two scratchpad-count fields gives `(1 << 5) | 2 = 34`.
Commit `e5495c6` raised the accepted limit to 34, shared the bound between policy,
allocation and reporting, and added the exact photographed-register regression
plus boundary checks.

The then-current worst-case DMA budget was 4 controller pages + 1 scratchpad
pointer page + 34 scratchpads + 3 device slots of 8 pages = 63 of 64 pages.
The kernel quota and 32-bit-address restriction were not silently increased.
The same focused host/native checks passed, with source/package/image
fingerprints and preserved user volumes recorded in
`build\intel-scratchpad-acceptance.json`.

### Latest physical result: controller starts, USB3 device is skipped

The user's later laptop boot demonstrated:

```text
XHCI CAP: version=0x110 ports=14
XHCI RAW: hcs1=0x0e000840 hcs2=0x14200054
XHCI CAP: scratchpads=34 DMA=32-bit
XHCI protocol USB2: ports 1+8
XHCI protocol USB3: speeds unsupported
XHCI protocol USB3: ports 9+6
XHCI ownership: requesting firmware handoff
XHCI running: context=32 scratchpads=34
USB PORT 14: status=0x000012b1
USB SKIP port=14: unsupported protocol
DIAGNOSTIC ENUMERATION: 0 supported devices
DIAGNOSTIC READ ONLY PASS: WRITE/FLUSH/RPC denied
DIAGNOSTIC READY: type keys / move mouse; no shell
```

This confirms the 34-scratchpad correction and controller handoff/run on real
hardware. It does not confirm USB mass-storage enumeration, built-in keyboard/
touchpad input, a physical shell or persistence.

The user correctly clarified that normal laptop operation should use integrated
input, not require an external keyboard/mouse. Existing legacy initialization
required both a PS/2 keyboard and a mouse, while USB diagnostic mode granted
neither legacy port. USB support alone does not supply an I2C/SPI touchpad driver.

## Generic-PC USB research, design and unfinished implementation

The user authorized trying a broader modern-PC baseline rather than another
single-controller workaround. Work began on `hardware/generic-pc-usb`, then was
paused for the user's open-source/organizational policy condition. The pending work
is now saved on `main` by explicit request.

### Research conclusions and compatibility boundaries

- QNX's reusable pattern is layered: controller drivers, a USB core and class
  drivers. Its historical disk demo targeted a defined contemporary hardware
  baseline; it did not provide universal support for arbitrary future laptops.
- Modern-PC USB support should be capability-driven rather than a list of PCI
  vendor/product IDs. The PCI class match remains xHCI, with exclusive MMIO/DMA
  capabilities and bounded resource allocation.
- Supported Protocol capabilities define controller-local speed IDs. A raw
  PORTSC ID is not a universal semantic speed enum. Decode PSI rate/exponent
  and symmetric or paired asymmetric entries, retain the hardware ID for slot
  contexts, and use the decoded speed for endpoint behavior.
- USB2 reset and USB3 enabled/warm-reset handling differ; a universal USB2
  Port Reset write is not a complete USB3 policy.
- Composite interfaces, alternate settings, hub routes/transaction translators
  and optional class-request stalls are separate protocol responsibilities.
  Removing a rejection alone does not implement those responsibilities.
- Built-in input may use PS/2, USB or HID over I2C/SPI. Integrated PS/2 keyboard
  initialization must not depend on a PS/2 mouse being present.
- Emulator success is necessary but does not establish support for "most
  recent laptops." Maintain a tested-hardware matrix and explicit exclusions.

### Draft changes saved, not accepted

1. `libos\src\lib.rs`: shared bounds of 8 device slots, 64 scratchpads and
   `5 + 64 + 8 * 8 = 133` DMA pages.
2. `kernel\src\process\hardware.rs`: PCI class-based controller discovery in
   place of the vendor-ID allowlist; select the first matching controller and
   explicitly report additional controllers. Still one owned controller,
   4-64 KiB BAR, trusted DMA and no IOMMU.
3. `servers\console\src\usb\policy.rs`: controller-defined speed-ID mapping,
   nominal USB2/SuperSpeed rate classification, paired asymmetric PSI validation,
   implicit tables when PSI is absent and expanded scratchpad-bound tests.
4. `servers\console\src\usb\xhci.rs`: protocol/slot-type handling in both USB
   modes, bounded port power/reset changes, semantic versus raw speed use,
   optional GET_MAX_LUN/SET_IDLE stall recovery, larger device accounting and
   boot-time USB2 hub routing/TT configuration.
5. `servers\console\src\usb\descriptors.rs`: bounded selection of one supported
   interface from a composite configuration, alternate-interface selection,
   USB2 hub class recognition and SuperSpeed companion fields.
6. `servers\console\src\ps2.rs`: new bounded, testable optional integrated
   keyboard/mouse probing; missing mouse no longer implies missing keyboard.
   Diagnostic mode alone receives the legacy port grants and polls this input.
   Normal USB mode retains its no-PS/2-port isolation checks.
7. `tools\zos.py` / `tools\test_usb.py`: draft USB2, SuperSpeed, NEC-controller,
   hub, integrated-input and absent-i8042 diagnostic cases and command tests.

These are implementation attempts, not verified support claims. Remaining
limits include one controller/one selected class interface per device, no UAS,
no I2C/SPI HID, no SuperSpeed hubs, no complete hotplug/recovery lifecycle,
32-bit DMA addresses, and unchanged normal writable-disk geometry restrictions.
The draft is not an accepted replacement for the existing USB diagnostic.

### Validation actually executed on the saved draft

Before the policy pause, 18 Rust console-library tests and freestanding console
Clippy passed on an intermediate draft. Later edits had not yet been validated
when the pause was requested.

For this user-authorized consolidation, bounded pre-commit validation was run
again on the final saved source, without building images or resuming hardware
feature development:

```text
Rustfmt: cargo fmt --all, then cargo fmt --all -- --check
Rust host: 18 console-library tests, x86_64-pc-windows-gnu
Python: 6 tests in tools\test_usb.py
Clippy: freestanding zos-console, warnings denied
Clippy: zos-kernel, no defaults + usb-diagnostic, warnings denied
```

The tests cover rate-based IDs, asymmetric pairs, descriptor bounds, composite/
alternate selection, integrated keyboard-without-mouse behavior, SCSI denials,
and test-command construction. The topology tests check host arguments, not
guest behavior.

**Not executed on this draft:** complete regression suite, new QEMU topology
cases, native hub/endpoint-recovery acceptance, framebuffer inspection or a
physical-laptop retest. No new phase tag was created. Compilation and host tests
are not substitutes for those gates.

The preexisting diagnostic artifacts were verified unchanged during
consolidation, before the subsequent user-requested binary cleanup:

```text
build\zos-diagnostic.img
  SHA256 d8c880c1ca12f0e683b0647958edd29a5a786f7ba4a05b772ef9e56731d82a47
build\zos-diagnostic.elf
  SHA256 431a1282629210f3850888966137a6f00c1af90e418fb2a4becb8e20fb9fe323
build\usb-diagnostic-files\manifest.json
  SHA256 17099dae2f2f134d1050b7168c0523e3825dfe0ab3e3d32967e376e0501a32a7
```

### User-requested generated-output cleanup

On November 11, 2026 (Pacific time), the user requested removing binaries that
can be regenerated. Removed 1,293 files totaling 2,299,518,247 bytes (2.14 GiB):

- The Rust `target` build cache and `tools\__pycache__`.
- Generated `build\zos*.img` boot images and `build\zos*.elf` executables.
- Generated EFI/ELF binaries inside `build\usb-diagnostic-files`.
- Generated per-run UEFI variable-store copies and the old
  `build\zos-source.bundle`, which can be recreated from the retained Git history.

Preserved source, `.git`, `.tools`, all 19 data images, screenshots, logs,
acceptance reports, and the USB package's configuration/manifest. Verified all
289 retained build files against their pre-cleanup SHA-256 hashes. The manifest
and artifact hashes above are historical evidence, not proof that the removed
binaries remain available. No physical USB disk was changed.

No binaries were rebuilt as part of cleanup. The generic-PC implementation and
boot-package policy gate remains unchanged. The source bundle was not refreshed,
because doing so would recreate an artifact just removed at the user's request.

### Remaining implementation/acceptance steps after the policy gate

1. Review the saved draft against the authoritative specifications, including
   asymmetric speed tables, endpoint recovery, hub context/TT setup, topology
   limits, descriptor selection, resource accounting and safe cleanup.
2. Run focused host/format/Clippy checks, then genuine USB2/SuperSpeed,
   alternate-controller, hub and integrated-input QEMU cases in BIOS and UEFI.
   Fix failures rather than interpreting command-construction tests as native
   acceptance. Verify storage reads and input from the intended transport.
3. Preserve diagnostic write denials; run missing-device, unsupported-device,
   controller-absence and disconnect regressions. Use dedicated disposable
   images, fingerprint normal user volumes and inspect actual framebuffers.
4. Re-run the normal persistent USB shell and legacy PS/2/ATA regressions.
   Confirm initialization failures and cleanup do not leak slots/DMA or leave
   bus mastering active over released memory.
5. Only after license/policy and software acceptance gates, generate a distinct
   candidate package, verify its five file hashes and ELF/source identity, and
   record provenance. Do not silently overwrite evidence for the accepted build.
6. Have the user separately authorize/re-identify the physical USB target and
   prepare it with the guarded script. Record actual device IDs, PSI/port output,
   storage/read-only proofs and integrated-input behavior from the laptop.
7. Publish only tested compatibility claims. A real hardware-ready shell still
   requires normal-mode input, selected-USB filesystem writes and persistence,
   not merely diagnostic readiness.
8. Leave Steps 10-12 deferred until explicitly released; no shell/GUI work is
   hidden inside this hardware task.

## External-source and license/policy research

### User condition and current decision

The user required open-source inputs and no violation of organizational policy,
otherwise no further implementation. Work was paused; no compliance approval
has been obtained or inferred. Later authorization to commit all work locally
does not authorize publishing, release or third-party code incorporation.

No dependency manifests or lockfile were changed by the generic-PC work.
No QNX or Linux driver code was copied in that work. QNX/Photon is UX/history
inspiration, not a source-code/runtime/artwork dependency. The earlier xHCI
investigation did consult an official QEMU source reference; that consultation
is not a completed code-origin audit of the whole project's history.

Publicly readable documentation is not automatically open-source software:
the Intel specification and QNX manuals are documentation references, not
permission to copy their implementation or assets.

### Local dependency inventory inspected

Offline `cargo metadata --locked --offline --format-version 1` reported these
existing external packages. These are package license declarations, not a
complete legal review of every file or transitive notice:

| Package | Locked version | Declared license expression |
| --- | --- | --- |
| bit_field | 0.10.3 | Apache-2.0/MIT |
| bitflags | 2.13.2 | MIT OR Apache-2.0 |
| font8x8 | 0.3.1 | MIT |
| limine (Rust bindings) | 0.6.5 | MIT OR Apache-2.0 |
| linked_list_allocator | 0.10.5 | Apache-2.0/MIT |
| lock_api | 0.4.14 | MIT OR Apache-2.0 |
| rustversion | 1.0.23 | MIT OR Apache-2.0 |
| scopeguard | 1.2.0 | MIT OR Apache-2.0 |
| spin | 0.9.9 | MIT |
| volatile | 0.4.6 | MIT OR Apache-2.0 |
| x86_64 | 0.15.2 | MIT/Apache-2.0 |

The pinned Limine bootloader's local `.tools\limine\LICENSE` was read separately
and contains the two-clause BSD redistribution conditions. The Rust bindings
and bootloader binary are different components. Redistribution notices and
third-party binary contents still need review.

The local ZOS workspace packages currently declare no project license. Do not
invent ownership, choose a redistribution license or add a corporate copyright
statement without authorization.

Build/test tools and bundled firmware also require their own inventory;
examining Rust crate metadata does not clear Rust/LLVM runtime material,
MinGW/GCC, QEMU/EDK2 firmware, Limine binary contents or extraction tools.
Using a GPL-licensed test tool is not the same operation as incorporating its
source into ZOS; record the actual usage rather than assuming either automatic
approval or automatic incompatibility.

### Outstanding compliance work

- Identify the authoritative organizational policy/process applicable to this
  project and intended use/distribution; no project-specific approval or
  registration evidence has been supplied.
- Verify actual component license/notice files, optional features, bundled
  assets/binaries and code origins, rather than relying only on metadata.
- Resolve any required component registration, approval and notice/attribution
  obligations before continuing under the user's condition.
- Do not claim that public open-source guidance is a substitute for internal
  policy or that permissive license labels alone establish compliance.

This records an unresolved gate, not a legal finding that the project violates
policy. Do not bypass it by importing another implementation.

## Additional research references and evidence boundaries

The eight shell/Photon references Q1-Q5 and B1-B3 are retained in full at the
end of the deferred roadmap below. The following captures the USB/generic-PC
and compliance references used or attempted in the investigation:

- **U1 - xHCI specification:** Intel, Extensible Host Controller Interface.
  Capability/PSI, port/reset, context, ring and transfer-recovery reference;
  documentation, not an OSS driver implementation.
  `https://www.intel.com/content/dam/www/public/us/en/documents/technical-specifications/extensible-host-controler-interface-usb-xhci.pdf`
- **U2 - QNX USB layers:** QNX custom BSP documentation, USB subsystem.
  Controller/core/class-driver architecture reference; not imported code.
  `https://www.qnx.com/developers/docs/8.0/com.qnx.doc.neutrino.custom_bsp/topic/usb.html`
- **U3 - QNX generic-x86 driver inventory:** identifies specific supported
  controller drivers rather than a universal-hardware guarantee.
  `https://www.qnx.com/developers/docs/BSP8.0/com.qnx.doc.bsp_x86_64.generic_8.0/topic/driver_commands.html`
- **Q0 - Historical demo-page reference:** archived QNX demo description.
  Edition-specific historical material, not code/assets licensed for reuse.
  `https://fsck.technology/software/QNX/QNX%204/QNX4/QNX%20Demo/about_QNX4/Demo%20Disk%28Modem%20Version%204%29.htm`
- **L2 - Limine upstream:** project/license reference. The exact local pinned
  bootloader license was inspected; do not substitute a future upstream license
  for the version actually distributed.
  `https://github.com/limine-bootloader/limine`
- **R1 - Attempted Linux xHCI header cross-check:** retrieval returned HTTP 429;
  no header contents were obtained or used as implementation evidence.
  `https://raw.githubusercontent.com/torvalds/linux/master/drivers/usb/host/xhci-ext-caps.h`
- **R2 - Attempted Linux hub cross-check:** likewise HTTP 429, not retrieved.
  `https://raw.githubusercontent.com/torvalds/linux/master/drivers/usb/host/xhci-hub.c`

Generated reports/logs under `build` are local evidence, not tracked release
artifacts or a guarantee that a fresh clone has the same binaries. Existing
reports describe their own commits; do not relabel old acceptance as proof for
the unfinished source now on `main`.

## Deferred research already completed, implementation not started

A documentation-only branch, `planning/shell-photon-desktop`, commit `80cbcdc`,
preserved the researched roadmap while USB work remained on the accepted
baseline. The original Downloads plan's 8,245-byte prefix was preserved in
that research version, and its versioned copy matched the extended file.
The complete-history source bundle was verified with both branches.

The following retained material includes all three detailed execution prompts:
Step 10 (native Bash-inspired shell, streams/scripts/jobs), Step 11 (ring-3
windowing and independent terminals), Step 12 (Files, clock, backgrounds and
small applications), plus edition-specific QNX research and source references.
The user's integrated-laptop-input correction supersedes the older suggestion
that external USB input should be a normal prerequisite.

---

# Original Microkernel OS Plan (historical specification)

The original language and proposed commands below are historical context.
Use current repository tooling and the status/gates above, not old `mkos`
paths, as the execution contract.

## Microkernel OS on a USB Pendrive: Implementation Steps

Goal: a small QNX-style microkernel OS (x86-64) that boots from a USB stick and opens a bash-like shell.
Defaults chosen: **Rust**, **Limine bootloader** (BIOS + UEFI), **QEMU** for testing.

Design recap:
- Kernel (ring 0) does only: memory/address spaces, threads + scheduler, synchronous IPC (`Send`/`Receive`/`Reply`), IRQ forwarding.
- Everything else is a user-space server (ring 3): `procmgr`, `vfs`, `fs-simple`, `blkdev`, `console`, `input`, `shell`.
- Disk = the pendrive only. Never touch the internal disk.

> Tip: you can paste this whole file to Claude Code on the other device and say
> "Execute Step 0 to Step 3, stop and show me the result before continuing."

---

## Step 0: Prepare the machine

Easiest path on Windows is **WSL2 (Ubuntu)**; on Linux/macOS use the native shell.

Install:
1. **Rust via rustup** (https://rustup.rs), then:
   ```bash
   rustup toolchain install nightly
   rustup component add rust-src llvm-tools-preview --toolchain nightly
   rustup target add x86_64-unknown-none --toolchain nightly
   ```
2. **QEMU**: `sudo apt install qemu-system-x86` (Windows native: installer from qemu.org).
3. **Build tools**: `sudo apt install git make xorriso mtools gdisk build-essential`
4. **OVMF** (UEFI firmware for QEMU): `sudo apt install ovmf`

Check:
```bash
rustc +nightly --version
qemu-system-x86_64 --version
xorriso --version
```

## Step 1: Project skeleton

```
mkos/
  Cargo.toml            # workspace
  rust-toolchain.toml   # pin nightly
  kernel/               # the microkernel crate (no_std, no_main)
  servers/
    procmgr/ vfs/ fs-simple/ blkdev/ console/ input/ shell/
  libos/                # shared user-space lib: syscall wrappers, IPC message types
  boot/limine.conf      # bootloader config
  tools/mkimage.sh      # builds the bootable disk image
  Makefile              # build / run / debug targets
```

Create `rust-toolchain.toml` pinning a specific nightly date (so the build stays reproducible).
Create the workspace with the kernel crate first; servers come later.

## Step 2: Phase 1: boot and print to the screen

Deliverable: `make run` opens QEMU and the kernel prints "mkos booting" on screen and to the serial port.

1. Kernel crate: `#![no_std]`, `#![no_main]`, target `x86_64-unknown-none`, custom linker script that places the kernel in the higher half (`0xffffffff80000000`).
2. Use the `limine` crate (check the current version and its docs) to request: framebuffer, memory map, HHDM (higher-half direct map), and kernel address.
3. Write a **serial (COM1, port 0x3F8) logger** first. It is the most reliable debug output, and QEMU shows it with `-serial stdio`.
4. Draw text on the framebuffer with an embedded 8x16 bitmap font.
5. Set up the **GDT** (kernel/user code and data segments plus a TSS) and the **IDT** (CPU exception handlers that print the fault and halt).
6. Set up the **APIC timer** (or PIT first for simplicity) and prove interrupts work with a tick counter.
7. `tools/mkimage.sh`: build a disk image with GPT, an EFI System Partition and the Limine files plus the kernel (see the Limine docs, "Limine bootloader deploy" section for the exact commands for your version; get the Limine binary release matching the `limine` crate).
8. Makefile target:
   ```bash
   qemu-system-x86_64 -m 256M -drive file=mkos.img,format=raw \
     -bios /usr/share/ovmf/OVMF.fd -serial stdio
   ```
   Also add a BIOS-mode run target (no `-bios`) to test legacy boot.

Acceptance: boots in QEMU in both UEFI and BIOS mode, prints text, timer ticks, a deliberate divide-by-zero prints a readable fault.

## Step 3: Phase 2: memory management

1. Parse the Limine memory map, then build a **physical frame allocator** (bitmap or buddy).
2. Own the page tables: map/unmap/protect for 4 KiB pages; build a **per-process address space** (kernel half shared).
3. **Kernel heap** (linked-list or slab allocator) so `alloc` (`Box`, `Vec`) works in the kernel.
4. Expose memory totals/free for the future `free` command.

Acceptance: allocate and free 100k pages, a `Vec` test in the kernel, page-fault handler reports bad addresses.

## Step 4: Phase 3: threads, ring 3, IPC

1. Thread control block, kernel stacks, context switch, **priority scheduler** (preemptive, round-robin per priority).
2. Enter **ring 3** via `syscall`/`sysret` (set up the MSRs).
3. Syscalls (keep it tiny): `ChannelCreate`, `Send`, `Receive`, `Reply`, `ThreadCreate`, `MapMemory`, `IrqAttach`, `IoPortGrant`, `Exit`.
4. **Synchronous message passing**: sender blocks until the receiver replies (copy small messages; map for large ones). Add priority inheritance on the server that serves a client.
5. Load the first user program from a boot module (Limine module = an ELF in the image). Write an ELF loader.
6. Write `libos` with safe wrappers, then a toy "echo server + client" to test IPC.

Acceptance: two user processes exchange messages; a user process crash does not crash the kernel.

## Step 5: Phase 4: console and input servers

1. `console` server: framebuffer text terminal (cursor, scroll, colors, backspace).
2. `input` server: **PS/2 keyboard** (IRQ1, scancode set 1 -> keycodes -> chars) and **PS/2 mouse** (IRQ12). QEMU provides both.
3. Line editing in the shell/tty layer: backspace, arrows, history.

## Step 6: Phase 5: disk and filesystem

1. `blkdev` server: start with **virtio-blk or AHCI/ATA** (easy in QEMU) behind a block interface (`read_blocks`, `write_blocks`). USB comes in Step 8.
2. `fs-simple`: own simple filesystem: superblock, inode table, block bitmap, directories, files, small journal or careful write ordering.
3. `mkfs` tool (host side) to format the data partition image.
4. `vfs` server: path lookup, file descriptors, mount table, and a message protocol (`open/read/write/close/stat/readdir/mkdir/unlink/rename`).

Add to the QEMU command: `-drive file=data.img,format=raw,if=virtio`.

Acceptance: create, write, reboot QEMU, and the file is still there.

## Step 7: Phase 6: process manager and shell

1. `procmgr`: spawn from a path, kill, list (PID, name, state, memory), exit status.
2. `shell`: prompt, parser (quotes, `>` and `>>` redirect, `;`), built-ins and spawned commands:
   `help clear ls cd pwd mkdir rmdir touch cat echo rm cp mv free ps kill uname reboot halt`
3. `free` reads memory stats from the kernel/procmgr; `ps` reads the process table from procmgr.

Acceptance: a session like `mkdir a; cd a; echo hi > f; cat f; ps; free` works.

## Step 8: Phase 7: real hardware

Do this last, and only after everything works in QEMU.

1. **Safety first:** write only to a USB stick you have confirmed. Use `lsblk` (Linux) to identify it. On Windows use Rufus (DD mode) or balenaEtcher. Wrong target = lost data.
2. Write the image: `sudo dd if=mkos.img of=/dev/sdX bs=4M status=progress conv=fsync` (replace `sdX` after double-checking with `lsblk`).
3. In the PC firmware: set boot from USB (try UEFI first; disable Secure Boot, since Limine is unsigned).
4. **Known gaps on real PCs:**
   - USB keyboard/mouse need an **xHCI driver** (PCI enumeration, rings, HID boot protocol). Many UEFI-only machines have no PS/2 emulation.
   - USB storage needs xHCI plus the **USB mass storage (bulk-only, SCSI)** class driver.
   - Interim: have the kernel load the filesystem into a RAM disk from the boot media so the shell works without a USB driver (non-persistent).
5. Order: PCI enumeration -> xHCI controller -> HID keyboard -> mass storage -> persistence.

## Working agreements

- Commit after every phase; tag `phase-N`.
- Always keep the serial log on; when something breaks, the serial output comes first.
- Debug: `qemu-system-x86_64 ... -s -S` then `gdb` + `target remote :1234`; add `-d int,cpu_reset -no-reboot` for triple-fault diagnosis.
- Do not skip acceptance checks between phases.
- Prefer small, reviewed unsafe blocks, each with a comment explaining the invariant.

## Open decisions to revisit

- Large-message transfer: copy vs shared mapping (start with copy).
- Filesystem: custom simple vs adopting FAT32/ext2 for easy interop with other OSes (FAT32 would let you read/write the stick from Windows).
- SMP (multi-core): single core until Step 7 works.

---

# Deferred ZOS Roadmap: Shell 2.0 and a Photon-Inspired Desktop

**Status: research and implementation prompts only. Do not execute these new phases yet.**

The project name is **ZOS - Zombie OS**. The original plan above is preserved as historical context; its `mkos` placeholders are not the product name.

Numbering follows the repository's implemented checkpoints rather than renumbering the original nested phase labels: **Step 9** is the existing physical-USB diagnostic/bring-up work; **Steps 10-12** below are the proposed next implementation phases.

## Execution gate and Git separation

- Keep USB bring-up on its existing branch. Save this roadmap on a documentation-only planning branch; do not start a shell or GUI implementation branch until the user explicitly resumes that work.
- First record the accepted physical-USB result, hardware, image/commit, and remaining limitations. The latest 34-scratchpad Intel fix still needs a physical retest.
- A read-only `DIAGNOSTIC READY` result proves only the diagnostic path. It does **not** prove that the laptop can run the normal shell, writable root filesystem, or the future desktop.
- Recommended laptop-ready acceptance: normal-mode boot, working USB keyboard/mouse, shell interaction, writes to the selected USB filesystem, persistence after reboot, and no internal-disk access. If only diagnostic readiness is available, a future QEMU-only implementation start requires explicit user direction and must be described as such.
- When implementation is authorized, branch from the then-accepted USB baseline. Do not fork from an obsolete diagnostic snapshot or overwrite the diagnostic image with a writable normal-mode image.
- Retain the current CLI boot path and a separate read-only diagnostic build throughout these phases.

## Research: what the QNX disk demo and Photon actually offered

The relevant inspiration is the **QNX 1.44 MB demo disk and Photon microGUI**, not a modern QNX desktop distribution. A reproduction of Dan Hildebrand's 1997 announcement lists a graphical file browser/editor, Voyager browser and embedded web server, networking/dialer, a set-top-box simulation, and vector animation alongside Photon (Q1).

The full Photon manuals describe additional desktop facilities, but those manuals are **not a manifest of every floppy-demo edition**. In particular, the later Neutrino shelf/clock/resource widgets must not all be attributed to the 1997 disk.

| Feature | Evidence and edition | Proposed ZOS treatment |
| --- | --- | --- |
| Compact windowed GUI; graphical file browser and editor | Original demo announcement, reproduced in Q1 | Core inspiration: a small original native desktop, not a Photon port. |
| Application launcher, taskbar, multiple terminal sessions, virtual workspace navigation | Full QNX 4.25 / Photon 1.14 desktop guide, Q2 | Launcher/taskbar and independent terminal windows first; a small workspace switcher later. |
| File list, path navigation, favorites, sorting, context menus, copy/move/rename/delete, open-with behavior | Full Photon file-manager guide, Q3 | VFS-backed Files application with real operations and explicit errors. |
| Background color, backdrop selection, window appearance/focus choices, screen savers | Full Photon customization guide, Q4 | Persistent colors/patterns first; small tiled images only within measured limits. |
| Right-hand shelf, date/time display, configurable clock, resource monitor | Later QNX Neutrino 6.3.2 Photon guide, Q5 | Optional compact right shelf; clock and real resource counters, clearly a later-Photon-inspired addition. |
| Calculator, help viewer, image viewer, screenshots, appointment book, CD player | Full Photon desktop guide, Q2 | Editor, calculator, offline help and system information are useful follow-ups. Media/appointment tools are not required initially. |
| Web browser/server, dialer, remote-control demo, vector animation | Demo announcement, Q1 | Defer networking/browser/server. A small local animation can be an optional graphics test, not a substitute for useful apps. |

### Recommended ZOS look and interaction

Use an original, restrained late-1990s desktop: compact bitmap typography, light beveled controls, dark title bars with a green active-window accent, a muted selectable background, and a visible pointer. These are **proposed ZOS design choices**, not a claim that every demo used this exact layout.

Start with one workspace, a bottom taskbar/clock and a launcher containing Files, Terminal, Settings and Help. A collapsible right shelf and a two-workspace overview can follow after the basic desktop is accepted. Do not try to reproduce Photon's entire nine-console environment immediately.

Proposed layout, not an implementation or a copied QNX screenshot:

```text
+--------------------------------------------------------------------+
| ZOS - Zombie OS                           selectable color/pattern  |
| +----------------------------+  +-------------------------------+   |
| | Files                 _ [] X|  | Terminal                 _ [] X|  |
| | Path: /                    |  | ZOS:/$                        |   |
| | Favorites | Name     Size  |  |                                |   |
| | /         | bin/           |  | real shell session             |   |
| | /bin      | welcome.txt    |  |                                |   |
| +----------------------------+  +-------------------------------+   |
|                                                                    |
| [ZOS] [Files] [Terminal] [Settings]       [USB root] [date / time]     |
+--------------------------------------------------------------------+
```

### Why not simply install GNU Bash or a full GUI toolkit?

The current shell is native Rust software, **not GNU Bash**. GNU Bash includes substantially more interactive and scripting behavior (B1-B3). The recommendation is a well-defined **Bash-inspired ZOS shell**, with a published compatibility subset and honest limitations. A literal GNU Bash port would first need a separate libc/POSIX/process/terminal compatibility effort; renaming the existing shell would not accomplish that.

Similarly, use a small native ZOS graphics/client library rather than introducing X11, Wayland, Qt, GTK, a browser engine, or the proprietary Photon runtime. Preserve the microkernel architecture: services and applications remain in ring 3.

## Current implementation baseline to re-audit before coding

These are observed repository limits, not permanent design goals. Recheck them at implementation time.

- `servers\session\src\parser.rs`: 120-byte input, 12 words per command, eight commands; quotes/escapes, `;`, `>`, `>>`, and `&`. Its parser removes quote context early; correct expansion needs a token/AST design that preserves that context.
- `servers\session\src\shell.rs`: `help clear ls cd pwd mkdir rmdir touch cat echo rm cp mv free ps kill wait uname reboot halt`; executable lookup currently uses `/bin` or an explicit path.
- `servers\session\src\command.rs` supplies the existing external command implementation; reuse existing command dispatch and helpers where appropriate.
- The current TTY has eight history entries, draft restoration and basic cursor editing. Existing background spawn/kill/wait is not equivalent to terminal job control.
- Service IPC is copied and bounded at 1024 bytes. There are currently 32 thread slots and 16 channel slots. Do not allocate a channel/thread per widget or assume that a pipe is an existing channel abstraction.
- Each process currently has a 64 KiB user stack and a 128-page dynamic-mapping limit, approximately 512 KiB. The kernel heap is 8 MiB.
- The requested RGB32 framebuffer is 1024 x 768. A full-size software backbuffer would require 3,145,728 bytes, exceeding that per-process dynamic-map budget. The hardware framebuffer mapping and newly allocated backing storage are different resources.
- `servers\storage\src\lib.rs`: 64 inodes, 24-byte name storage, and a 65,536-byte maximum file. `servers\storage\src\vfs.rs` bounds paths at 192 bytes; use its actual name/path validation rules rather than assuming every storage byte is usable text.
- Larger shell/GUI executables must fit the actual ELF loader and filesystem limits. Account for all seeded utilities/apps/configuration files; do not silently grow the on-disk format, assume arbitrary wallpapers fit, or install binaries that the loader cannot run.
- Normal-mode framebuffer ownership currently belongs to the console. GUI mode needs an explicit ownership transition, not two competing framebuffer writers.
- PIT ticks provide uptime, not a wall-clock date/time. A clock needs a real time source and a documented timezone contract.

## Step 10: ZOS Shell 2.0 - Bash-inspired interactive and scripting environment

**Goal:** substantially improve day-to-day shell use and scripting, with real stream/process support rather than a larger collection of hard-coded commands.

**Dependencies:** accepted USB/start gate; existing CLI, process manager, VFS and IPC regressions remain green. Complete this phase before the graphical terminal integration.

### Copy/paste implementation prompt

```text
Implement Step 10 of MICROKERNEL_OS_PLAN.md for ZOS - Zombie OS, only after
the user has explicitly released the deferred-phase/USB gate.

Read this addendum and the current repository first. Implement a native
Bash-inspired shell; do not claim that it is GNU Bash or fully POSIX.
Use GNU documentation B1-B3 as behavior references for the chosen subset.
Publish supported syntax, options, limits, and deliberate differences.

Work in these dependency-ordered checkpoints:

10A - Language and execution contracts
- Audit argv/environment startup transfer, stdin/stdout/stderr, FD ownership,
  process identity, per-session cwd, exit status, and TTY input ownership.
  Add only the ABI/protocol support actually needed by this phase.
- Preserve quote/escape context in a bounded lexer and AST. Add comments,
  multiline input, assignments, exported environment variables, PATH lookup,
  $VAR/${VAR}, positional arguments and status parameters, pathname matching,
  and quote-aware expansion. Cover empty arguments and expansion boundaries.
- Add &&, || and ! with explicit precedence. Reject malformed or unsupported
  constructs before partially executing them.
- Keep limits explicit. Target at least 512-byte interactive input and
  32 history entries, but first budget stack/storage and protocol framing.
  Do not simply enlarge arrays on the 64 KiB user stack or truncate IPC.

10B - Real streams, redirection and concurrent pipelines
- Provide process-owned, explicitly inherited/duplicated/closed descriptors
  for stdin, stdout and stderr. Add <, >, >>, 2>, 2>>, 2>&1 and descriptor
  closure with documented left-to-right redirection behavior.
- Implement bounded byte-stream pipes with blocking/backpressure, partial
  reads/writes, EOF after all writers close, and broken-pipe behavior when
  readers disappear. Authenticate descriptor transfers and prevent leaks.
- Start all pipeline stages before waiting. Support multi-stage pipelines
  and pipeline status; include a tested pipefail option.
- Isolate state-changing builtins in pipelines/subshells so cd/export there
  do not mutate the parent session. No fake fork: extend process-manager
  execution contexts or launch a properly initialized child interpreter.
- Do not simulate pipes with temporary files or run one producer to
  completion before starting its consumer.

10C - Scripts and useful tools
- Support shell script files, a documented interpreter/shebang contract,
  source/., functions with bounded call depth, return/exit, if/elif/else,
  for and while/until, case, break/continue, test/[ ], integer arithmetic,
  grouping/subshells, bounded command substitution and here-documents.
- Distinguish parent-shell state from isolated execution. Preserve quoting
  and status behavior in nested scripts, substitutions and sourced files.
- Add export/unset, env, printf, read, type/command and working-directory
  conveniences. Extend existing tools instead of duplicating their logic.
- Supply useful small stream tools: grep with a documented initial match
  subset, head, tail, wc, sort, uniq, and bounded find. Add help for supported
  flags; do not imply that these are complete GNU coreutils ports.
- Keep streaming operations streaming. Bound tools that need whole-input
  storage and report size/resource exhaustion rather than dropping data.
- Keep generated programs within filesystem/loader limits. If that cannot
  be achieved, stop and present the concrete prerequisite format/loader
  change before committing to a broad storage migration.

10D - Interactive usability and genuine jobs
- Add command/path completion, improved history/search, configurable prompt,
  and a documented startup script. Persist history only with explicit
  bounded settings and normal VFS operations.
- Add jobs/fg/bg and real foreground pipeline/session ownership. Implement
  the process-group and suspend/resume/interrupt semantics they require;
  Ctrl+C/Ctrl+Z must affect the intended job, not the shell or every process.
- Define behavior for background TTY reads, exiting a session with active
  jobs, child faults and cancelled blocked I/O. Merely printing job labels
  around the current background-spawn feature is not job control.
- Document any ZOS-specific status/notification semantics rather than
  claiming Unix signals exist when they do not.

Acceptance:
- Host tests exercise syntax, precedence, quote-sensitive expansion, glob
  limits, malformed syntax, nesting/resource bounds, redirection ordering,
  scripts/functions and status propagation. Compare selected compatible
  cases with a real GNU Bash reference where available.
- Existing shell sessions and VFS/process tests still pass.
- Native tests prove pipelines with output several times their buffer
  capacity, slow consumers, early reader exit, correct EOF, cancellation,
  and cleanup after spawn/resource failures. No deadlocks or stale FDs.
- Demonstrate a producer/consumer pipeline, filtering/counting, a script
  using a loop/function/conditional, environment inheritance, and parent
  cwd preservation after an isolated cd.
- Demonstrate jobs, interrupt, stop/resume and foreground/background
  transitions with actual scheduler/process state and isolated TTY input.
- BIOS and UEFI QEMU shell runs pass. Repeat applicable authorized USB
  normal-mode tests without modifying read-only diagnostic behavior.
- Record before/after process, channel, descriptor and memory counts.
  Repeated command/script/job cycles return to the expected baseline.

Commit coherent accepted checkpoints locally. When the whole phase passes,
create a new phase-10 checkpoint/tag without moving existing tags, update
documentation and refresh the full-history source bundle.
Stop after Step 10; do not begin graphics or desktop implementation.
```

**Not required in this phase:** a literal GNU Bash binary, complete POSIX/libc, every Bash extension, arrays/associative arrays, process substitution, dynamically loaded builtins, full regex tools, locale/Unicode support, or package management. Explicitly diagnose unsupported syntax rather than silently misinterpreting it.

## Step 11: Native graphics, window management and independent terminal windows

**Goal:** a reliable ring-3 graphical foundation on which real applications can run.

**Dependencies:** Step 10 accepted; a measured resource design; preserved CLI fallback.

### Copy/paste implementation prompt

```text
Implement only Step 11 of the deferred ZOS roadmap after its execution gate
and Step 10 acceptance have been satisfied.

Build a small native 2D windowing system. Use Photon only as UX inspiration;
do not port its runtime or copy its artwork. Keep graphics policy and all
applications in ring 3. Keep the kernel limited to necessary memory,
scheduling, IPC and narrowly scoped hardware capabilities.

11A - Ownership, protocol and rendering
- Audit current console/input/TTY/process-manager startup and authorization.
  In GUI mode give exactly one display/window-manager service the writable
  framebuffer; console/terminal output becomes a client. Preserve a separate
  selectable CLI mode and the existing diagnostic build.
- Define versioned, bounded client requests/events for windows, drawing,
  invalidation, focus, input and close. Authenticate each request using
  actual sender identity/capabilities; reject stale/reused window handles.
- Account for existing 1024-byte IPC, 32 threads, 16 channels and per-process
  memory/stack limits. Multiplex clients without per-widget channels.
- Choose a measured memory model before allocating surfaces: direct clipped
  repaint, retained bounded drawing lists, or small tiles are candidates.
  A 3 MiB full-screen private backbuffer cannot fit today's 512 KiB dynamic
  mapping quota. Do not silently lift limits or grant raw framebuffer/DMA
  access to every app.
- Implement clipped rectangles, lines, bitmap text and original small icons.
  Validate coordinates, lengths, strides, overflow and framebuffer format.
  Reuse the embedded font and existing safe helpers where appropriate.
- Use dirty-region/event-driven repaint and bounded event queues. Coalesce
  pointer motion, but never silently lose key/button-release/close events.
  Define overload/backpressure behavior that avoids synchronous IPC cycles.

11B - Windows, focus and minimal widgets
- Implement overlapping windows, z-order, title bars, move/resize, minimize,
  maximize/restore, close and correct damage repair on exposure.
- Route mouse capture and keyboard input to the intended client. Support
  click-to-focus, visible active state, Alt+Tab and keyboard traversal.
- Provide a small shared client/widget layer: labels, buttons, text fields,
  scrollable lists, menus and modal/error dialogs. Bound every collection.
- A blocked or crashed client must not freeze the pointer or unrelated
  windows. Dispose its windows/resources and show an explicit failure.
- Define recovery when the display service fails and provide a tested
  fallback/restart route. Do not let a second writer race the framebuffer.

11C - Real graphical terminals
- Run at least two independent terminal windows using the Step 10 shell.
  Each needs separate cwd, environment, history, foreground job and output.
  Add the session-aware process/VFS/TTY authorization this actually needs;
  do not mirror the single existing TTY into two decorative windows.
- Support terminal sizing, scrolling/scrollback and the escape/control
  sequence subset needed by delivered programs. Document that subset.
- Closing one terminal cancels or detaches its jobs according to the
  documented policy and does not kill another session's jobs.

Acceptance:
- Host tests cover geometry/clipping, pixel formats, focus/hit testing,
  handle generations, malformed IPC and resource/event-queue exhaustion.
- Native BIOS/UEFI QEMU tests drive real keyboard/mouse events, overlap,
  drag/resize/minimize/restore and concurrent terminal output.
- An adversarial client cannot draw outside its window, steal another
  client's handle, gain framebuffer access, or consume unbounded resources.
- Crash/kill and restart clients repeatedly; compare resource baselines.
- Keep pointer/focus responsive while a terminal streams output and another
  app performs USB-backed VFS I/O; no input-storage-display IPC deadlock.
- Capture and inspect actual rendered framebuffers, including odd window
  sizes and mode dimensions. A boot marker alone is not GUI acceptance.
- Re-run CLI/USB/diagnostic regressions and report measured memory/CPU use.

Persist accepted checkpoints, documentation and phase-11 tag/source bundle.
Stop after Step 11; do not substitute a static desktop mockup for apps.
```

## Step 12: ZOS Desktop - Files, clock, backgrounds and small useful applications

**Goal:** a usable small desktop, with the user's requested file manager, clock and selectable background, plus selected Photon-inspired conveniences.

**Dependencies:** Step 11 accepted, real terminal sessions, VFS-backed operations and a clock-source design.

### Copy/paste implementation prompt

```text
Implement Step 12 of the deferred ZOS roadmap only after its gate and the
shell/windowing phases have passed. Follow the original proposed layout
and the research edition distinctions in this plan.

12A - Desktop essentials
- Build a desktop shell with a launcher, running-app task buttons, real
  window activation/minimize state, and Files/Terminal/Settings/Help entries.
  Start with one workspace. Use an original ZOS visual theme and assets.
- Build Files against VFS only: path/breadcrumb navigation, back/up/refresh,
  favorites, filename/size sorting, keyboard selection, context menus,
  create folder/file, rename, copy, move and delete. Support opening text
  in the delivered editor/viewer and opening a terminal at that directory.
- File operations must be real. Show missing-path, invalid-name,
  destination-conflict, read-only, file/inode-limit, and I/O errors.
  Do not invent permissions, dates or metadata absent from the filesystem.
- Use typed app-launch requests with argument arrays, not shell-string
  interpolation of filenames. Restrict filesystem access to the selected
  mounted USB root; do not discover or mount laptop internal disks.
- Keep storage work from blocking the desktop input loop. Make progress and
  cancellation truthful, and document atomicity/partial-copy behavior.
- Add a real clock/date service or narrowly scoped read-only RTC snapshot
  interface. Validate hardware time, update consistency and BCD/12h modes
  if using CMOS RTC. Document the hardware-time/timezone interpretation.
  Use monotonic time only for intervals between validated wall-time reads;
  never label PIT uptime as a date or invent time when no source exists.
- Clock UI shows its timezone/source state and handles unavailable time
  explicitly. Share this source with any date utility, rather than adding
  a second implementation. Do not add NTP or hardware clock writes here.
- Settings provides selectable background colors and original patterns,
  active/inactive accent choices and a 12/24-hour display preference.
  Include Apply, Cancel, defaults, and an explicit save behavior.
- Persist bounded/versioned settings through VFS with validated parsing
  and the filesystem's real atomic-update/flush guarantees. Exercise
  invalid/truncated settings and interrupted-save recovery.

12B - Useful native applications
- Deliver a small plain-text editor with open/save/save-as, dirty-state
  prompts, find and useful keyboard shortcuts, within actual file limits.
- Add a calculator with defined arithmetic/errors, an offline Help/About
  viewer, and system information using actual process/memory/storage data.
  Label unavailable statistics; do not synthesize CPU/disk graphs.
- Reuse common widgets and service APIs. Apps are separately owned clients,
  not painted placeholders with all logic privileged in the display server.
- Keep resource quotas and capability access explicit. Closing/crashing an
  app releases windows, descriptors, processes and event subscriptions.

12C - Desktop polish after essentials pass
- Add a small collapsible right-hand launcher/resource shelf and optional
  two-workspace switcher. Store workspace window membership, not another
  full framebuffer per workspace. Keep this simpler than Photon's full
  nine-console desktop.
- Add basic shared text clipboard behavior and, if the input protocol can
  support it correctly, Files drag-and-drop after click/menu operations.
- Provide a simple idle blank/pattern screen saver with input wakeup.
  This is not a security lock screen or an ACPI power-management claim.
- Background-image selection starts with a strictly bounded, validated
  small tiled format. Respect the current 64 KiB file and process memory
  limits; reject unsupported/oversized images. Full-resolution photos and
  complex image codecs require a separately accepted storage/memory design.
- Optional image viewing/local graphics animation follows the same bounds.
  Defer browser/networking, audio/CD playback, remote desktop and an
  appointment/calendar application; they are not hidden prerequisites.

Acceptance:
- Host tests cover file-operation requests, hostile paths/filenames, dialog
  state, settings version/bounds/recovery, clock conversion and rollovers,
  editor save/error handling and app-launch argument preservation.
- Use controlled wall-time input for deterministic clock tests and verify
  an actual time source in the native run. Test unavailable/invalid time.
- Native BIOS/UEFI runs launch two terminals plus Files and an editor,
  switch focus/tasks, manipulate windows and run concurrent shell/file I/O.
- Create a folder/file in Files, edit/save, copy/rename/move, inspect the
  result from a terminal, reboot, and confirm persisted content/settings.
- Demonstrate clock display changes and background selection, including
  Cancel and persistence. Test full disk/inode limits, invalid settings,
  unsupported images, disconnected/failed storage and app faults.
- Inspect captured framebuffers for real file listings, rendered text,
  menus, clock, backgrounds and dialogs. Also verify the resulting VFS
  state; screenshots alone do not prove data operations worked.
- Repeated app/window/terminal cycles return to bounded resource baselines;
  no second framebuffer owner, leaked input focus or hung IPC chains.
- Preserve normal CLI and read-only diagnostics. Run authorized physical
  USB desktop checks only for the hardware support actually implemented,
  and distinguish those results from QEMU-only evidence.

Commit accepted checkpoints, document user controls and known limitations,
create a new phase-12 checkpoint/tag, and refresh the source bundle.
Stop here. Browser/network and broad POSIX compatibility are later work.
```

## Explicitly deferred work

This roadmap does not authorize implementation now. It does not include a GNU Bash port, a Linux compatibility layer, networking/TLS/browser engine, audio stack, USB hubs/composite-device expansion, GPU acceleration, SMP, a new general-purpose filesystem, or an installer to the laptop's internal disk. Any prerequisite that genuinely exceeds the staged scope must be explained and accepted separately rather than hidden in a "desktop" change.

## Sources and evidence boundaries

The prompts and ZOS architecture proposals above are original design work. The sources establish historical features and shell semantics; they are not specifications for copying QNX code or artwork.

- **Q1 - Historical demo contents:** Dan Hildebrand's 1997 QNX announcement, reproduced under "UP CLOSE: QNX Demo Disk" in TinyApps' 2001 newsletter. This is a reproduction of the primary announcement, not an original QNX-hosted page. `https://tinyapps.org/ezine/2001.html`
- **Q2 - Full Photon desktop:** QNX 4.25 / Photon 1.14, "Photon Desktop Manager." Launcher/taskbar, workspace navigation, terminals and application inventory; not proof that every listed app shipped on the floppy. `https://developers.qnx.com/developers/docs/qnx_4.25_docs/photon114/user_guide/desktop_man.html`
- **Q3 - Full Photon file manager:** QNX 4.25 / Photon 1.14, "File Manager." `https://developers.qnx.com/developers/docs/qnx_4.25_docs/photon114/user_guide/file_man.html`
- **Q4 - Full Photon appearance/settings:** QNX 4.25 / Photon 1.14, "Customizing Photon." Backgrounds, window behavior, backdrops and screen savers. `https://developers.qnx.com/developers/docs/qnx_4.25_docs/photon114/user_guide/customizing.html`
- **Q5 - Later Photon desktop:** QNX Neutrino 6.3.2, "Using the Photon microGUI." Shelf, taskbar/date/time, clock/resource plugins, settings and applications. Later-version evidence only, not a floppy-demo manifest. `https://www.qnx.com/developers/docs/6.3.2/neutrino/user_guide/using_photon.html`
- **B1 - Shell behavior:** GNU Bash Reference Manual, particularly quoting/expansion, redirection, execution environments, scripts and job control. `https://www.gnu.org/software/bash/manual/bash.html`
- **B2 - Pipeline behavior:** GNU Bash Reference Manual, "Pipelines." Concurrent execution, stream connection and exit status. `https://www.gnu.org/software/bash/manual/html_node/Pipelines`
- **B3 - Bash scope:** GNU Bash project overview; distinguishes the real GNU shell from ZOS's proposed native compatibility subset. `https://www.gnu.org/software/bash/`
