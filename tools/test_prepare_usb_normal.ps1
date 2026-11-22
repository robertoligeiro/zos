$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\prepare-usb.ps1"
$root = Join-Path $PSScriptRoot "..\build\usb-normal-script-test-$PID"
$script:ZosNormalPackage = Join-Path $root 'package'
$script:Destination = Join-Path $root 'destination'
$null = New-Item -ItemType Directory -Path $script:ZosNormalPackage -Force
$null = New-Item -ItemType Directory -Path $script:Destination -Force
$names = @('EFI\BOOT\BOOTX64.EFI', 'limine.conf', 'boot\zos.elf', 'boot\console-server.elf', 'boot\input-server.elf',
    'boot\tty-server.elf', 'boot\blkdev-server.elf', 'boot\fs-simple-server.elf', 'boot\vfs-server.elf',
    'boot\procmgr-server.elf', 'boot\shell-server.elf', 'boot\zos-command.elf', 'boot\wm-server.elf',
    'boot\app-browser.elf', 'boot\app-calc.elf')
$hashes = @{}
foreach ($name in $names) {
    $file = Join-Path $script:ZosNormalPackage $name
    $null = New-Item -ItemType Directory -Path (Split-Path -Parent $file) -Force
    Set-Content -LiteralPath $file -Value "fixture $name"
    $hashes[$name] = (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLowerInvariant()
}
$volumePath = Join-Path $script:ZosNormalPackage 'zos-volume.img'
[IO.File]::WriteAllBytes($volumePath, (New-Object byte[] (16MB)))
$volumeHash = (Get-FileHash -LiteralPath $volumePath -Algorithm SHA256).Hash.ToLowerInvariant()
function Write-Manifest {
    param($Volume = @{ file = 'zos-volume.img'; bytes = 16777216; sha256 = $volumeHash; gpt_type = 'f2ac67b5-91fa-4c46-a324-95b456c3829d' })
    @{ kind = 'zos-usb-normal'; storage_read_only = $false; files = $hashes; volume = $Volume } |
        ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $script:ZosNormalPackage 'manifest.json')
}
Write-Manifest
$script:Disk = [pscustomobject]@{
    Number = 77; BusType = 'USB'; IsBoot = $false; IsSystem = $false
    IsOffline = $false; IsReadOnly = $false; Size = 8GB; LogicalSectorSize = 512
    UniqueId = 'MOCK-USB'; FriendlyName = 'File-backed test only'; PartitionStyle = 'MBR'; NumberOfPartitions = 1
}
$script:Calls = @()
$script:Boot = [pscustomobject]@{ DiskNumber = 77; DriveLetter = 'Z'; IsBoot = $false; IsSystem = $false; Offset = [UInt64]1MB; Size = [UInt64]512MB }
$script:Zos = [pscustomobject]@{ DiskNumber = 77; DriveLetter = [char]0; IsBoot = $false; IsSystem = $false; Offset = [UInt64](513MB); Size = [UInt64]16MB }
$script:Swap = $false
$script:Reads = 0
$script:Answer = 'ERASE USB DISK 77'
$script:ClearedStyle = 'RAW'
$script:Written = @()
function Get-Disk {
    param($Number, $ErrorAction)
    $script:Reads++
    if ($script:Swap -and $script:Reads -gt 1) {
        $copy = $script:Disk.PSObject.Copy()
        $copy.UniqueId = 'SWAPPED'
        return $copy
    }
    return $script:Disk
}
function Get-Partition {
    param($DiskNumber, $ErrorAction)
    if ($script:Disk.NumberOfPartitions -eq 0) { throw 'No partitions exist on the mock disk.' }
    return $script:Boot
}
function Clear-Disk {
    param($Number, [switch]$RemoveData, [switch]$RemoveOEM, $Confirm, $ErrorAction)
    $script:Calls += 'clear'
    $script:Disk.PartitionStyle = $script:ClearedStyle
    $script:Disk.NumberOfPartitions = 0
}
function Update-Disk { param($Number, $ErrorAction) $script:Calls += 'refresh' }
function Initialize-Disk {
    param($Number, $PartitionStyle, $ErrorAction)
    if ($script:Disk.PartitionStyle -ne 'RAW' -or $PartitionStyle -ne 'GPT') { throw 'Only a blank disk is initialized, as GPT.' }
    $script:Calls += 'initialize'
    $script:Disk.PartitionStyle = $PartitionStyle
}
function Set-Disk {
    param($Number, $PartitionStyle, $ErrorAction)
    if ($script:Disk.NumberOfPartitions -ne 0 -or $PartitionStyle -ne 'GPT') { throw 'Only an empty disk may be converted, to GPT.' }
    $script:Calls += 'convert'
    $script:Disk.PartitionStyle = $PartitionStyle
}
function New-Partition {
    param($DiskNumber, $Size, $GptType, [switch]$AssignDriveLetter, $ErrorAction)
    if ($script:Disk.PartitionStyle -ne 'GPT') { throw 'The mock disk must be GPT before partitioning.' }
    $script:Disk.NumberOfPartitions++
    if ($GptType) {
        if ($GptType -ne '{f2ac67b5-91fa-4c46-a324-95b456c3829d}' -or $Size -ne 16MB -or $AssignDriveLetter) {
            throw 'Unexpected ZOS partition parameters.'
        }
        $script:Calls += 'zos-partition'
        return $script:Zos
    }
    if ($Size -ne 512MB -or -not $AssignDriveLetter) { throw 'Unexpected boot partition parameters.' }
    $script:Calls += 'boot-partition'
    return $script:Boot
}
function Format-Volume {
    param([Parameter(ValueFromPipeline = $true)]$InputObject, $FileSystem, $AllocationUnitSize, $NewFileSystemLabel, $Confirm)
    process {
        if ($FileSystem -ne 'FAT32' -or $InputObject -ne $script:Boot) { throw 'Only the boot partition may be formatted.' }
        $script:Calls += 'format'
    }
}
function Write-ZosVolume {
    param([int]$DiskNumber, [UInt64]$Offset, $Volume)
    $script:Calls += 'write-volume'
    $script:Written += [pscustomobject]@{ Disk = $DiskNumber; Offset = $Offset; Bytes = $Volume.Bytes }
}
function Read-Host { param($Prompt) return $script:Answer }
function Test-ZosAdministrator { return $true }
function Get-ZosUsbRoot { param($Partition) return $script:Destination }
function New-Item {
    [CmdletBinding()]
    param([string]$ItemType, [string]$Path, [switch]$Force)
    if ([IO.Path]::GetFullPath($Path) -eq [IO.Path]::GetFullPath($script:Destination)) {
        throw 'Attempted to recreate the existing USB drive root.'
    }
    & (Get-Command New-Item -CommandType Cmdlet) @PSBoundParameters
}
function Assert-True { param($Value, $Message) if (-not $Value) { throw $Message } }
function Expect-Rejection {
    param([scriptblock]$Action, [string]$Message)
    $script:Calls = @()
    $rejected = $false
    try { & $Action } catch { $rejected = $true }
    Assert-True $rejected "$Message was accepted"
    Assert-True ($script:Calls.Count -eq 0) "$Message touched storage"
}
function Reset-Disk {
    $script:Disk.PartitionStyle = 'MBR'
    $script:Disk.NumberOfPartitions = 1
    $script:Calls = @()
    $script:Written = @()
    $script:Reads = 0
}
try {
    Expect-Rejection { Invoke-ZosNormalPreparation } 'Missing disk number'
    foreach ($property in @('IsBoot', 'IsSystem', 'IsOffline', 'IsReadOnly')) {
        $script:Disk.$property = $true
        Expect-Rejection { Invoke-ZosNormalPreparation -DiskNumber 77 -Write } $property
        $script:Disk.$property = $false
    }
    $script:Disk.BusType = 'NVMe'
    Expect-Rejection { Invoke-ZosNormalPreparation -DiskNumber 77 -Write } 'Internal disk'
    $script:Disk.BusType = 'USB'
    Invoke-ZosNormalPreparation -DiskNumber 77
    Invoke-ZosNormalPreparation -DiskNumber 77 -Write -WhatIf
    Assert-True ($script:Calls.Count -eq 0) 'Preview or WhatIf touched storage'
    $script:Answer = 'yes'
    Expect-Rejection { Invoke-ZosNormalPreparation -DiskNumber 77 -Write } 'Incorrect confirmation'
    $script:Answer = 'ERASE USB DISK 77'
    $script:Swap = $true
    $script:Reads = 0
    Expect-Rejection { Invoke-ZosNormalPreparation -DiskNumber 77 -Write } 'Changed identity'
    $script:Swap = $false
    $script:Reads = 0

    # Package integrity: every file, the volume image and the manifest are checked first.
    $file = Join-Path $script:ZosNormalPackage 'boot\zos.elf'
    $original = [IO.File]::ReadAllBytes($file)
    Set-Content -LiteralPath $file -Value 'corrupted'
    Expect-Rejection { Invoke-ZosNormalPreparation -DiskNumber 77 -Write } 'Corrupt boot file'
    [IO.File]::WriteAllBytes($file, $original)
    [IO.File]::WriteAllBytes($volumePath, (New-Object byte[] (16MB - 512)))
    Expect-Rejection { Invoke-ZosNormalPreparation -DiskNumber 77 -Write } 'Short volume image'
    [IO.File]::WriteAllBytes($volumePath, (New-Object byte[] (16MB)))
    Write-Manifest -Volume @{ file = 'zos-volume.img'; bytes = 16777216; sha256 = $volumeHash; gpt_type = 'ebd0a0a2-b9e5-4433-87c0-68b6b72699c7' }
    Expect-Rejection { Invoke-ZosNormalPreparation -DiskNumber 77 -Write } 'Wrong partition type in manifest'
    Write-Manifest
    $diagnostic = @{ kind = 'zos-usb-diagnostic'; storage_read_only = $true; files = $hashes; volume = $null } | ConvertTo-Json
    Set-Content -LiteralPath (Join-Path $script:ZosNormalPackage 'manifest.json') -Value $diagnostic
    Expect-Rejection { Invoke-ZosNormalPreparation -DiskNumber 77 -Write } 'Diagnostic package used as normal package'
    Write-Manifest
    $unsafe = $hashes.Clone()
    $unsafe['..\evil.elf'] = $hashes['limine.conf']
    @{ kind = 'zos-usb-normal'; storage_read_only = $false; files = $unsafe
       volume = @{ file = 'zos-volume.img'; bytes = 16777216; sha256 = $volumeHash; gpt_type = 'f2ac67b5-91fa-4c46-a324-95b456c3829d' } } |
        ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $script:ZosNormalPackage 'manifest.json')
    Expect-Rejection { Invoke-ZosNormalPreparation -DiskNumber 77 -Write } 'Path traversal in manifest'
    Write-Manifest

    # Full preparation of a disk that already holds an MBR layout.
    Reset-Disk
    $script:ClearedStyle = 'MBR'
    Invoke-ZosNormalPreparation -DiskNumber 77 -Write
    Assert-True (($script:Calls -join ',') -eq 'clear,refresh,convert,boot-partition,format,zos-partition,write-volume') "Unexpected storage commands: $($script:Calls -join ',')"
    Assert-True ($script:Written.Count -eq 1 -and $script:Written[0].Disk -eq 77 -and $script:Written[0].Offset -eq 513MB -and $script:Written[0].Bytes -eq 16MB) 'The volume was not written to exactly the ZOS partition'
    foreach ($name in $names) {
        Assert-True ((Get-FileHash -LiteralPath (Join-Path $script:Destination $name)).Hash -eq $hashes[$name]) "Readback failed: $name"
    }
    # A blank disk is initialized directly as GPT; an empty GPT disk needs no conversion.
    Reset-Disk
    $script:Disk.PartitionStyle = 'RAW'
    $script:Disk.NumberOfPartitions = 0
    Invoke-ZosNormalPreparation -DiskNumber 77 -Write
    Assert-True (($script:Calls -join ',') -eq 'refresh,initialize,boot-partition,format,zos-partition,write-volume') "Blank disk flow: $($script:Calls -join ',')"
    Reset-Disk
    $script:Disk.PartitionStyle = 'GPT'
    $script:ClearedStyle = 'GPT'
    Invoke-ZosNormalPreparation -DiskNumber 77 -Write
    Assert-True (($script:Calls -join ',') -eq 'clear,refresh,boot-partition,format,zos-partition,write-volume') "GPT disk flow: $($script:Calls -join ',')"

    # The ZOS partition is checked before anything is written into it.
    $good = $script:Zos
    foreach ($case in @(
        @{ Name = 'wrong disk'; Change = { $script:Zos = $good.PSObject.Copy(); $script:Zos.DiskNumber = 5 } },
        @{ Name = 'drive letter assigned'; Change = { $script:Zos = $good.PSObject.Copy(); $script:Zos.DriveLetter = 'Y' } },
        @{ Name = 'too small'; Change = { $script:Zos = $good.PSObject.Copy(); $script:Zos.Size = [UInt64](16MB - 512) } },
        @{ Name = 'unaligned'; Change = { $script:Zos = $good.PSObject.Copy(); $script:Zos.Offset = [UInt64](513MB + 100) } },
        @{ Name = 'overlaps the boot partition'; Change = { $script:Zos = $good.PSObject.Copy(); $script:Zos.Offset = [UInt64](400MB) } },
        @{ Name = 'beyond the disk'; Change = { $script:Zos = $good.PSObject.Copy(); $script:Zos.Offset = [UInt64](8GB - 1MB) } }
    )) {
        & $case.Change
        Reset-Disk
        $script:ClearedStyle = 'GPT'
        $script:Disk.PartitionStyle = 'GPT'
        $rejected = $false
        try { Invoke-ZosNormalPreparation -DiskNumber 77 -Write } catch { $rejected = $true }
        Assert-True $rejected "ZOS partition '$($case.Name)' was accepted"
        Assert-True ($script:Written.Count -eq 0) "ZOS partition '$($case.Name)' was written to"
    }
    $script:Zos = $good
    Write-Output 'USB NORMAL SCRIPT PASS: mocked targets, package integrity, GPT layout, ZOS partition checks, verified file copy and volume write'
} finally {
    $resolved = (Get-Item -LiteralPath $root).FullName
    if ((Split-Path -Leaf $resolved) -ne "usb-normal-script-test-$PID") { throw 'Unexpected test cleanup path' }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
