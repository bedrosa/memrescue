# Entorno común de los scripts de MemRescue. Uso: . "$PSScriptRoot\env.ps1"
$ErrorActionPreference = 'Continue'
$script:Root = Split-Path -Parent $PSScriptRoot
$localDotnet = Join-Path $Root '.tools\dotnet\dotnet.exe'
if (Test-Path $localDotnet) {
    # SDK portable del proyecto (ver docs/DECISIONES.md, ADR-011). No se modifica el PATH del sistema.
    $env:DOTNET_ROOT = Split-Path -Parent $localDotnet
    $script:Dotnet = $localDotnet
} else {
    $script:Dotnet = 'dotnet'
}
$env:DOTNET_CLI_TELEMETRY_OPTOUT = '1'
$env:DOTNET_NOLOGO = '1'
$script:Python = 'python'
