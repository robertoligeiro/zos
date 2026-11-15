[CmdletBinding(SupportsShouldProcess = $true)]
param(
    [int]$DiskNumber = -1,
    [switch]$List,
    [switch]$Write,
    [switch]$Normal
)

Set-StrictMode -Version 2
$ErrorActionPreference = 'Stop'
$script:ZosPackage = Join-Path $PSScriptRoot '..\build\usb-diagnostic-files'
$script:ZosNormalPackage = Join-Path $PSScriptRoot '..\build\usb-normal-files'
# GPT type of the ZOS data partition; it must match servers\console\src\usb\gpt.rs.
$script:ZosPartitionType = '{f2ac67b5-91fa-4c46-a324-95b456c3829d}'
$script:ZosVolumeBytes = 16MB
$script:ZosScriptFile = $PSCommandPath

function Test-ZosAdministrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Assert-ZosUsbDisk {
    param($Disk, [int]$Number)
    if ($null -eq $Disk -or $Disk.Number -ne $Number -or $Disk.BusType -ne 'USB' -or
        $Disk.IsBoot -or $Disk.IsSystem -or $Disk.IsOffline -or $Disk.IsReadOnly -or
        $Disk.Size -lt 1GB -or $Disk.Size -gt 2TB -or $Disk.LogicalSectorSize -ne 512 -or
        [string]::IsNullOrWhiteSpace($Disk.UniqueId)) {
        throw 'Refusing target: require an online, writable, non-boot/non-system USB disk, 1 GiB-2 TiB, 512-byte logical sectors, and a stable identity.'
    }
}

