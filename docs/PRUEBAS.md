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
powershell -ExecutionPolicy Bypass -File scripts\test.ps1                  # 14 pasos (-SkipGui, -SkipQemu)
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

Desde M1-iteración 2 (13 pasos): el paso 3 ejecuta **176** pruebas Rust; un paso nuevo, **«Formatos contra
qemu-img»**, ejecuta las 9 pruebas ignoradas por defecto (`MR_QEMU_IMG`): VHD/VHDX/VMDK generados por
qemu-img comparados byte a byte y en su mapa de dispersos, rechazo de streamOptimized/flat/diferenciales y
el corpus GPT/MBR convertido a 5 variantes de contenedor y leído con la detección automática. El paso de
`dotnet test` ejecuta 52 pruebas de interop y **9 de interfaz** con UI Automation (`PRUEBAS_UI.md`).
Documentación de cada formato y de sus pruebas en `formatos/`.

Modo tolerante de contenedores (issue #4, ADR-020): pruebas unitarias por formato con comparación sector
a sector contenido ↔ estado, 2 pruebas más contra qemu-img (11 ignoradas en total), y pruebas de API,
FFI, CLI (`cli/tests/tolerant.rs`), C# e interfaz (11 pruebas de UI Automation).

Patrones de relleno configurables y colores por estado (issue #7, ADR-021): mr-io (formato y regla de
admisión, patrones propios con salto comprobados sector a sector con `assert_honest`), mr-api
(`read_sectors_mapped_with`, hash estricto con cualquier patrón), mr-ffi (`mr_read_options`: patrones,
salto, lectura hacia atrás, igualdad con `mr_source_read_mapped` por defecto, validación de tamaño, bits,
patrones y punteros), CLI (`sectors --bad-fill/--skip-fill/--jump` sobre el VHD dañado y rechazo de
patrones no admitidos con código 2), C# (`FillPatternTests`: 27 casos de patrones, preferencias con
fichero ausente, dañado o desmesurado, ubicación sustituible y paleta con contraste ≥ 7:1) e interfaz
(13 pruebas de UI Automation, 2 nuevas en `FillPatternUiTests`). Las preferencias de las pruebas viven
siempre en directorios temporales (`MEMRESCUE_SETTINGS_DIR`); nunca se toca el perfil del usuario.

Metadatos de asignación ilegibles (issue #39): `FaultyDevice::with_allocation_error` simula un
dispositivo cuya consulta `allocation` falla en un tramo. Las pruebas de mr-io comprueban sector a
sector (`assert_honest`) que con `FillAndContinue` el bloque afectado se lee como `Data` y, si la
lectura falla, queda `Unreadable` con relleno (nunca `Sparse`), que el resto del mapa es exacto, que
dentro de un salto queda `Skipped` sin leerse, que reintentos y omisión por código se aplican a esa
lectura y que con `Fail` el error sigue abortando. En mr-image, un VHD dinámico sintético se trunca en
mitad del mapa de bits de un bloque después de abrirlo: `allocation` informa ese bloque como `Data`, la
verificación termina con el bloque `Unreadable` y los demás estados correctos, y la lectura estricta
aborta con la posición exacta. VHDX y VMDK no necesitan prueba equivalente: su `allocation` no lee del
fichero (mapas cargados al abrir).

Endurecimiento de la capa de bloques (issue #40): `core/mr-image/tests/corruption.rs` (8 pruebas, ≈20 s
en perfil de depuración) con contenedores sintéticos construidos en la propia prueba según las
especificaciones públicas: **inversión aleatoria determinista** de 1 a 4 bytes (semillas fijas; 300
iteraciones en VHDX, la mitad con las CRC-32C de cabeceras y tablas de regiones recalculadas; 400 en VMDK;
200 en una imagen dividida), **truncado en cada frontera de estructura** a ±1 byte (VHDX: identificador,
cabeceras, tablas de regiones, registro, tabla y elementos de metadatos, BAT, bloques; VMDK: cabecera,
descriptor, GD, GT, granos) y **fichero acortado después de abrir** (VHDX, VMDK y dividida). Se comprueba
que abrir devuelve `Ok` o un error tipado sin pánicos (un vigilante de tiempo convierte un bloqueo en
fallo); que lo que el modo estricto abre se lee entero y que estricto y tolerante leen igual cuando el
estricto abre; que el lector resiliente marca `Unreadable` exactamente los sectores cuya lectura directa
falla y nunca entrega datos de lo rechazado; que lo truncado o acortado se lee como el original o da
error (nunca otro contenido); y que el fichero de origen no cambia. Además: `ReadPolicy.retries` se acota
a `resilient::MAX_RETRIES` (100) dentro del lector (`retries_are_capped_at_max_retries`), apertura de una
imagen dividida con `MAX_SEGMENTS` segmentos y un repetido al final
(`open_with_max_segments_and_late_duplicate_is_fast`, comprobación de repetidos con `HashSet`), consulta
O(1) de 10 000 tramos no fiables por la FFI (`untrusted_ranges_are_indexed_in_constant_time`) y el perfil
Release compila con `overflow-checks = true` (`cargo test --release --workspace` pasa con ellas).

Memoria acotada al abrir contenedores (issue #38, addendum de ADR-020): `core/mr-ffi/tests/table_memory.rs`
(4 pruebas, no ignoradas) genera en un directorio temporal imágenes sintéticas con la tabla al límite de
`MAX_TABLE_BYTES` y entradas duplicadas (VHD con BAT de 256 MiB y bloques de 512 B; VHDX con BAT de
256 MiB y bloques de 1 MiB; VMDK con GD de 256 MiB y todas las entradas en la misma GT; VMDK con
131 072 GT y todos los granos en el mismo sector), las abre en modo estricto (debe dar `Corrupt`) y
tolerante (debe descartar todos los bloques o granos) y mide el pico de memoria con un asignador global
que cuenta los bytes vivos; el pico debe quedar dentro de `mr_image::open_memory_budget` más 4 MiB
para reservas que no son tablas. Está en `mr-ffi` porque es el único crate con `unsafe` permitido
(`GlobalAlloc`); las medidas se serializan con un mutex. Cada imagen ocupa unos 260 MiB en disco y se
borra al terminar; el pico de memoria del proceso de pruebas es de unos 530 MiB. Duración: unos 75 s en
Debug y unos 8 s en Release (`cargo test --release -p mr-ffi --test table_memory -- --nocapture`
muestra los picos). Además, pruebas unitarias en `mr-image` del recorrido en flujo de la detección de
solapes (un millón de zonas idénticas, parada en el primer error), del registro de daños como mapa de
bits, del fallo inmediato en modo estricto y del contador de presupuesto (límite, liberación y fallo
de asignación informado como error).

**Integración continua** (`.github/workflows/ci.yml`, detalle en `CONTRIBUTING.md`): las mismas
comprobaciones se ejecutan en GitHub Actions en cada rama `feat/` o `fix/` y en cada PR, más la auditoría
de higiene del repositorio (`tools/check_repo_hygiene.py`: rutas personales, correos, credenciales,
material excluido, carácter U+2014 y binarios reproducibles), que también es un paso de `test.ps1`. Las
pruebas de interfaz se lanzan a demanda con el workflow `UI tests`.

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
