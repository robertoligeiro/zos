[CmdletBinding()]
param(
    [int]$DiskNumber = -1,
    [switch]$List,
    [string]$OutFile
)

# READ-ONLY: copies the 16 MiB ZOS data partition of a prepared USB stick into a file so
# its files (for example /boot.log) can be read with tools\zosfs.py. It opens the
# physical drive for reading only and never writes to any disk.

Set-StrictMode -Version 2
$ErrorActionPreference = 'Stop'
# GPT type of the ZOS data partition; it must match servers\console\src\usb\gpt.rs.
$script:ZosPartitionType = '{f2ac67b5-91fa-4c46-a324-95b456c3829d}'
$script:ZosVolumeBytes = 16MB

function Test-ZosAdministrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Read-ZosDiskBytes {
    param([int]$DiskNumber, [UInt64]$Offset, [int]$Count)
    $chunk = 1MB
    if ($Offset % 512 -ne 0 -or $Count % 512 -ne 0) { throw 'Read range is not sector-aligned.' }
    # FileAccess.Read: this handle cannot write.
    $stream = New-Object IO.FileStream("\\.\PhysicalDrive$DiskNumber", [IO.FileMode]::Open,
        [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite, 1)
    try {
        $stream.Position = [int64]$Offset
        $bytes = New-Object byte[] $Count
        $done = 0
        while ($done -lt $Count) {
            $read = $stream.Read($bytes, $done, [Math]::Min($chunk, $Count - $done))
            if ($read -le 0) { throw 'Short read from the USB disk.' }
            $done += $read
        }
        return $bytes
    } finally {
        $stream.Dispose()
    }
}

function Invoke-ZosVolumeDump {
    param([int]$DiskNumber = -1, [switch]$List, [string]$OutFile)
    if ($List) {
        Get-Disk -ErrorAction Stop | Where-Object BusType -EQ 'USB' |
            Select-Object Number, FriendlyName, Size, IsBoot, IsSystem
        return
    }
    if ($DiskNumber -lt 0) { throw 'Select an explicit -DiskNumber. Use -List first.' }
    $disk = Get-Disk -Number $DiskNumber -ErrorAction Stop
    if ($null -eq $disk -or $disk.Number -ne $DiskNumber -or $disk.BusType -ne 'USB' -or $disk.IsBoot -or
        $disk.IsSystem -or $disk.IsOffline -or $disk.LogicalSectorSize -ne 512 -or $disk.Size -lt 1GB) {
        throw 'Refusing target: require an online, non-boot/non-system USB disk with 512-byte sectors.'
    }
    $zos = @(Get-Partition -DiskNumber $DiskNumber -ErrorAction Stop |
        Where-Object { "$($_.GptType)" -eq $script:ZosPartitionType })
    if ($zos.Count -ne 1) { throw "Expected exactly one ZOS data partition on disk $DiskNumber, found $($zos.Count)." }
    $partition = $zos[0]
    if ($partition.Size -lt $script:ZosVolumeBytes -or $partition.Offset % 512 -ne 0 -or
        ($partition.Offset + $script:ZosVolumeBytes) -gt $disk.Size) {
        throw 'The ZOS partition has an unexpected size or position.'
    }
    if (-not (Test-ZosAdministrator)) {
        throw 'Reading a physical drive needs an already-administrative PowerShell session. This script does not elevate itself.'
    }
    if (-not $OutFile) { $OutFile = Join-Path $PSScriptRoot '..\build\zos-volume-dump.img' }
    if ($OutFile -match '^\\\\[.?]\\') { throw 'The output must be an ordinary file, not a device.' }
    $target = [IO.Path]::GetFullPath($OutFile)
    $null = New-Item -ItemType Directory -Path (Split-Path -Parent $target) -Force
    $bytes = Read-ZosDiskBytes -DiskNumber $DiskNumber -Offset $partition.Offset -Count $script:ZosVolumeBytes
    if ([Text.Encoding]::ASCII.GetString($bytes, 0, 8) -ne 'ZOSFS001') {
        throw 'The ZOS partition does not start with a ZOS filesystem (unformatted or foreign data). Nothing was saved.'
    }
    [IO.File]::WriteAllBytes($target, $bytes)
    $hash = (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash.ToLowerInvariant()
    Write-Host "Saved the 16 MiB ZOS volume from disk $DiskNumber to $target (sha256 $hash)."
    Write-Host "List its files:   python tools\zosfs.py ls `"$target`""
    Write-Host "Read the boot log: python tools\zosfs.py cat `"$target`" /boot.log"
}

if ($MyInvocation.InvocationName -ne '.') {
    Invoke-ZosVolumeDump -DiskNumber $DiskNumber -List:$List -OutFile $OutFile
}
