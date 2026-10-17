<#
.SYNOPSIS
  Compila MemRescue: motor Rust (workspace), CLI, DLL y solución .NET (WPF + interop + pruebas).
.PARAMETER Configuration
  Debug (por defecto) o Release.
.PARAMETER Publish
  Además publica MemRescue autocontenido en artifacts\MemRescue (requiere Release).
#>
param(
    [ValidateSet('Debug', 'Release')] [string] $Configuration = 'Debug',
    [switch] $Publish
)
. "$PSScriptRoot\env.ps1"
Set-Location $Root

$cargoProfile = @()
if ($Configuration -eq 'Release') { $cargoProfile = @('--release') }

Write-Host '== cargo build --workspace' -ForegroundColor Cyan
cargo build --workspace @cargoProfile
if ($LASTEXITCODE -ne 0) { exit 1 }

Write-Host "== dotnet build ($Configuration)" -ForegroundColor Cyan
& $Dotnet build app\MemRescue.slnx -c $Configuration -p:SkipCargoBuild=true
if ($LASTEXITCODE -ne 0) { exit 1 }

if ($Publish) {
    Write-Host '== dotnet publish (autocontenido, win-x64)' -ForegroundColor Cyan
    & $Dotnet publish app\src\MemRescue\MemRescue.csproj -c Release -r win-x64 --self-contained true -o artifacts\MemRescue
    if ($LASTEXITCODE -ne 0) { exit 1 }
}
Write-Host 'Compilación correcta.' -ForegroundColor Green
