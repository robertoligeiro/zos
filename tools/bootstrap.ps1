$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$root = Split-Path $PSScriptRoot -Parent
$tools = Join-Path $root '.tools'
$downloads = Join-Path $tools 'downloads'
New-Item -ItemType Directory -Force $downloads | Out-Null

function Get-Download($url, $destination) {
    if (-not (Test-Path $destination)) {
        Write-Host "Downloading $url"
        Invoke-WebRequest -UseBasicParsing $url -OutFile "$destination.part"
        Move-Item "$destination.part" $destination
    }
}

$env:CARGO_HOME = Join-Path $tools 'cargo'
$env:RUSTUP_HOME = Join-Path $tools 'rustup'
$env:PATH = "$env:CARGO_HOME\bin;$tools\mingw64\bin;$env:PATH"
Get-Download 'https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe' "$downloads\rustup-init.exe"
if (-not (Test-Path "$env:CARGO_HOME\bin\rustup.exe")) {
    & "$downloads\rustup-init.exe" -y --no-modify-path --default-host x86_64-pc-windows-gnu --default-toolchain none
    if ($LASTEXITCODE -ne 0) { throw 'rustup installation failed' }
}
Push-Location $root
try {
    & "$env:CARGO_HOME\bin\rustup.exe" show
    if ($LASTEXITCODE -ne 0) { throw 'Pinned Rust installation failed' }
} finally {
    Pop-Location
}

# Extract installers without running them: no registry, PATH or system changes.
Get-Download 'https://www.7-zip.org/a/7zr.exe' "$downloads\7zr.exe"
Get-Download 'https://github.com/ip7z/7zip/releases/download/26.06/7z2606-x64.exe' "$downloads\7zip.exe"
if (-not (Test-Path "$tools\7zip\7z.exe")) {
    & "$downloads\7zr.exe" x "$downloads\7zip.exe" "-o$tools\7zip" -y
    if ($LASTEXITCODE -ne 0) { throw '7-Zip extraction failed' }
}
Get-Download 'https://github.com/niXman/mingw-builds-binaries/releases/download/16.2.0-rt_v14-rev1/x86_64-16.2.0-release-posix-seh-ucrt-rt_v14-rev1.7z' "$downloads\mingw.7z"
if ((Get-FileHash "$downloads\mingw.7z" -Algorithm SHA256).Hash -ne '26788998d615856a915e1eeacb0854fce87bc608fa9a0c16f96bcbfdcf3415c0') {
    throw 'MinGW checksum mismatch'
}
if (-not (Test-Path "$tools\mingw64\bin\gcc.exe")) {
    & "$tools\7zip\7z.exe" x "$downloads\mingw.7z" "-o$tools" -y
    if ($LASTEXITCODE -ne 0) { throw 'MinGW extraction failed' }
}
Get-Download 'https://qemu.weilnetz.de/w64/qemu-w64-setup-20260811.exe' "$downloads\qemu.exe"
Get-Download 'https://qemu.weilnetz.de/w64/qemu-w64-setup-20260811.sha512' "$downloads\qemu.sha512"
$expected = ((Get-Content "$downloads\qemu.sha512" -Raw) -split '\s+')[0]
if ((Get-FileHash "$downloads\qemu.exe" -Algorithm SHA512).Hash -ne $expected) {
    throw 'QEMU checksum mismatch'
}
if (-not (Test-Path "$tools\qemu\qemu-system-x86_64.exe")) {
    & "$tools\7zip\7z.exe" x "$downloads\qemu.exe" "-o$tools\qemu" -y
    if ($LASTEXITCODE -ne 0) { throw 'QEMU extraction failed' }
}

$limineCommit = 'ee5d29cd0a8034612dcd1df3f00052480db785c5'
if (-not (Test-Path "$tools\limine\.git")) {
    git clone --depth 1 --branch v9.x-binary https://github.com/Limine-Bootloader/Limine.git "$tools\limine"
    if ($LASTEXITCODE -ne 0) { throw 'Limine download failed' }
}
git -C "$tools\limine" fetch --depth 1 origin $limineCommit
if ($LASTEXITCODE -ne 0) { throw 'Limine pinned revision download failed' }
git -C "$tools\limine" checkout --detach $limineCommit
if ($LASTEXITCODE -ne 0) { throw 'Limine checkout failed' }
& "$env:CARGO_HOME\bin\rustc.exe" +nightly-2025-03-01 --version
if ($LASTEXITCODE -ne 0) { throw 'Rust verification failed' }
& "$tools\mingw64\bin\gcc.exe" --version
if ($LASTEXITCODE -ne 0) { throw 'Host linker verification failed' }
& "$tools\qemu\qemu-system-x86_64.exe" --version
if ($LASTEXITCODE -ne 0) { throw 'QEMU verification failed' }
Write-Host 'Portable ZOS build environment ready.'
