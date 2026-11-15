# Third-party software

ZOS itself is licensed under **MIT OR Apache-2.0** (see `LICENSE-MIT` and
`LICENSE-APACHE`). No third-party source code is copied into this repository; the
components below are used as dependencies or tools. Licenses are the ones declared by
each package's metadata at the locked versions in `Cargo.lock`.

## Rust crates linked into ZOS

| Crate | Version | License |
| --- | --- | --- |
| bit_field | 0.10.3 | Apache-2.0 / MIT |
| bitflags | 2.13.2 | MIT OR Apache-2.0 |
| font8x8 | 0.3.1 | MIT |
| limine (Rust bindings) | 0.6.5 | MIT OR Apache-2.0 |
| linked_list_allocator | 0.10.5 | Apache-2.0 / MIT |
| lock_api | 0.4.14 | MIT OR Apache-2.0 |
| rustversion | 1.0.23 | MIT OR Apache-2.0 |
| scopeguard | 1.2.0 | MIT OR Apache-2.0 |
| spin | 0.9.9 | MIT |
| volatile | 0.4.6 | MIT OR Apache-2.0 |
| x86_64 | 0.15.2 | MIT / Apache-2.0 |

## Build and test tooling (downloaded by `tools\bootstrap.ps1`, not in this repository)

- **Limine bootloader** (BSD 2-Clause). The bootloader binaries are copied into the
  boot images and USB packages that the tools build. Redistributing such an image
  requires keeping Limine's license notice with it.
- Rust toolchain, MinGW-w64 (GNU linker), QEMU with its EDK2 firmware, 7-Zip: used
  to build and test only; none of them is part of ZOS or its images.

## Inspiration

The design follows the microkernel architecture popularised by QNX (synchronous
message passing, drivers and services as user-space processes). No QNX code, assets
or documentation text is used.
