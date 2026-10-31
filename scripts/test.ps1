<#
.SYNOPSIS
  Ejecuta todas las comprobaciones automatizadas de MemRescue y resume el resultado.
  Solo usa ficheros sintéticos; no accede a discos ni requiere administrador.
.PARAMETER SkipGui
  Omite el autodiagnóstico que abre la ventana de MemRescue.exe.
.PARAMETER SkipQemu
  Omite las pruebas cruzadas de formatos contra qemu-img (si falta qemu-img y no se omiten, el paso falla).
.PARAMETER SkipNtfsCorpus
  Omite las pruebas del lector NTFS contra el corpus NTFS (si falta el corpus y no se omiten, el paso
  falla e indica cómo descargarlo con tools\fetch_ntfs_corpus.py).
#>
param([switch] $SkipGui, [switch] $SkipQemu, [switch] $SkipNtfsCorpus)
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
$ntfsGen = 'tests\corpus\ntfs\generated'

Step 'Repositorio: higiene (rutas, credenciales, material excluido, U+2014)' { & $Python -I -X utf8 tools\check_repo_hygiene.py }
Step 'Rust: formato (cargo fmt --check)' { cargo fmt --all -- --check }
Step 'Rust: análisis (clippy -D warnings)' { cargo clippy --workspace --all-targets -- -D warnings }
Step 'Rust: pruebas unitarias e integración' { cargo test --workspace }
Step 'Corpus: generación determinista' { & $Python -I -X utf8 tests\generators\gen_partition_images.py $gen }
# Huella de los orígenes tras generarlos: al final se comprueba que nada los ha modificado.
$fingerprint = @(Get-ChildItem "$gen\*.img") + @(Get-ChildItem "$ntfsGen\*.img" -ErrorAction SilentlyContinue) | ForEach-Object {
    [pscustomobject]@{ Path = $_.FullName; Hash = (Get-FileHash $_.FullName).Hash; Mtime = $_.LastWriteTimeUtc; Len = $_.Length }
}
Step 'Corpus: validación independiente vs manifiestos versionados' { & $Python -I -X utf8 tests\generators\validate_partition_images.py $gen $refs }
Step 'Corpus: pruebas negativas del validador' { & $Python -I -X utf8 tests\generators\test_validator.py $gen $refs }
Step 'Rust: pruebas cruzadas con el corpus' { cargo test -p memrescue-cli --test corpus -- --ignored }
Step 'Rust: tablas de particiones y búsqueda de perdidas contra el corpus' { cargo test -p mr-part -- --ignored }
if (-not $SkipNtfsCorpus) {
    # Corpus NTFS (ADR-024): artefacto del workflow ntfs-corpus.yml, verificado con SHA-256 al descargarlo.
    Step 'Rust: lector NTFS y ntfs-info contra el corpus NTFS' {
        if (-not (Test-Path "$ntfsGen\ntfs_basic.img")) {
            Write-Host "  Falta el corpus NTFS en $ntfsGen. Descárguelo con:"
            Write-Host "    python -I -X utf8 tools\fetch_ntfs_corpus.py   (necesita gh autenticado)"
            Write-Host "  o ejecute este script con -SkipNtfsCorpus."
            $global:LASTEXITCODE = 1; return
        }
        cargo test -p mr-fs-ntfs --test ntfs_corpus -- --ignored
        if ($LASTEXITCODE -ne 0) { return }
        cargo test -p memrescue-cli --test ntfs_corpus -- --ignored
        if ($LASTEXITCODE -ne 0) { return }
        cargo test -p mr-fs-ntfs --test ntfs_deleted_corpus -- --ignored
        if ($LASTEXITCODE -ne 0) { return }
        cargo test -p memrescue-cli --test ntfs_deleted -- --ignored
        if ($LASTEXITCODE -ne 0) { return }
        cargo test -p mr-vfs --test vfs_corpus -- --ignored
        if ($LASTEXITCODE -ne 0) { return }
        cargo test -p memrescue-cli --test vfs_corpus -- --ignored
        if ($LASTEXITCODE -ne 0) { return }
        cargo test -p mr-recover --test corpus -- --ignored
        if ($LASTEXITCODE -ne 0) { return }
        cargo test -p memrescue-cli --test recover -- --ignored
    }
}
if (-not $SkipQemu) {
    # Referencia independiente: qemu-img portable verificado (docs/HERRAMIENTAS_TERCEROS.md). Solo se usa
    # sobre imágenes temporales creadas por las pruebas, con convert/info/map (nunca reparación).
    $qemu = Join-Path $Root '.tools\qemu\bin\qemu-img.exe'
    Step 'Formatos: VHD/VHDX/VMDK y corpus contra qemu-img' {
        if (-not (Test-Path $qemu)) { Write-Host "  No se encuentra $qemu (use -SkipQemu para omitir)"; $global:LASTEXITCODE = 1; return }
        $env:MR_QEMU_IMG = $qemu
        cargo test --release -p mr-image -p mr-api -- --ignored
    }
}
Step '.NET: compilación (warnings = errores)' { & $Dotnet build app\MemRescue.slnx -c Debug }
# Las pruebas de .NET con el corpus NTFS (interop y UI Automation) llevan la categoría NtfsCorpus.
$dotnetFilter = if ($SkipNtfsCorpus) { @('--filter', 'Category!=NtfsCorpus') } else { @() }
Step '.NET: pruebas de interop y de interfaz (xUnit + UI Automation)' { & $Dotnet test app\MemRescue.slnx -c Debug --no-build @dotnetFilter }

