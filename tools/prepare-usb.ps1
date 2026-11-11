[CmdletBinding(SupportsShouldProcess = $true)]
param(
    [int]$DiskNumber = -1,
    [switch]$List,
    [switch]$Write
)

Set-StrictMode -Version 2
$ErrorActionPreference = 'Stop'
$script:ZosPackage = Join-Path $PSScriptRoot '..\build\usb-diagnostic-files'
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

if ($MyInvocation.InvocationName -ne '.') {
    Invoke-ZosUsbPreparation -DiskNumber $DiskNumber -List:$List -Write:$Write
}
