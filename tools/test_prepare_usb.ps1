$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\prepare-usb.ps1"
$root = Join-Path $PSScriptRoot "..\build\usb-script-test-$PID"
$script:ZosPackage = Join-Path $root 'package'
$script:Destination = Join-Path $root 'destination'
$null = New-Item -ItemType Directory -Path $script:ZosPackage -Force
$null = New-Item -ItemType Directory -Path $script:Destination -Force
$names = @('EFI\BOOT\BOOTX64.EFI', 'limine.conf', 'boot\zos.elf', 'boot\console-server.elf', 'boot\input-server.elf')
$hashes = @{}
foreach ($name in $names) {
    $file = Join-Path $script:ZosPackage $name
    $null = New-Item -ItemType Directory -Path (Split-Path -Parent $file) -Force
    Set-Content -LiteralPath $file -Value "fixture $name"
    $hashes[$name] = (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLowerInvariant()
}
@{ kind = 'zos-usb-diagnostic'; storage_read_only = $true; files = $hashes } |
    ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:ZosPackage 'manifest.json')
$script:Disk = [pscustomobject]@{
    Number = 77; BusType = 'USB'; IsBoot = $false; IsSystem = $false
    IsOffline = $false; IsReadOnly = $false; Size = 8GB; LogicalSectorSize = 512
    UniqueId = 'MOCK-USB'; FriendlyName = 'File-backed test only'; PartitionStyle = 'MBR'; NumberOfPartitions = 1
}
$script:Calls = @()
$script:Partition = [pscustomobject]@{ DiskNumber = 77; DriveLetter = 'Z'; IsBoot = $false; IsSystem = $false }
$script:Swap = $false
$script:Reads = 0
$script:Answer = 'ERASE USB DISK 77'
$script:ClearedStyle = 'RAW'
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
    return $script:Partition
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
    if ($script:Disk.PartitionStyle -ne 'RAW') { throw 'The disk has already been initialized.' }
    $script:Calls += 'initialize'
    $script:Disk.PartitionStyle = $PartitionStyle
}
function Set-Disk {
    param($Number, $PartitionStyle, $ErrorAction)
    if ($script:Disk.PartitionStyle -ne 'GPT' -or $script:Disk.NumberOfPartitions -ne 0) {
        throw 'Only an empty GPT disk should need conversion.'
    }
    $script:Calls += 'convert'
    $script:Disk.PartitionStyle = $PartitionStyle
}
function New-Partition {
    param($DiskNumber, $Size, $MbrType, [switch]$AssignDriveLetter, $ErrorAction)
    if ($script:Disk.PartitionStyle -ne 'MBR' -or $script:Disk.NumberOfPartitions -ne 0) {
        throw 'The mock disk must be empty and MBR before partitioning.'
    }
    $script:Calls += 'partition'
    $script:Disk.NumberOfPartitions = 1
    return $script:Partition
}
function Format-Volume {
    param([Parameter(ValueFromPipeline = $true)]$InputObject, $FileSystem, $AllocationUnitSize, $NewFileSystemLabel, $Confirm)
    process { $script:Calls += 'format' }
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
try {
    Expect-Rejection { Invoke-ZosUsbPreparation } 'Missing disk number'
    foreach ($property in @('IsBoot', 'IsSystem', 'IsOffline', 'IsReadOnly')) {
        $script:Disk.$property = $true
        Expect-Rejection { Invoke-ZosUsbPreparation -DiskNumber 77 -Write } $property
        $script:Disk.$property = $false
    }
    $script:Disk.BusType = 'NVMe'
    Expect-Rejection { Invoke-ZosUsbPreparation -DiskNumber 77 -Write } 'Internal disk'
    $script:Disk.BusType = 'USB'
    $script:Disk.LogicalSectorSize = 4096
    Expect-Rejection { Invoke-ZosUsbPreparation -DiskNumber 77 -Write } 'Unsupported sectors'
    $script:Disk.LogicalSectorSize = 512
    $script:Disk.Size = 128MB
    Expect-Rejection { Invoke-ZosUsbPreparation -DiskNumber 77 -Write } 'Small disk'
    $script:Disk.Size = 8GB
    Invoke-ZosUsbPreparation -DiskNumber 77
    Invoke-ZosUsbPreparation -DiskNumber 77 -Write -WhatIf
    Assert-True ($script:Calls.Count -eq 0) 'Preview or WhatIf touched storage'
    $script:Answer = 'yes'
    Expect-Rejection { Invoke-ZosUsbPreparation -DiskNumber 77 -Write } 'Incorrect confirmation'
    $script:Answer = 'ERASE USB DISK 77'
    $script:Swap = $true
    $script:Reads = 0
    Expect-Rejection { Invoke-ZosUsbPreparation -DiskNumber 77 -Write } 'Changed identity'
    $script:Swap = $false
    $file = Join-Path $script:ZosPackage 'boot\zos.elf'
    $original = [IO.File]::ReadAllBytes($file)
    Set-Content -LiteralPath $file -Value 'corrupted'
    Expect-Rejection { Invoke-ZosUsbPreparation -DiskNumber 77 -Write } 'Corrupt package'
    [IO.File]::WriteAllBytes($file, $original)
    Invoke-ZosUsbPreparation -DiskNumber 77 -Write
    Assert-True (($script:Calls -join ',') -eq 'clear,refresh,initialize,partition,format') 'Unexpected storage commands'
    foreach ($name in $names) {
        Assert-True ((Get-FileHash -LiteralPath (Join-Path $script:Destination $name)).Hash -eq $hashes[$name]) "Readback failed: $name"
    }
    $script:Disk.PartitionStyle = 'RAW'
    $script:Disk.NumberOfPartitions = 0
    $script:Calls = @()
    Invoke-ZosUsbPreparation -DiskNumber 77 -Write
    Assert-True (($script:Calls -join ',') -eq 'refresh,initialize,partition,format') 'Blank USB disk was not initialized directly'
    $script:ClearedStyle = 'MBR'
    $script:Calls = @()
    Invoke-ZosUsbPreparation -DiskNumber 77 -Write
    Assert-True (($script:Calls -join ',') -eq 'clear,refresh,partition,format') 'Already initialized MBR disk was reinitialized'
    $script:Disk.PartitionStyle = 'GPT'
    $script:ClearedStyle = 'GPT'
    $script:Calls = @()
    Invoke-ZosUsbPreparation -DiskNumber 77 -Write
    Assert-True (($script:Calls -join ',') -eq 'clear,refresh,convert,partition,format') 'Empty GPT disk was not converted to MBR'
    $script:Disk.PartitionStyle = 'MBR'
    $script:Disk.NumberOfPartitions = 0
    $script:Calls = @()
    Invoke-ZosUsbPreparation -DiskNumber 77 -Write
    Assert-True (($script:Calls -join ',') -eq 'refresh,partition,format') 'Retry on an already empty MBR disk failed'
    Write-Output 'USB SCRIPT PASS: mocked targets, refusal/preview/identity/hash checks, drive-root handling, verified file copy'
} finally {
    $resolved = (Get-Item -LiteralPath $root).FullName
    if ((Split-Path -Leaf $resolved) -ne "usb-script-test-$PID") { throw 'Unexpected test cleanup path' }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
