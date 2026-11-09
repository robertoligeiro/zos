$root = Split-Path $PSScriptRoot -Parent
$env:CARGO_HOME = Join-Path $root '.tools\cargo'
$env:RUSTUP_HOME = Join-Path $root '.tools\rustup'
$env:PATH = "$env:CARGO_HOME\bin;$root\.tools\qemu;$env:PATH"
