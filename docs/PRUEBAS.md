# Estrategia de pruebas

Principio: **ninguna prueba toca discos físicos reales** hasta M9, y aun entonces solo en VM con
discos virtuales. Todas las fuentes se abren en solo lectura.

## 1. Corpus de imágenes sintéticas

- Ubicación: `tests/corpus/`. Imágenes pequeñas (≤ 64 MiB, comprimidas) versionadas; las grandes
  se **generan** con scripts deterministas en `tests/generators/` y no se versionan.
- Cada imagen tiene un **manifiesto** (`*.manifest.toml`): cómo se generó, SHA-256 de la imagen,
  y la **verdad esperada** (particiones, árbol de ficheros, tamaños, hashes de contenido, ficheros
  borrados y su recuperabilidad esperada).
- Generación con herramientas **independientes** de MemRescue (oráculos de creación):
  - Windows: `diskpart`/`Format-Volume` sobre VHD/VHDX (NTFS, FAT32, exFAT, ReFS si el SO lo permite).
  - Linux (WSL o VM): `sfdisk`, `mkfs.fat`, `mkfs.exfat`, `mkfs.ext2/3/4`, `mkfs.btrfs`, `mkfs.hfsplus`,
    `mdadm` (RAID), `qemu-img` (VMDK/VHD), `xorriso` (ISO).
  - APFS y E01: ver INV-10 e INV-11.
- Las imágenes del sistema de pruebas se crean y destruyen en carpetas temporales; nunca se montan
  sobre letras del sistema en la máquina del desarrollador sin VM.

## 2. Corrupción determinista

Transformaciones reproducibles con semilla sobre una imagen sana:
borrar tabla de particiones · sobrescribir boot sector (con/sin copia) · dañar N registros MFT ·
borrar ficheros y escribir datos encima · desplazar la imagen (recortar inicio) · truncar ·
dañar FAT1 o FAT2 · eliminar un disco de un RAID · invertir el orden de discos.

## 3. Dispositivo simulado

`SimulatedDevice` sobre una imagen con un **mapa de fallos**: sectores ilegibles permanentes,
fallos intermitentes (fallan k veces y luego leen), latencias, desconexión en el sector N.
Permite probar `ResilientReader`, clonado y recuperación con errores de forma determinista.

## 4. Tipos de prueba

| Tipo | Herramienta | Alcance |
|---|---|---|
| Unitarias | `cargo test` | Parsers, matcher de firmas, mapeo RAID, hashes |
| Propiedades | `proptest` | Ida y vuelta, monotonía (Más ⊇ Menos), invariantes de rangos |
| Fuzzing | `cargo fuzz` | Cada parser de estructura (boot, MFT, GPT, superbloques, INDX…): sin pánicos ni bucles |
| Integración | CLI + corpus | Abrir imagen → particiones → árbol → recuperar → comparar hashes con el manifiesto |
| Oráculo cruzado | Herramientas libres independientes (p. ej. The Sleuth Kit, `ntfs-3g`, `debugfs`) | Comparar listados de ficheros sobre las mismas imágenes |
| Rendimiento | `criterion` + imágenes grandes generadas | MB/s de escaneo, tiempo hasta primer volumen |
| UI | Autodiagnóstico `--selftest` + captura `--screenshot` (M0); UI Automation (M10) | Flujos principales; controles no implementados deben aparecer deshabilitados |
| Seguridad de escritura | Integración | Verificar que en modo por defecto ninguna operación escribe (hash del origen antes/después) |

## 5. Criterio para declarar paridad (ADR-002)

Una fila de `PARIDAD_DMDE_PRO.md` pasa a **Paridad** solo si:
1. Hay al menos una prueba automatizada que cubre el comportamiento descrito en su columna "Ref. DMDE".
2. La prueba usa imágenes del corpus con manifiesto (reproducible por cualquiera).
3. Se documentan las diferencias conocidas restantes (o "ninguna").
4. Opcional y recomendado: comparación manual con DMDE sobre la **misma imagen sintética**, en una
   VM, cuando el propietario autorice ejecutar DMDE (no autorizado en las fases iniciales).

## 6. Cómo ejecutar (estado M0)

```powershell
# Desde la raíz del repositorio (no requiere administrador)
powershell -ExecutionPolicy Bypass -File scripts\build.ps1                 # compila todo (Debug)
powershell -ExecutionPolicy Bypass -File scripts\test.ps1                  # 10 pasos de verificación
powershell -ExecutionPolicy Bypass -File scripts\build.ps1 -Configuration Release -Publish   # exe autocontenido
```

Pasos de `test.ps1`:

| # | Paso | Qué demuestra |
|---|---|---|
| 1 | `cargo fmt --check` | Formato canónico |
| 2 | `cargo clippy -D warnings` | Sin advertencias del analizador |
| 3 | `cargo test --workspace` | 34 pruebas unitarias/integración del motor y la CLI |
| 4 | Generación del corpus | Las imágenes se pueden regenerar |
| 5 | Validación independiente | Las imágenes cumplen la especificación MBR/GPT y su SHA-256 es idéntico al de los manifiestos versionados (determinismo) |
| 6 | Pruebas negativas | El validador detecta 9 tipos de corrupción (no es un validador que siempre aprueba) |
| 7 | Pruebas cruzadas Rust ↔ corpus | SHA-256 y CRC-32 del motor coinciden con `hashlib`/`zlib` de Python |
| 8 | `dotnet build` | Compilación .NET con advertencias como errores |
| 9 | `dotnet test` | 47 pruebas de la frontera C# ↔ Rust contra la DLL real (incluye corpus) |
| 10–11 | Autodiagnóstico GUI (512 y 4096 B/sector) | `MemRescue.exe` abre la imagen, lee la LBA 1 (`EFI PART`), sectores y SHA-256 = manifiesto, verificación sin sectores ilegibles |
| 12 | Orígenes intactos | SHA-256, tamaño y fecha de las imágenes iguales antes y después de todas las operaciones |

(Desde M1 el paso 3 ejecuta 77 pruebas Rust y el 7 incluye la lectura de marcadores en LBA exactas.)

Corpus actual (`tests/corpus/manifests`, imágenes en `tests/corpus/generated`, no versionadas):

| Imagen | Contenido |
|---|---|
| `mbr_basic` | 16 MiB, 512 B/sector, MBR con 2 primarias (una activa) + extendida con cadena de 2 EBR y 2 lógicas |
| `gpt_basic` | 16 MiB, 512 B/sector, MBR protector, GPT primaria + copia, 3 particiones (datos, Linux, EFI) |
| `gpt_4kn` | 8 MiB, 4096 B/sector, GPT con 1 partición |

Limitación conocida: el validador lo ha escrito el mismo equipo que el generador (aunque son códigos
separados). Falta un oráculo de terceros (`sgdisk`/`sfdisk`), que requiere WSL o una VM (ver
[PLAN_CORPUS_NTFS.md](PLAN_CORPUS_NTFS.md) §4).

## 7. Registro

Los resultados de cada hito se anotan en `PROGRESO.md` con: suites ejecutadas, nº de pruebas,
fallos conocidos y versión del corpus.