function Get-ZosPackageFiles {
    $manifestPath = Join-Path $script:ZosPackage 'manifest.json'
    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    $expected = @('EFI\BOOT\BOOTX64.EFI', 'limine.conf', 'boot\zos.elf',
        'boot\console-server.elf', 'boot\input-server.elf')
    if ($manifest.kind -ne 'zos-usb-diagnostic' -or $manifest.storage_read_only -ne $true) {
        throw 'Not a read-only ZOS diagnostic package. Run: python tools\zos.py image --diagnostic'
    }
    $names = @($manifest.files.PSObject.Properties.Name)
    if (@(Compare-Object ($expected | Sort-Object) ($names | Sort-Object) -CaseSensitive).Count -ne 0) {
        throw 'Unexpected USB package file list.'
    }
    foreach ($name in $expected) {
        $path = Join-Path $script:ZosPackage $name
        $hash = $manifest.files.PSObject.Properties[$name].Value
        if ($hash -notmatch '^[0-9a-f]{64}$' -or (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ne $hash) {
            throw "USB package checksum mismatch: $name"
        }
        [pscustomobject]@{ Name = $name; Path = $path; Hash = $hash }
    }
}

function Get-ZosUsbRoot {
    param($Partition)
    if ($Partition.DriveLetter -notmatch '^[A-Za-z]$') { throw 'USB partition has no drive letter.' }
    return "$($Partition.DriveLetter):\"
}

function Invoke-ZosUsbPreparation {
    [CmdletBinding(SupportsShouldProcess = $true)]
    param([int]$DiskNumber = -1, [switch]$List, [switch]$Write)
    if ($List) {
        if ($Write) { throw '-List cannot be combined with -Write.' }
        Get-Disk -ErrorAction Stop | Where-Object BusType -EQ 'USB' |
            Select-Object Number, FriendlyName, Size, IsBoot, IsSystem, IsReadOnly
        return
    }
    if ($DiskNumber -lt 0) { throw 'Select an explicit -DiskNumber. Use -List first.' }
    $files = @(Get-ZosPackageFiles)
    $disk = Get-Disk -Number $DiskNumber -ErrorAction Stop
    Assert-ZosUsbDisk $disk $DiskNumber
    $partitions = @()
    if ($disk.PartitionStyle -ne 'RAW' -and $disk.NumberOfPartitions -gt 0) {
        $partitions = @(Get-Partition -DiskNumber $DiskNumber -ErrorAction Stop)
    }
    foreach ($partition in $partitions) {
        if ($partition.IsBoot -or $partition.IsSystem) { throw 'Refusing a boot/system partition.' }
        if ($partition.DriveLetter) {
            $prefix = "$($partition.DriveLetter):\"
            foreach ($path in @($script:ZosPackage, $script:ZosScriptFile, (Get-Location).Path)) {
                if ([IO.Path]::GetFullPath($path).StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
                    throw 'The target contains the script, package, or current working directory.'
                }
            }
        }
    }
    Write-Host ("USB disk {0}: {1}, {2:N2} GiB" -f $disk.Number, $disk.FriendlyName, ($disk.Size / 1GB))
    Write-Host 'This removes ALL partitions/data access on this USB disk, creates a 512 MiB FAT32 boot volume, and leaves the rest unallocated. This is not a secure erase.'
    if (-not $Write) {
        Write-Host 'PREVIEW ONLY: no disk changes. Rerun with -Write to prepare this exact disk.'
        return
    }
    if (-not $PSCmdlet.ShouldProcess("USB disk $DiskNumber", 'Erase partitions and install ZOS diagnostic UEFI files')) { return }
    if (-not (Test-ZosAdministrator)) { throw 'Disk preparation requires an already-administrative PowerShell session. This script does not elevate itself.' }
    $confirmation = Read-Host "Type exactly ERASE USB DISK $DiskNumber"
    if ($confirmation -cne "ERASE USB DISK $DiskNumber") { throw 'Confirmation did not match; no disk changes.' }
    $current = Get-Disk -Number $DiskNumber -ErrorAction Stop
    Assert-ZosUsbDisk $current $DiskNumber
    if ($current.UniqueId -cne $disk.UniqueId -or $current.Size -ne $disk.Size) {
        throw 'Disk identity changed; no disk changes.'
    }
    $null = @(Get-ZosPackageFiles)
    if ($current.PartitionStyle -ne 'RAW' -and $current.NumberOfPartitions -gt 0) {
        Clear-Disk -Number $DiskNumber -RemoveData -RemoveOEM -Confirm:$false -ErrorAction Stop
    }
    Update-Disk -Number $DiskNumber -ErrorAction Stop
    $prepared = Get-Disk -Number $DiskNumber -ErrorAction Stop
    Assert-ZosUsbDisk $prepared $DiskNumber
    if ($prepared.UniqueId -cne $current.UniqueId -or $prepared.Size -ne $current.Size) {
        throw 'Disk identity changed after refresh; stopping preparation.'
    }
    if ($prepared.NumberOfPartitions -ne 0) {
        throw 'Partitions remain after clearing; stopping preparation.'
    }
    switch ($prepared.PartitionStyle) {
        'RAW' { $null = Initialize-Disk -Number $DiskNumber -PartitionStyle MBR -ErrorAction Stop }
        'MBR' { }
        'GPT' { Set-Disk -Number $DiskNumber -PartitionStyle MBR -ErrorAction Stop }
        default { throw "Unsupported partition style after refresh: $($prepared.PartitionStyle)" }
    }
    $partition = New-Partition -DiskNumber $DiskNumber -Size 512MB -MbrType FAT32 -AssignDriveLetter -ErrorAction Stop
    if ($partition.DiskNumber -ne $DiskNumber) { throw 'Storage API returned a different target disk.' }
    $null = $partition | Format-Volume -FileSystem FAT32 -AllocationUnitSize 4096 -NewFileSystemLabel ZOSDIAG -Confirm:$false -ErrorAction Stop
    $root = Get-ZosUsbRoot $partition
    foreach ($file in $files) {
        $destination = Join-Path $root $file.Name
        $parent = Split-Path -Parent $destination
        if (-not (Test-Path -LiteralPath $parent -PathType Container)) {
            $null = New-Item -ItemType Directory -Path $parent -Force -ErrorAction Stop
        }
        Copy-Item -LiteralPath $file.Path -Destination $destination -ErrorAction Stop
        if ((Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash -ne $file.Hash) {
            throw "USB readback checksum mismatch: $($file.Name)"
        }
    }
    Write-Host "VERIFIED: 5 UEFI diagnostic files on $root. Safely eject the USB before unplugging."
    Write-Host 'Boot the target laptop using its one-time UEFI USB boot menu. This unsigned diagnostic has no shell or persistent filesystem.'
}

function Get-ZosNormalPackage {
    $manifestPath = Join-Path $script:ZosNormalPackage 'manifest.json'
    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    if ($manifest.kind -ne 'zos-usb-normal' -or $manifest.storage_read_only -ne $false) {
        throw 'Not a ZOS normal-mode USB package. Run: python tools\zos.py image --usb'
    }
    $names = @($manifest.files.PSObject.Properties.Name)
    $required = @('EFI\BOOT\BOOTX64.EFI', 'limine.conf', 'boot\zos.elf', 'boot\console-server.elf',
        'boot\input-server.elf', 'boot\tty-server.elf', 'boot\blkdev-server.elf', 'boot\fs-simple-server.elf',
        'boot\vfs-server.elf', 'boot\procmgr-server.elf', 'boot\shell-server.elf', 'boot\zos-command.elf')
    foreach ($name in $required) {
        if ($names -cnotcontains $name) { throw "Normal package is missing: $name" }
    }
    $files = @()
    foreach ($name in $names) {
        # Plain relative names only: no drive letters, no parent directories.
        if ($name -notmatch '^(?:[A-Za-z0-9_.-]+\\)*[A-Za-z0-9_.-]+$' -or $name -match '(^|\\)\.\.?(\\|$)') {
            throw "Unsafe file name in package manifest: $name"
        }
        $path = Join-Path $script:ZosNormalPackage $name
        $hash = $manifest.files.PSObject.Properties[$name].Value
        if ($hash -notmatch '^[0-9a-f]{64}$' -or (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ne $hash) {
            throw "USB package checksum mismatch: $name"
        }
        $files += [pscustomobject]@{ Name = $name; Path = $path; Hash = $hash }
    }
    $volume = $manifest.volume
    $volumePath = Join-Path $script:ZosNormalPackage 'zos-volume.img'
    if ($volume.file -cne 'zos-volume.img' -or $volume.bytes -ne $script:ZosVolumeBytes -or
        '{' + $volume.gpt_type + '}' -ne $script:ZosPartitionType -or $volume.sha256 -notmatch '^[0-9a-f]{64}$' -or
        (Get-Item -LiteralPath $volumePath).Length -ne $script:ZosVolumeBytes -or
        (Get-FileHash -LiteralPath $volumePath -Algorithm SHA256).Hash -ne $volume.sha256) {
        throw 'ZOS volume image in the package is missing or does not match its manifest.'
    }
    [pscustomobject]@{
        Files = $files
        Volume = [pscustomobject]@{ Path = $volumePath; Hash = $volume.sha256; Bytes = [int64]$volume.bytes }
    }
}

# Writes the pre-formatted ZOS volume into the ZOS partition through the physical drive
# and verifies it by reading it back. The target range is exactly the new partition.
function Write-ZosVolume {
    param([int]$DiskNumber, [UInt64]$Offset, $Volume)
    $chunk = 1MB
    if ($Offset % 512 -ne 0 -or $Volume.Bytes % $chunk -ne 0) { throw 'Volume offset or size is not sector-aligned.' }
    $bytes = [IO.File]::ReadAllBytes($Volume.Path)
    $sha = [Security.Cryptography.SHA256]::Create()
    if ([BitConverter]::ToString($sha.ComputeHash($bytes)).Replace('-', '').ToLowerInvariant() -ne $Volume.Hash) {
        throw 'ZOS volume image changed after verification.'
    }
    $stream = New-Object IO.FileStream("\\.\PhysicalDrive$DiskNumber", [IO.FileMode]::Open,
        [IO.FileAccess]::ReadWrite, [IO.FileShare]::ReadWrite, 1, [IO.FileOptions]::WriteThrough)
    try {
        $stream.Position = [int64]$Offset
        for ($position = 0; $position -lt $bytes.Length; $position += $chunk) {
            $stream.Write($bytes, $position, $chunk)
        }
        $stream.Flush()
        $stream.Position = [int64]$Offset
        $readback = New-Object byte[] $chunk
        $hasher = [Security.Cryptography.SHA256]::Create()
        for ($position = 0; $position -lt $bytes.Length; $position += $chunk) {
            $done = 0
            while ($done -lt $chunk) {
                $count = $stream.Read($readback, $done, $chunk - $done)
                if ($count -le 0) { throw 'Short read while verifying the ZOS volume.' }
                $done += $count
            }
            $null = $hasher.TransformBlock($readback, 0, $chunk, $null, 0)
        }
        $null = $hasher.TransformFinalBlock([byte[]]@(), 0, 0)
        if ([BitConverter]::ToString($hasher.Hash).Replace('-', '').ToLowerInvariant() -ne $Volume.Hash) {
            throw 'ZOS volume readback checksum mismatch.'
        }
    } finally {
        $stream.Dispose()
    }
}

function Invoke-ZosNormalPreparation {
    [CmdletBinding(SupportsShouldProcess = $true)]
    param([int]$DiskNumber = -1, [switch]$Write)
    if ($DiskNumber -lt 0) { throw 'Select an explicit -DiskNumber. Use -List first.' }
    $package = Get-ZosNormalPackage
    $disk = Get-Disk -Number $DiskNumber -ErrorAction Stop
    Assert-ZosUsbDisk $disk $DiskNumber
    $partitions = @()
    if ($disk.PartitionStyle -ne 'RAW' -and $disk.NumberOfPartitions -gt 0) {
        $partitions = @(Get-Partition -DiskNumber $DiskNumber -ErrorAction Stop)
    }
    foreach ($partition in $partitions) {
        if ($partition.IsBoot -or $partition.IsSystem) { throw 'Refusing a boot/system partition.' }
        if ($partition.DriveLetter) {
            $prefix = "$($partition.DriveLetter):\"
            foreach ($path in @($script:ZosNormalPackage, $script:ZosScriptFile, (Get-Location).Path)) {
                if ([IO.Path]::GetFullPath($path).StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
                    throw 'The target contains the script, package, or current working directory.'
                }
            }
        }
    }
    Write-Host ("USB disk {0}: {1}, {2:N2} GiB" -f $disk.Number, $disk.FriendlyName, ($disk.Size / 1GB))
    Write-Host 'This removes ALL partitions/data access on this USB disk and creates a GPT layout: a 512 MiB FAT32 boot volume and a 16 MiB ZOS data partition. The rest stays unallocated. This is not a secure erase.'
    if (-not $Write) {
        Write-Host 'PREVIEW ONLY: no disk changes. Rerun with -Write to prepare this exact disk.'
        return
    }
    if (-not $PSCmdlet.ShouldProcess("USB disk $DiskNumber", 'Erase partitions and install ZOS (shell, writable ZOS volume)')) { return }
    if (-not (Test-ZosAdministrator)) { throw 'Disk preparation requires an already-administrative PowerShell session. This script does not elevate itself.' }
    $confirmation = Read-Host "Type exactly ERASE USB DISK $DiskNumber"
    if ($confirmation -cne "ERASE USB DISK $DiskNumber") { throw 'Confirmation did not match; no disk changes.' }
    $current = Get-Disk -Number $DiskNumber -ErrorAction Stop
    Assert-ZosUsbDisk $current $DiskNumber
    if ($current.UniqueId -cne $disk.UniqueId -or $current.Size -ne $disk.Size) {
        throw 'Disk identity changed; no disk changes.'
    }
    $package = Get-ZosNormalPackage
    if ($current.PartitionStyle -ne 'RAW' -and $current.NumberOfPartitions -gt 0) {
        Clear-Disk -Number $DiskNumber -RemoveData -RemoveOEM -Confirm:$false -ErrorAction Stop
    }
    Update-Disk -Number $DiskNumber -ErrorAction Stop
    $prepared = Get-Disk -Number $DiskNumber -ErrorAction Stop
    Assert-ZosUsbDisk $prepared $DiskNumber
    if ($prepared.UniqueId -cne $current.UniqueId -or $prepared.Size -ne $current.Size) {
        throw 'Disk identity changed after refresh; stopping preparation.'
    }
    if ($prepared.NumberOfPartitions -ne 0) {
        throw 'Partitions remain after clearing; stopping preparation.'
    }
    switch ($prepared.PartitionStyle) {
        'RAW' { $null = Initialize-Disk -Number $DiskNumber -PartitionStyle GPT -ErrorAction Stop }
        'GPT' { }
        'MBR' { Set-Disk -Number $DiskNumber -PartitionStyle GPT -ErrorAction Stop }
        default { throw "Unsupported partition style after refresh: $($prepared.PartitionStyle)" }
    }
    $boot = New-Partition -DiskNumber $DiskNumber -Size 512MB -AssignDriveLetter -ErrorAction Stop
    if ($boot.DiskNumber -ne $DiskNumber) { throw 'Storage API returned a different target disk.' }
    $null = $boot | Format-Volume -FileSystem FAT32 -AllocationUnitSize 4096 -NewFileSystemLabel ZOSBOOT -Confirm:$false -ErrorAction Stop
    $root = Get-ZosUsbRoot $boot
    foreach ($file in $package.Files) {
        $destination = Join-Path $root $file.Name
        $parent = Split-Path -Parent $destination
        if (-not (Test-Path -LiteralPath $parent -PathType Container)) {
            $null = New-Item -ItemType Directory -Path $parent -Force -ErrorAction Stop
        }
        Copy-Item -LiteralPath $file.Path -Destination $destination -ErrorAction Stop
        if ((Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash -ne $file.Hash) {
            throw "USB readback checksum mismatch: $($file.Name)"
        }
    }
    $zos = New-Partition -DiskNumber $DiskNumber -Size $script:ZosVolumeBytes -GptType $script:ZosPartitionType -ErrorAction Stop
    if ($zos.DiskNumber -ne $DiskNumber -or "$($zos.DriveLetter)" -match '^[A-Za-z]$' -or $zos.Size -lt $script:ZosVolumeBytes -or
        $zos.Offset % 512 -ne 0 -or $zos.Offset -lt ($boot.Offset + $boot.Size) -or
        ($zos.Offset + $script:ZosVolumeBytes) -gt $prepared.Size) {
        throw 'The ZOS partition is not where expected; refusing to write the volume.'
    }
    Write-ZosVolume -DiskNumber $DiskNumber -Offset $zos.Offset -Volume $package.Volume
    Write-Host "VERIFIED: $($package.Files.Count) boot files on $root and a formatted 16 MiB ZOS volume at byte offset $($zos.Offset). Safely eject the USB before unplugging."
    Write-Host 'Boot the target laptop using its one-time UEFI USB boot menu. ZOS writes only inside its own 16 MiB partition.'
}

if ($MyInvocation.InvocationName -ne '.') {
    if ($Normal -and -not $List) {
        Invoke-ZosNormalPreparation -DiskNumber $DiskNumber -Write:$Write
    } else {
        Invoke-ZosUsbPreparation -DiskNumber $DiskNumber -List:$List -Write:$Write
    }
}