function Invoke-GuiSelfTest([string] $name) {
    $manifest = "$refs\$name.manifest.toml"
    $get = { param($k) (Select-String $manifest -Pattern "^$k = `"?([^`"]+)`"?$").Matches[0].Groups[1].Value }
    $ss = & $get 'sector_size'; $sectors = & $get 'total_sectors'; $sha = & $get 'sha256'
    $exe = Join-Path $Root 'app\src\MemRescue\bin\Debug\net10.0-windows\win-x64\MemRescue.exe'
    $out = Join-Path $env:TEMP "memrescue-selftest-$name-$PID.txt"
    Remove-Item $out -ErrorAction SilentlyContinue
    $img = Join-Path $Root "$gen\$name.img"
    $p = Start-Process $exe -ArgumentList '--selftest', "`"$out`"", "`"$img`"", '--sector-size', $ss, '--read-lba', '1' -PassThru
    if (-not $p.WaitForExit(120000)) { $p.Kill(); Write-Host 'TIMEOUT'; $global:LASTEXITCODE = 1; return }
    $lines = Get-Content $out
    $lines | ForEach-Object { Write-Host "  $_" }
    $checks = @(
        ($p.ExitCode -eq 0), ($lines[0] -eq 'OK'),
        ($lines -contains "sha256=$sha"), ($lines -contains "image_sectors=$sectors"),
        ($lines -contains "sector_size=$ss"), ($lines -contains "verify_ok=$sectors"), ($lines -contains 'verify_bad=0'),
        # LBA 1 de un disco GPT empieza por "EFI PART"
        ($lines -contains 'lba1_hex16=4546492050415254000001005C000000')
    )
    $global:LASTEXITCODE = $(if ($checks -notcontains $false) { 0 } else { 1 })
}

if (-not $SkipGui) {
    Step 'GUI: autodiagnóstico (GPT, 512 B/sector)' { Invoke-GuiSelfTest 'gpt_basic' }
    Step 'GUI: autodiagnóstico (GPT 4Kn, 4096 B/sector)' { Invoke-GuiSelfTest 'gpt_4kn' }
}

Step 'Orígenes intactos tras todas las operaciones' {
    $changed = @($fingerprint | Where-Object {
        $f = Get-Item $_.Path
        (Get-FileHash $_.Path).Hash -ne $_.Hash -or $f.LastWriteTimeUtc -ne $_.Mtime -or $f.Length -ne $_.Len
    })
    $changed | ForEach-Object { Write-Host "  MODIFICADO: $($_.Path)" -ForegroundColor Red }
    Write-Host "  $($fingerprint.Count) imágenes comprobadas, $($changed.Count) modificadas"
    $global:LASTEXITCODE = $(if ($changed.Count -eq 0 -and $fingerprint.Count -gt 0) { 0 } else { 1 })
}

Write-Host "`n== Resumen" -ForegroundColor Cyan
$results | Format-Table -AutoSize | Out-String | Write-Host
$failed = @($results | Where-Object Resultado -ne 'OK').Count
if ($failed -gt 0) { Write-Host "$failed paso(s) con fallos." -ForegroundColor Red; exit 1 }
Write-Host 'Todas las comprobaciones han pasado.' -ForegroundColor Green
exit 0
