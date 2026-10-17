<#
.SYNOPSIS
  Ejecuta todas las comprobaciones automatizadas de MemRescue y resume el resultado.
  Solo usa ficheros sintéticos; no accede a discos ni requiere administrador.
.PARAMETER SkipGui
  Omite el autodiagnóstico que abre la ventana de MemRescue.exe.
#>
param([switch] $SkipGui)
. "$PSScriptRoot\env.ps1"
Set-Location $Root

$results = [System.Collections.Generic.List[object]]::new()
function Step([string] $name, [scriptblock] $body) {
    Write-Host "`n== $name" -ForegroundColor Cyan
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $global:LASTEXITCODE = 0
    & $body
    $ok = ($LASTEXITCODE -eq 0)
    $results.Add([pscustomobject]@{ Paso = $name; Resultado = $(if ($ok) { 'OK' } else { 'FALLO' }); Segundos = [math]::Round($sw.Elapsed.TotalSeconds, 1) })
}

$gen = 'tests\corpus\generated'
$refs = 'tests\corpus\manifests'

Step 'Rust: formato (cargo fmt --check)' { cargo fmt --all -- --check }
Step 'Rust: análisis (clippy -D warnings)' { cargo clippy --workspace --all-targets -- -D warnings }
Step 'Rust: pruebas unitarias e integración' { cargo test --workspace }
Step 'Corpus: generación determinista' { & $Python -I -X utf8 tests\generators\gen_partition_images.py $gen }
Step 'Corpus: validación independiente vs manifiestos versionados' { & $Python -I -X utf8 tests\generators\validate_partition_images.py $gen $refs }
Step 'Corpus: pruebas negativas del validador' { & $Python -I -X utf8 tests\generators\test_validator.py $gen $refs }
Step 'Rust: pruebas cruzadas con el corpus' { cargo test -p memrescue-cli --test corpus -- --ignored }
Step '.NET: compilación (warnings = errores)' { & $Dotnet build app\MemRescue.slnx -c Debug }
Step '.NET: pruebas de interop (xUnit)' { & $Dotnet test app\MemRescue.slnx -c Debug --no-build }

if (-not $SkipGui) {
    Step 'GUI: autodiagnóstico de MemRescue.exe' {
        $exe = Join-Path $Root 'app\src\MemRescue\bin\Debug\net10.0-windows\win-x64\MemRescue.exe'
        $out = Join-Path $env:TEMP "memrescue-selftest-$PID.txt"
        $img = Join-Path $Root "$gen\gpt_basic.img"
        Remove-Item $out -ErrorAction SilentlyContinue
        $p = Start-Process $exe -ArgumentList '--selftest', "`"$out`"", "`"$img`"" -PassThru
        if (-not $p.WaitForExit(120000)) { $p.Kill(); Write-Host 'TIMEOUT'; $global:LASTEXITCODE = 1; return }
        $lines = Get-Content $out
        $lines | ForEach-Object { Write-Host "  $_" }
        $expected = (Select-String "$refs\gpt_basic.manifest.toml" -Pattern '^sha256 = "(.+)"').Matches[0].Groups[1].Value
        $ok = $p.ExitCode -eq 0 -and $lines[0] -eq 'OK' -and ($lines -contains "sha256=$expected")
        $global:LASTEXITCODE = $(if ($ok) { 0 } else { 1 })
    }
}

Write-Host "`n== Resumen" -ForegroundColor Cyan
$results | Format-Table -AutoSize | Out-String | Write-Host
$failed = @($results | Where-Object Resultado -ne 'OK').Count
if ($failed -gt 0) { Write-Host "$failed paso(s) con fallos." -ForegroundColor Red; exit 1 }
Write-Host 'Todas las comprobaciones han pasado.' -ForegroundColor Green
exit 0
