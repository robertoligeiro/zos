$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\read-zos-volume.ps1"
$root = Join-Path $PSScriptRoot "..\build\read-zos-test-$PID"
$null = New-Item -ItemType Directory -Path $root -Force
$script:Disk = [pscustomobject]@{
    Number = 77; BusType = 'USB'; IsBoot = $false; IsSystem = $false; IsOffline = $false
    LogicalSectorSize = 512; Size = 8GB; FriendlyName = 'File-backed test only'
}
$script:Zos = [pscustomobject]@{ GptType = '{F2AC67B5-91FA-4C46-A324-95B456C3829D}'; Offset = [UInt64](513MB); Size = [UInt64]16MB }
$script:Other = [pscustomobject]@{ GptType = '{EBD0A0A2-B9E5-4433-87C0-68B6B72699C7}'; Offset = [UInt64]1MB; Size = [UInt64]512MB }
$script:Partitions = @($script:Other, $script:Zos)
$script:Magic = 'ZOSFS001'
$script:Reads = @()
$script:IsAdmin = $true
function Get-Disk { param($Number, $ErrorAction) return $script:Disk }
function Get-Partition { param($DiskNumber, $ErrorAction) return $script:Partitions }
function Test-ZosAdministrator { return $script:IsAdmin }
function Read-ZosDiskBytes {
    param([int]$DiskNumber, [UInt64]$Offset, [int]$Count)
    $script:Reads += [pscustomobject]@{ Disk = $DiskNumber; Offset = $Offset; Count = $Count }
    $bytes = New-Object byte[] $Count
    [Text.Encoding]::ASCII.GetBytes($script:Magic).CopyTo($bytes, 0)
    return $bytes
}
function Assert-True { param($Value, $Message) if (-not $Value) { throw $Message } }
function Expect-Rejection {
    param([scriptblock]$Action, [string]$Message)
    $script:Reads = @()
    $rejected = $false
    try { & $Action } catch { $rejected = $true }
    Assert-True $rejected "$Message was accepted"
    Assert-True ($script:Reads.Count -eq 0) "$Message read the disk"
}
try {
    $out = Join-Path $root 'dump.img'
    Expect-Rejection { Invoke-ZosVolumeDump } 'Missing disk number'
    foreach ($property in @('IsBoot', 'IsSystem', 'IsOffline')) {
        $script:Disk.$property = $true
        Expect-Rejection { Invoke-ZosVolumeDump -DiskNumber 77 -OutFile $out } $property
        $script:Disk.$property = $false
    }
    $script:Disk.BusType = 'NVMe'
    Expect-Rejection { Invoke-ZosVolumeDump -DiskNumber 77 -OutFile $out } 'Internal disk'
    $script:Disk.BusType = 'USB'
    $script:Partitions = @($script:Other)
    Expect-Rejection { Invoke-ZosVolumeDump -DiskNumber 77 -OutFile $out } 'No ZOS partition'
    $script:Partitions = @($script:Zos, $script:Zos)
    Expect-Rejection { Invoke-ZosVolumeDump -DiskNumber 77 -OutFile $out } 'Two ZOS partitions'
    $script:Partitions = @($script:Other, $script:Zos)
    $script:IsAdmin = $false
    Expect-Rejection { Invoke-ZosVolumeDump -DiskNumber 77 -OutFile $out } 'Not administrator'
    $script:IsAdmin = $true
    Expect-Rejection { Invoke-ZosVolumeDump -DiskNumber 77 -OutFile '\\.\PhysicalDrive1' } 'Device output path'
    # An unformatted or foreign partition is read (to find out) but never saved.
    $script:Magic = 'NOTZOS!!'
    $rejected = $false
    try { Invoke-ZosVolumeDump -DiskNumber 77 -OutFile $out } catch { $rejected = $true }
    Assert-True $rejected 'Foreign partition was accepted'
    Assert-True (-not (Test-Path -LiteralPath $out)) 'A foreign partition was saved'
    $script:Magic = 'ZOSFS001'
    $script:Reads = @()
    Invoke-ZosVolumeDump -DiskNumber 77 -OutFile $out
    Assert-True ($script:Reads.Count -eq 1 -and $script:Reads[0].Disk -eq 77 -and $script:Reads[0].Offset -eq 513MB -and $script:Reads[0].Count -eq 16MB) 'Read was not exactly the ZOS partition'
    Assert-True ((Get-Item -LiteralPath $out).Length -eq 16MB) 'Dump has the wrong size'
    Write-Output 'READ ZOS VOLUME SCRIPT PASS: mocked refusal checks, exact partition range, read-only dump'
} finally {
    $resolved = (Get-Item -LiteralPath $root).FullName
    if ((Split-Path -Leaf $resolved) -ne "read-zos-test-$PID") { throw 'Unexpected test cleanup path' }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
