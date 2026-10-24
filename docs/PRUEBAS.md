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
  - NTFS: `mkntfs` y `ntfs-3g` en un ejecutor Ubuntu desechable de GitHub Actions, con The Sleuth Kit
    como oráculo (§1.2, ADR-024).
  - Linux (WSL o VM): `sfdisk`, `mkfs.fat`, `mkfs.exfat`, `mkfs.ext2/3/4`, `mkfs.btrfs`, `mkfs.hfsplus`,
    `mdadm` (RAID), `qemu-img` (VMDK/VHD), `xorriso` (ISO).
  - APFS y E01: ver INV-10 e INV-11.
- Las imágenes del sistema de pruebas se crean y destruyen en carpetas temporales; nunca se montan
  sobre letras del sistema en la máquina del desarrollador sin VM.

### 1.1 Esquema del manifiesto de particiones (versión 2)

Lo genera `tests/generators/gen_partition_images.py` y lo leen el validador independiente, las pruebas
cruzadas de Rust y C# y, en adelante, los analizadores de `mr-part` (#12, #13) y la búsqueda de
particiones perdidas (#15). Es TOML; las claves de nivel superior van antes de cualquier tabla.

**Coordenadas.** Las LBA de `[[partitions]]`, las claves de la GPT (`first_usable_lba`, `entries_lba`,
`backup_header_lba`...) y `hidden_sectors` son las **declaradas** (disco original). Todas las demás
(`*_image_lba`, `marker_lba`, `[[anomalies]].lba`, `[[found_volumes]]`, `unreadable_lbas`, `decoy_lbas`)
son de la **imagen**: `LBA imagen = LBA disco - leading_sectors_missing`.

| Clave | Significado |
|---|---|
| `schema_version`, `generator_version` | Versión del esquema (2) y del generador (3 desde #17: añade las imágenes APM sin cambiar ningún byte de las anteriores) |
| `name`, `file`, `scenario` | Identificador, fichero `.img` y descripción del escenario |
| `scheme` | Lo que hay en LBA 0 y la GPT: `mbr`, `gpt` (incluye MBR protector), `hybrid`, `apm` (Apple Partition Map) o `none` |
| `sector_size`, `total_sectors`, `size_bytes`, `sha256` | Geometría de la imagen y huella determinista |
| `leading_sectors_missing` | Sectores del disco que faltan al inicio (negativo: sectores añadidos) |
| `trailing_sectors_missing` | Sectores del disco que faltan al final |
| `disk_signature`, `disk_guid` | Firma de disco del MBR (`mbr`, `hybrid`) y GUID de disco GPT |
| `extended_start_lba`, `extended_sectors`, `extended_type`, `ebr_lbas` | Extendida y cadena de EBR recorrida (cada EBR una vez) |
| `first_usable_lba` ... `entries_crc32` | Disposición GPT sana (CRC-32 correcta de la tabla de entradas) |
| `gpt_primary_header`, `gpt_backup_header` | `valid`, `crc` (firma presente, CRC-32 o tamaño incorrectos), `missing` (sin firma) u `outside_image` |
| `gpt_primary_header_image_lba`, `gpt_backup_header_image_lba` | Dónde está la cabecera en la imagen (solo si tiene firma) |
| `gpt_primary_entries`, `gpt_backup_entries` | `valid`, `crc`, `outside_image` o `unchecked` (cabecera no válida) |
| `gpt_selected` | Copia que debe usarse según UEFI: `primary`, `backup` o `none` |
| `unreadable_lbas` | Sectores que la prueba debe simular ilegibles (`FaultyDevice`); contienen un relleno que imita una firma |
| `decoy_lbas` | Sectores con aspecto de tabla dentro de datos, que no deben interpretarse |
| `apm_block_size`, `apm_disk_blocks`, `apm_map_entries` | APM: tamaño de bloque del mapa (en el que están las LBA de `[[partitions]]`), bloques del disco y `pmMapBlkCnt` |

Tablas:

- `[[partitions]]`: particiones que debe devolver el analizador de la tabla (MBR: primarias y luego
  lógicas en orden de cadena; GPT/híbrido: entradas de la copia elegida). Claves `label`, `start_lba`,
  `sectors`, `end_lba`; MBR: `mbr_type`, `active`, `logical`; GPT: `gpt_type`, `gpt_name`,
  `unique_guid`; APM (entradas con firma `PM` en orden de mapa, también `Apple_partition_map` y
  `Apple_Free`): `apm_index` (número de entrada desde 1), `apm_type`, `apm_name`, `apm_status`
  (`pmPartStatus`); `marker_lba` (si la imagen está desplazada) y `marker` (texto en el primer sector;
  vacío si no se escribe: la entrada del propio mapa APM, cuyo primer bloque es la entrada 1, y las
  entradas que empiezan fuera del disco).
- `[[hybrid_mbr_entries]]`: entradas del MBR híbrido (`index`, `mbr_type`, `active`, `start_lba`,
  `sectors`, `gpt_label` de la partición GPT que reflejan, vacío para 0xEE).
- `[[anomalies]]`: conjunto **exacto** de anomalías (`code`, `lba`; opcionales `target_lba`, `value`,
  `partitions`). Códigos: `mbr_missing` (LBA 0 sin tabla reconocible), `mbr_empty` (firma sin entradas),
  `mbr_signature_missing`, `mbr_multiple_active`, `mbr_overlap`, `mbr_unknown_type` (`value` = tipo),
  `mbr_outside_disk`, `ebr_loop`, `ebr_link_outside_disk`, `ebr_link_outside_extended` (`lba` = EBR con
  el enlace, `target_lba` = destino), `hybrid_mbr`, `protective_mbr_size_mismatch` (`value` = tamaño
  declarado), `gpt_{primary,backup}_header_{crc,missing,outside_image}`,
  `gpt_{primary,backup}_entries_{crc,outside_image}`, `gpt_usable_range_outside_image` (`lba` =
  cabecera elegida), `image_shifted` (`lba` = posición en la imagen de la LBA 0 del disco, `value` =
  `leading_sectors_missing`), `mbr_displaced` (MBR encontrado en `lba`), `superfloppy` (volumen en LBA 0)
  y, en APM (`lba` = sector de la entrada, `value` = número de entrada), `apm_entry_signature` (entrada
  sin firma `PM`, no se lista), `apm_entry_outside_disk` y `apm_entry_partially_outside_disk`. Una
  imagen APM lleva también `mbr_missing`: su bloque 0 es el Driver Descriptor Map.
- `[[found_volumes]]`: volúmenes que debe encontrar la búsqueda de particiones perdidas: `fs` (`ntfs`,
  `fat32`, `exfat`), `start_lba`, `sectors`, `end_lba`, `hidden_sectors` (inicio declarado por el FS),
  `boot_lba`, `backup_boot_lba`, `primary_boot` / `backup_boot` (`valid`, `damaged`, `unreadable`,
  `outside_image`), `evidence` (copias válidas: `boot`, `backup_boot`), `partial` (empieza antes de la
  imagen), `serial` y `suggested_mbr_type`.

### 1.2 Corpus NTFS (issue #19, ADR-024) y su manifiesto semántico (versión 1)

Las imágenes NTFS **no se generan en local**: las crea el workflow `NTFS corpus`
(`.github/workflows/ntfs-corpus.yml`) en un ejecutor Ubuntu desechable con `mkntfs` y `ntfs-3g`, y la
verdad de cada manifiesto la obtiene **The Sleuth Kit** (`build_manifests.py`). Los manifiestos están
versionados en `tests/corpus/ntfs/manifests/*.manifest.toml`; las imágenes se publican como artefacto
`ntfs-corpus` (90 días) y se regeneran byte a byte (`docs/PLAN_CORPUS_NTFS.md` §4.2).

```powershell
python -I -X utf8 tools\fetch_ntfs_corpus.py                      # última ejecución con éxito (main o, si no hay, cualquier rama)
python -I -X utf8 tools\fetch_ntfs_corpus.py --run-id 123456789   # o una ejecución concreta / --branch <rama>
python -I -X utf8 tests\generators\validate_ntfs_images.py tests\corpus\ntfs\generated tests\corpus\ntfs\manifests
python -I -X utf8 tests\generators\test_ntfs_validator.py tests\corpus\ntfs\generated tests\corpus\ntfs\manifests
```

`fetch_ntfs_corpus.py` necesita `gh` autenticado, descarga en un directorio temporal nuevo, exige que
cada imagen tenga el tamaño y el SHA-256 de su manifiesto versionado y solo entonces la copia a
`tests/corpus/ntfs/generated/` (ignorado por git). Si el artefacto ha caducado, se relanza el workflow
(`gh workflow run ntfs-corpus.yml --ref <rama>`): como la generación es byte a byte, los SHA-256 no
cambian.

**Uso por las pruebas de M3 (#20 en adelante).** Las pruebas de corpus NTFS de Rust son
`#[ignore]` y leen imágenes y manifiestos de esas rutas (variable `MR_NTFS_CORPUS` para otra
ubicación de las imágenes). En la CI, el job `ntfs` de `ci.yml` (el único con `actions: read`, además de
`contents: read`, y `GH_TOKEN` = el token de Actions) descarga el artefacto con
`tools/fetch_ntfs_corpus.py` (última ejecución con éxito de `main` o, si no hay, de cualquier rama;
el SHA-256 de cada imagen debe coincidir con su manifiesto versionado) y ejecuta
`cargo test -p mr-fs-ntfs --test ntfs_corpus -- --ignored`,
`cargo test -p memrescue-cli --test ntfs_corpus -- --ignored` y, para los borrados (#23),
`cargo test -p mr-fs-ntfs --test ntfs_deleted_corpus -- --ignored` y
`cargo test -p memrescue-cli --test ntfs_deleted -- --ignored`. En local, `scripts/test.ps1` tiene el
mismo paso (`-SkipNtfsCorpus` para omitirlo; sin corpus, el paso falla e indica cómo descargarlo).

**Validación independiente.** Tres comprobaciones que no comparten código entre sí:
`build_manifests.py` (TSK; además exige que lo que ve TSK coincida con la intención del generador: rutas,
inodos de ntfs-3g, tamaños, hashes y marcas de tiempo fijadas), `verify_tsk.py` (otra vez TSK, por otras
vías: `ifind -n`, `ffind`, `fls -u`, `istat`, `icat`, `mmls`) y `validate_ntfs_images.py` (Python puro,
sin TSK ni generador: tablas MBR/GPT, arranque y su copia, `$MFT`/`$MFTMirr`, cada registro MFT con su
secuencia de actualización y los runs decodificados de los mapping pairs, y el SHA-256 de cada flujo
leído de la imagen siguiendo esos runs). `test_ntfs_validator.py` aplica 20 alteraciones (imagen o
manifiesto, con el SHA-256 recalculado) y exige que el validador detecte cada una.

**Esquema.** Claves de nivel superior: `schema_version` (1), `name`, `file`, `scenario`, `generator`,
`tools` (paquetes de Ubuntu con versión), `fake_time` (hora congelada de las marcas de tiempo de los
ficheros de usuario), `image_size`, `image_sha256`, `byte_reproducible` (dos generaciones de la misma
ejecución con el mismo SHA-256), `base` (escenario del que deriva), `entries_live`, `entries_deleted`,
`entries_overwritten`.

| Tabla | Claves |
|---|---|
| `[partition]` (solo `ntfs_mbr`, `ntfs_gpt`) | `scheme`, `index`, `start_lba`, `sectors` (de `mmls`), `tsk_table`, `tsk_description`; MBR: `disk_signature`, `mbr_type`, `active`; GPT: `disk_guid`, `gpt_type`, `unique_guid`, `gpt_name` |
| `[volume]` (de `fsstat` e `istat` del registro 0) | `offset` (byte del volumen en la imagen), `serial`, `label`, `version`, `mft_lcn`, `mftmirr_lcn`, `mft_record_size`, `index_record_size`, `mft_entries`, `sector_size`, `cluster_size`, `total_sectors` (campo del arranque), `mft_runs` (`[vcn, lcn, clusters]`), `backup_boot_offset` (byte de la copia del arranque) |
| `[[damage]]` (solo `ntfs_damaged_*`) | `kind` (`boot`, `mft`, `mftmirr`), `regions` (`[byte, longitud]` sobrescritos), `description`, `expected` (comportamiento esperado de un lector tolerante), `tsk_opens`; `mft`: `records` y `tsk_unreadable_records` |
| `[[files]]` (entradas de usuario, ordenadas por ruta) | `path` (desde la raíz del volumen, `/` como separador, UTF-8), `type` (`file`, `dir`), `mft_record`, `sequence` (la actual del registro; ntfs-3g la incrementa al borrar), `state` (`live`, `deleted`, `overwritten`); ficheros: `size`, `sha256` (contenido original), `resident`, `runs` (`[vcn, lcn, clusters]`, contiguos fusionados), `sparse` (`[vcn, clusters]`), `init_size` (si difiere de `size`), `recoverable` (`full`, `partial`, `none`); sobrescritos: `recovered_sha256` (lo que devuelve `icat`), `clusters`, `overwritten_clusters`, `reason`; dañados: `mft_damaged` |
| `[[files.ads]]` | `name`, `size`, `sha256`, `resident`, `runs`, `sparse`, `init_size` del flujo alternativo |

`state = "overwritten"` se decide comparando cluster a cluster lo que devuelve `icat` con el contenido
original; `reason` nombra los ficheros que ocupan ahora esos clusters según los runs de TSK. En
`ntfs_damaged_boot`, TSK no abre la imagen y la verdad de `[[files]]` sale de TSK sobre una copia
temporal con el arranque restaurado desde `backup_boot_offset`; en `ntfs_damaged_mft` las entradas con
`mft_damaged = true` conservan los datos de `ntfs_basic` (su registro ya no es legible).

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
powershell -ExecutionPolicy Bypass -File scripts\test.ps1                  # 16 pasos (-SkipGui, -SkipQemu, -SkipNtfsCorpus)
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
| 6 | Pruebas negativas | El validador detecta las corrupciones (no es un validador que siempre aprueba); desde #14, 44 de imagen y 10 de manifiesto sin apoyarse en el SHA-256 |
| 7 | Pruebas cruzadas Rust ↔ corpus | SHA-256 y CRC-32 del motor coinciden con `hashlib`/`zlib` de Python |
| 8 | `dotnet build` | Compilación .NET con advertencias como errores |
| 9 | `dotnet test` | 47 pruebas de la frontera C# ↔ Rust contra la DLL real (incluye corpus) |
| 10–11 | Autodiagnóstico GUI (512 y 4096 B/sector) | `MemRescue.exe` abre la imagen, lee la LBA 1 (`EFI PART`), sectores y SHA-256 = manifiesto, verificación sin sectores ilegibles |
| 12 | Orígenes intactos | SHA-256, tamaño y fecha de las imágenes iguales antes y después de todas las operaciones |

(Desde M1 el paso 3 ejecuta 77 pruebas Rust y el 7 incluye la lectura de marcadores en LBA exactas.)

Desde #20 (16 pasos): un paso nuevo, **«Rust: lector NTFS y ntfs-info contra el corpus NTFS»**, ejecuta
las pruebas ignoradas de `mr-fs-ntfs` y de la CLI contra el corpus NTFS descargado
(`tools/fetch_ntfs_corpus.py`); `-SkipNtfsCorpus` lo omite. El paso final «Orígenes intactos» incluye
también las imágenes NTFS si están descargadas.

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

Registro pendiente de VHDX (issue #10, `formatos/vhdx.md`): 10 pruebas unitarias en
`core/mr-image/src/vhdx/tests/registro.rs` con entradas construidas según [MS-VHDX] y un modelo
independiente (las escrituras aplicadas a mano sobre una copia sin registro deben leerse igual que la
imagen con el registro pendiente): datos y ceros, BAT y metadatos, varias entradas con vuelta al
principio del registro, secuencia más reciente incompleta y entrada a medio escribir ignoradas, Tail,
saltos de secuencia, LastFileOffset y 18 registros no válidos (`Corrupt` en ambos modos), con el
fichero intacto. En `corruption.rs`, 2 pruebas más: inversiones dirigidas al registro (con las CRC-32C
de sus entradas recalculadas la mitad de las veces) y fichero acortado después de abrir. Contra
qemu-img, `core/mr-image/tests/vhdx_log_qemu.rs` (2 ignoradas, las ejecuta el paso «Formatos»): qemu
reproduce el registro en una **copia desechable** abierta en lectura y escritura
(`convert --image-opts …,read-only=off`) y la lectura de MemRescue del original debe tener el mismo
SHA-256; `qemu-img info` del original debe negarse a abrirlo por tener un registro pendiente. Casos:
registros escritos por qemu vueltos a dejar pendientes (también con sus destinos puestos a ceros) y
registros sintéticos con vuelta al principio y entrada a medias. El original no cambia.

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

Analizador GPT (issue #13, `formatos/gpt.md`): 21 pruebas unitarias en `core/mr-part/src/gpt.rs` con
discos GPT sintéticos en memoria (una o más por regla: CRC-32 de cabecera y entradas, tamaño de
cabecera, revisión, `MyLBA`/`AlternateLBA`, rango utilizable, ubicación de la tabla, límites de
entradas, solapes, GUID repetidos, nombres, copias en desacuerdo, 4Kn y el otro tamaño de sector,
desplazamiento y truncado, `FaultyDevice`) y un fuzzing determinista de 3000 iteraciones sin pánicos
ni bloqueos y con memoria acotada. `core/mr-part/tests/gpt_corpus.rs` (4 pruebas ignoradas por defecto;
`cargo test -p mr-part --test gpt_corpus -- --ignored` con el corpus generado) compara `read_gpt`
con los 13 manifiestos que tienen estado GPT (estados de cada copia, copia elegida, desplazamiento,
sectores finales ausentes, particiones con su marcador y el conjunto exacto de códigos GPT; los de
MBR/EBR se ignoran), comprueba que las imágenes sin GPT no producen ninguna (las de
`unreadable_lbas` envueltas en `FaultyDevice`), abre `gpt_4kn` con 512 B y `gpt_basic` con 4096 B y
simula sectores GPT ilegibles sobre el corpus.

**Integración continua** (`.github/workflows/ci.yml`, detalle en `CONTRIBUTING.md`): las mismas
comprobaciones se ejecutan en GitHub Actions en cada rama `feat/` o `fix/` y en cada PR, más la auditoría
de higiene del repositorio (`tools/check_repo_hygiene.py`: rutas personales, correos, credenciales,
material excluido, carácter U+2014 y binarios reproducibles), que también es un paso de `test.ps1`. Las
pruebas de interfaz se lanzan a demanda con el workflow `UI tests`.

Corpus actual (`tests/corpus/manifests`, imágenes en `tests/corpus/generated`, no versionadas; 41
imágenes, unos 213 MiB, generadas en menos de 1 s). Esquema del manifiesto en §1.1. Todo es sintético y
se construye solo a partir de especificaciones públicas (UEFI, MBR/EBR clásico, Apple «Inside Macintosh:
Devices» para el Apple Partition Map, Microsoft FAT32 y exFAT, estructura pública del arranque NTFS).

| Imagen | Tamaño | Contenido | Resultado esperado |
|---|---|---|---|
| `mbr_basic` | 16 MiB | MBR con 2 primarias (una activa) + extendida 0x0F con cadena de 2 EBR y 2 lógicas | 4 particiones |
| `gpt_basic` | 16 MiB | MBR protector, GPT primaria + copia, 3 particiones (datos, Linux, EFI) | 3 particiones, primaria |
| `gpt_4kn` | 8 MiB | GPT con 1 partición, 4096 B/sector | 1 partición |
| `mbr_no_signature` | 2 MiB | Entradas válidas sin 0x55AA | 2 particiones, `mbr_signature_missing` |
| `mbr_two_active` | 2 MiB | Dos entradas activas | 2 particiones, `mbr_multiple_active` |
| `mbr_ext_0x85` | 2 MiB | Extendida de tipo 0x85 (Linux) con 2 EBR | 3 particiones, sin anomalías |
| `mbr_ebr_loop` | 2 MiB | El enlace del segundo EBR vuelve al primero | 3 particiones, `ebr_loop` |
| `mbr_ebr_link_outside` | 2 MiB | Enlace de EBR más allá del final del disco | 2 particiones, `ebr_link_outside_disk` |
| `mbr_overlap` | 2 MiB | Dos primarias solapadas | `mbr_overlap` |
| `mbr_unknown_type` | 2 MiB | Entrada de tipo 0x7F | `mbr_unknown_type` |
| `mbr_outside_disk` | 2 MiB | Entrada que acaba fuera del disco | `mbr_outside_disk` |
| `empty_disk` | 1 MiB | Todo a cero | Sin tabla (`mbr_missing`) |
| `random_disk` | 1 MiB | Bytes pseudoaleatorios deterministas | Sin tabla ni volúmenes |
| `protective_mbr_only` | 1 MiB | MBR protector sin cabeceras GPT | `gpt_selected = none` |
| `hybrid_mbr_gpt` | 2 MiB | GPT sana + MBR híbrido (0xEE y 2 espejos) | Particiones GPT, `hybrid_mbr` |
| `fake_mbr_in_data` | 2 MiB | MBR sano; un sector con aspecto de MBR dentro de la partición | 1 partición; el señuelo no cuenta |
| `gpt_primary_header_bad` | 2 MiB | CRC-32 de la cabecera primaria incorrecta | Respaldo |
| `gpt_entries_crc_bad` | 2 MiB | Tabla de entradas primaria alterada | Respaldo |
| `gpt_backup_header_bad` | 2 MiB | CRC-32 de la cabecera de respaldo incorrecta | Primaria |
| `gpt_backup_missing` | 2 MiB | Cabecera y entradas de respaldo borradas | Primaria |
| `gpt_both_bad` | 2 MiB | Primaria con CRC incorrecta y respaldo borrado | Ninguna |
| `gpt_4kn_primary_bad` | 2 MiB | 4Kn con la cabecera primaria dañada | Respaldo |
| `gpt_shifted_start` | 2 MiB | Faltan los 63 primeros sectores (MBR, cabecera y entradas primarias) | Respaldo, desplazamiento 63 |
| `gpt_shifted_start_1` | 2 MiB | Falta el primer sector (cabecera primaria en LBA 0) | Primaria, desplazamiento 1 |
| `gpt_truncated_end` | 2 MiB | Faltan los 200 últimos sectores (respaldo incluido) | Primaria, 200 sectores finales ausentes |
| `lost_ntfs_mbr_zeroed` | 2 MiB | MBR a cero; NTFS en 2048 con copia en su último sector | 1 volumen, arranque + copia |
| `lost_fat32_chs` | 33 MiB | Entradas a cero con firma intacta; FAT32 en LBA 63 con copia en el sector 6 | 1 volumen (`mbr_empty`) |
| `lost_exfat_1mib` | 2 MiB | MBR a cero; exFAT en 1 MiB con regiones principal y de respaldo (suma del sector 11) | 1 volumen |
| `lost_chain_three` | 36 MiB | NTFS, FAT32 y exFAT consecutivos; el exFAT empieza en una LBA no alineada | 3 volúmenes |
| `ntfs_primary_damaged` | 2 MiB | Arranque NTFS principal a cero | 1 volumen, solo copia |
| `fat32_primary_damaged` | 34 MiB | Arranque FAT32 principal dañado | 1 volumen, solo copia del sector 6 |
| `exfat_primary_damaged` | 2 MiB | Sector de arranque exFAT principal a cero | 1 volumen, solo región de respaldo |
| `primary_unreadable` | 2 MiB | Arranque NTFS principal en `unreadable_lbas`, con relleno que imita «NTFS» y 0x55AA | 1 volumen, solo copia |
| `backup_unreadable` | 2 MiB | Copia NTFS en `unreadable_lbas`, con el mismo relleno | 1 volumen, solo arranque |
| `image_shifted_pos` | 3 MiB | 8 sectores añadidos al inicio; MBR en LBA 8, NTFS y exFAT | 2 volúmenes, desplazamiento -8, `mbr_displaced` |
| `image_shifted_neg` | 2 MiB | Faltan 2100 sectores: MBR y arranque NTFS perdidos | NTFS parcial (solo copia) y exFAT, desplazamiento 2100 |
| `gpt_4kn_lost` | 4 MiB | 4Kn sin GPT ni MBR protector; NTFS y exFAT de 4096 B/sector | 2 volúmenes |
| `superfloppy` | 1 MiB | NTFS en LBA 0, sin tabla | 1 volumen en LBA 0 (`superfloppy`) |
| `apm_basic` | 2 MiB | APM sano: DDM de 512 B y mapa de 4 entradas (`Apple_partition_map`, 2 `Apple_HFS`, `Apple_Free`) | 4 particiones APM, solo `mbr_missing` |
| `apm_entry_damaged` | 2 MiB | Como `apm_basic` con la entrada 3 sin la firma `PM` | 3 particiones, `apm_entry_signature` |
| `apm_entry_outside` | 2 MiB | Entrada 3 que termina fuera del disco y entrada 4 que empieza fuera | 4 particiones, `apm_entry_partially_outside_disk` y `apm_entry_outside_disk` |

Los nombres de los escenarios de volúmenes perdidos siguen INV-05b §9. Con 512 B/sector, un FAT32 válido
según la especificación necesita al menos 65 525 clústeres, por eso las tres imágenes con FAT32 ocupan
unos 33 MiB y las demás 1-4 MiB.

El validador (`validate_partition_images.py`) no importa el generador: deduce desde los bytes el esquema,
las particiones, el conjunto exacto de anomalías, el estado de cada copia GPT y la copia elegida, el
desplazamiento de la imagen (por la cabecera GPT de respaldo o por los sectores ocultos de los volúmenes)
y los volúmenes, recorriendo **todos** los sectores en busca de arranques NTFS, FAT32 y exFAT coherentes
(campos del BPB, número mínimo de clústeres de FAT32, FSInfo, sectores extendidos y suma de comprobación
de exFAT) y agrupando cada arranque con su copia; en APM interpreta por su cuenta el Driver Descriptor
Map y cada entrada big-endian. Las pruebas negativas (`test_validator.py`) aplican 54
corrupciones de imagen y 14 alteraciones de la verdad esperada del manifiesto, **recalculando el SHA-256**
de la copia del manifiesto para que la detección no dependa del hash, más 3 controles sin cambios que
deben validar.

Limitaciones conocidas: el validador lo ha escrito el mismo equipo que el generador (aunque son códigos
separados). Falta un oráculo de terceros (`sgdisk`/`sfdisk`), que requiere WSL o una VM (ver
[PLAN_CORPUS_NTFS.md](PLAN_CORPUS_NTFS.md) §4). Los volúmenes son sintéticos: solo contienen las
estructuras que consulta la búsqueda (arranque, copia, FSInfo, región de arranque exFAT, cabecera de las
FAT y etiqueta de FAT32), sin MFT ni árbol de directorios. Los sectores ilegibles no se pueden
representar en un fichero RAW: la prueba debe envolver la imagen con `FaultyDevice` en `unreadable_lbas`.
Los casos de presupuesto, cancelación, escala (1 GiB y 2 TiB) y «todo relleno de ilegible», que dependen
del motor y no del corpus, los cubre #15 (ver más abajo).

Tablas MBR/EBR (issue #12, ADR-022):

- `core/mr-part/src/mbr/tests.rs` (23 pruebas, discos construidos en memoria; una por regla y una de fuzzing): primarias sanas;
  sector vacío o con firma sin entradas; firma ausente (se lee, sospechosa); sector sin firma no
  plausible (indicador no válido, ninguna entrada dentro, dos extendidas); indicador no válido con firma
  (inválida) y en entrada libre (aviso); varias activas; fuera y parcialmente fuera del disco; solapes
  (una anomalía por partición, salida acotada); tipo desconocido y tamaño 0; cadena de EBR con ambos
  modos de direccionamiento y tipos 0x05/0x0F/0x85; bucle (también un EBR que se enlaza a sí mismo);
  enlace fuera del disco y fuera de la extendida; lógica fuera de la extendida; EBR con dos enlaces, sin
  firma o a cero; límite de 1024 EBR; MBR protector (inicio y tamaño) e híbrido; arranque NTFS/FAT32 en
  la LBA 0 (disco sin particionar) y cabecera GPT en la LBA 0; sectores ilegibles (`FaultyDevice`) en la
  LBA 0 y en un EBR; tabla desplazada (`read_mbr_at`); sector de 4096 B; y un fuzzing acotado con
  semilla fija (3000 discos con tablas aleatorias: sin pánicos, sin bucles, salida acotada, < 60 s).
- `cli/tests/corpus/partitions.rs` (6 pruebas, `--ignored`, dentro de `cli/tests/corpus.rs`): para los 41
  manifiestos, el esquema visto desde el MBR, las particiones (inicio, tamaño, tipo, activa, lógica), la
  extendida, los EBR recorridos, el conjunto **exacto** de anomalías MBR (código, LBA, destino y valor) y
  las particiones que señala cada una (29 anomalías y 25 particiones; en APM el MBR no ve tabla); señuelos no interpretados;
  `unreadable_lbas` con `FaultyDevice` (resultado idéntico), LBA 0 ilegible en todas las imágenes y
  último EBR ilegible; imágenes desplazadas con las coordenadas del manifiesto (cabecera GPT en la LBA 0,
  MBR desplazado leído en `mbr_displaced` con particiones que coinciden con `found_volumes` y empiezan por
  su sector de arranque); 300 mutaciones deterministas por imagen de las tablas del corpus (bytes de las
  entradas y enlaces que apuntan a otras tablas: sin pánicos, < 5 s por lectura, salida acotada y cada
  partición que empieza dentro se abre acotada al disco); y `partitions --json` de la CLI frente al
  manifiesto. Los códigos `gpt_*` son de #13 y `image_shifted` / `mbr_displaced`, de #15; aquí se
  ignoran como anomalías (los desplazamientos se comprueban aparte).
- Integración con GPT (ADR-022, adenda): `core/mr-part/src/table.rs` (6 pruebas con discos GPT
  sintéticos: MBR protector con particiones GPT, híbrido con `hybrid_mbr_mismatch`, MBR clásico que
  ignora una GPT obsoleta, LBA 0 en blanco con GPT, imagen desplazada que solo se abre pidiéndolo y
  partición anterior al inicio de la imagen, nombres de tipo y códigos GPT) y en
  `cli/tests/corpus/partitions.rs` `partition_table_matches_every_manifest` (los 41 manifiestos por
  `read_partition_table`: esquema, particiones MBR y después GPT con tipo, nombre y GUID único, copia en
  uso, desplazamiento y el conjunto **exacto** de anomalías con código en el manifiesto, MBR y GPT
  unidas, 47 anomalías con las APM; las 21 particiones GPT se abren, en las imágenes desplazadas solo con
  `apply_shift`, y empiezan por su marcador) y `cli_json_for_gpt_basic_and_damaged_primary`
  (`partitions --json` de `gpt_basic`, `gpt_primary_header_bad` y `gpt_shifted_start`, y `--partition`
  con y sin `--apply-shift`). Códigos sin equivalente en el manifiesto (`gpt_backup_in_use`,
  `gpt_image_truncated`, `gpt_copies_disagree`...) no se comparan, como en `formatos/gpt.md`.
- `mr-api` (`partitions_are_listed_and_open_as_sources`), `mr-ffi` (6 pruebas), CLI
  (`cli/tests/cli.rs`: `partitions` en texto y JSON, `--partition N` con `sectors`, `verify` e
  `image-info`, errores de uso), C# (`PartitionTests`: disco sintético, ciclo de vida y el corpus
  completo por la DLL, incluida la lectura del marcador de cada partición abierta) e interfaz
  (`PartitionUiTests`, 3 pruebas, una con `gpt_basic`). La prueba C# del corpus compara la tabla
  completa (MBR y GPT) y abre cada partición GPT comprobando su marcador.

Apple Partition Map (issue #17, `formatos/apm.md`):

- `core/mr-part/src/apm/tests.rs` (13 pruebas, mapas construidos en memoria): mapa sano con sus 4
  entradas (origen, tipo, nombre, estado, área de datos), integración en `read_partition_table` y
  apertura; bloques de 2048 B en sectores de 512 B (LBA en bloques del mapa y apertura rechazada salvo
  reabriendo con 2048 B) y bloques de 512 B en sectores de 4096 B; entrada sin firma; entradas fuera,
  parcialmente fuera, de tamaño 0, solapadas y con el área de datos fuera; `pmMapBlkCnt` 0, enorme y
  mayor que el origen (límite de 512 entradas); DDM ausente, con tamaño no admitido, con tamaño distinto
  del mapa y con un número de bloques menor o mayor que el origen; DDM sin mapa y «ER» sin APM; MBR que
  manda; sectores ilegibles (`FaultyDevice`) en el DDM y en las entradas; textos y nombres de tipo; y un
  fuzzing acotado con semilla fija (3000 mapas mutados con 4 tamaños de sector: sin pánicos, salida
  acotada, < 60 s).
- `core/mr-part/tests/apm_corpus.rs` (3 pruebas, `--ignored`): las 3 imágenes APM por
  `read_partition_table` (mapa, 11 particiones con entrada, tipo, nombre, estado y confianza, conjunto
  exacto de anomalías), ninguna otra imagen del corpus se toma por APM, cada sector del DDM y del mapa
  ilegible en `apm_basic` y `apm_basic` leído con sectores de 4096 B.
- `cli/tests/corpus/partitions.rs` (`partition_table_matches_every_manifest` y
  `cli_partitions_json_follows_the_engine`: particiones APM, apertura y marcador, `partitions --json`),
  `mr-ffi` (`apm_partitions_are_enumerated_and_open_as_sources`: `MR_SCHEME_APM`, `MR_ORIGIN_APM`,
  códigos 100 y 408, `pmPartStatus` en `attributes`, `mr_partition_apm_type`) y C#
  (`CorpusTableMatchesManifest` con los 3 manifiestos APM: esquema, entradas, tipo, nombre, estado,
  apertura y marcador).

Búsqueda exprés de particiones perdidas (issue #15, ADR-023, `formatos/busqueda-particiones.md`),
pruebas por fila del plan de INV-05b §9:

| Fila de INV-05b §9 | Pruebas |
|---|---|
| Volúmenes perdidos (`lost_ntfs_mbr_zeroed`, `lost_fat32_chs`, `lost_exfat_1mib`, `lost_chain_three`) | `core/mr-part/tests/search_corpus.rs` (`--ignored`): `found_volumes_match_every_manifest` compara los `[[found_volumes]]` de los **41** manifiestos (18 volúmenes: `fs`, `start_lba`, `sectors`, `end_lba`, `hidden_sectors`, `boot_lba`, `backup_boot_lba`, `primary_boot`, `backup_boot`, `evidence`, `partial`, `serial`, `suggested_mbr_type`, confianza y anomalías `image_shifted`/`mbr_displaced`; nada más) y `scenario_details` el nivel que encuentra cada uno (P2 rejilla, LBA 63 CHS, tercero por encadenamiento P1) e indicadores; la CLI (`cli/tests/corpus/search.rs`, 2) y C# (`PartitionSearchCorpusTests`, 41 casos) repiten la comparación |
| Principal dañada (`ntfs_`, `fat32_`, `exfat_primary_damaged`) | `scenario_details`: sospechosos con `found_primary_damaged`; el NTFS se encuentra por su copia al final (P4) |
| `primary_unreadable`, `backup_unreadable` (`unreadable_lbas`) | Con `FaultyDevice`: «?» y `found_*_unreadable`, nunca el relleno; `unreadable_boot_structures_never_yield_volumes` hace ilegibles **todas** las estructuras de arranque de cada volumen del corpus (ninguno aparece); unitaria `unreadable_fill_that_mimics_a_boot_sector_is_never_evidence` (relleno que es un arranque NTFS perfecto) |
| Imágenes desplazadas (`image_shifted_pos`/`_neg`) | Corpus (desplazamiento ±, tabla en la LBA 8, NTFS parcial); unitaria `shift_needs_two_independent_pieces_of_evidence`; apertura solo con `apply_shift` en mr-api, FFI (`shifted_search_reports_the_shift_and_opens_only_on_request`), CLI (`--found 2` con y sin `--apply-shift`, parcial rechazado) y C# |
| `gpt_4kn_lost` | Corpus con 4096 B y `gpt_4kn_lost_read_as_512_reports_sector_size` (mismos bytes, `found_sector_size_mismatch`); CLI `--sector-size 512 --found 1` |
| `superfloppy`, tablas intactas | Corpus (P0 en el byte 0; ninguna imagen con tabla produce volúmenes ni tablas encontradas); unitaria `table_partition_is_marked_not_duplicated` (`tabla #1`, sin duplicado) |
| Negativos (`empty_disk`, `random_disk`, señuelo) | Corpus (0 volúmenes; `decoy_lbas` nunca son tablas); unitaria `decoy_table_inside_valid_partition_is_ignored` |
| Presupuesto | `small_budget_is_respected_and_reported` (1, 2, 3, mitad y total - 1 lecturas y presupuesto de bytes: `BudgetExhausted`, lecturas del origen ≤ límite, pendientes, determinista); `grid_tiers_respect_their_quota`; FFI, CLI (`--max-reads 1`) y C# |
| Cancelación | `cancellation_stops_before_the_next_read` (tras 0, 1, 3, 7 y 12 lecturas: ninguna más); mr-api (tarea cancelada con resultado), FFI y C# |
| Escala | `reads_do_not_grow_with_disk_size`: dispositivo virtual de 1 GiB y de 2 TiB con el mismo contenido, mismo resultado y lecturas contadas en el origen iguales (± 4) |
| Determinismo | `search_is_deterministic`: dos ejecuciones por manifiesto, misma secuencia de lecturas del origen (envoltorio que las registra) y mismo resultado |
| Reconocedores y fuzzing | 3 unitarias de reconocedores (cada campo fuera de rango, suma exFAT, coherencia), `recognizers_never_panic_on_mutations` (6000 sectores) y `search_fuzz_is_bounded` (150 discos mutados) |

Total: 16 unitarias y 7 de corpus en mr-part, 3 en mr-api, 5 en mr-ffi, 2 de CLI más 2 de corpus,
5 + 41 casos en C# y 1 de UI Automation. Las pruebas de corpus de Rust se ejecutan en los pasos ya
existentes (`cargo test -p mr-part -- --ignored` y `cargo test -p memrescue-cli --test corpus --
--ignored`), sin pasos nuevos.

Copia del particionado (issue #18, PT-10, `formatos/copia-particionado.md`):

| Qué | Pruebas |
|---|---|
| Ida y vuelta con todo el corpus | `core/mr-part/tests/backup_corpus.rs` (`--ignored`): `round_trip_over_the_whole_partition_corpus` toma la copia de los **41** manifiestos (en los 3 APM, lectura APM y región `apm_map` exactas) (con sus `unreadable_lbas` simulados con `FaultyDevice` y la búsqueda exprés incluida), la escribe, la lee y exige igualdad exacta (`==`), mismo texto al reescribirla y comparación sin diferencias con la misma imagen |
| Ida y vuelta sintética | Unitarias en `core/mr-part/src/backup/tests.rs`: MBR con cadena de EBR, GPT con las dos copias y búsqueda, GPT vista con otro tamaño de sector, sectores de tabla ilegibles (región sin contenido), disco en blanco y de un sector, y `every_variant_round_trips` con **todas** las variantes de anomalía MBR, GPT, de volumen y de búsqueda, todas las confianzas, estados y comprobaciones; APM (#17): `apm_round_trips_with_its_map`, `every_apm_variant_round_trips` (las 15 anomalías APM y un tipo con bytes tras el NUL), `apm_field_must_match_the_scheme` (campo `apm` solo con esquema `apm`; `null` explícito aceptado) y `comparison_reports_changed_apm_entry_and_map`; `comparison_reports_differences_on_modified_copies` cubre además `apm_basic` con una entrada movida |
| Manipulaciones | Bytes de una región editados (los detecta `body_sha256` y, si también se recalcula, la suma de la región), sumas editadas, valor del particionado editado, versión 0, 2 y `"1"`, otro `format`, fichero truncado en cada posición de los primeros 2000 bytes y cada 97 después, no UTF-8, mayor de 32 MiB, campos desconocidos, ausentes, de otro tipo o fuera de rango, variante desconocida, región fuera del origen, identidad incoherente y código de anomalía cambiado; los cambios de espacios o CRLF siguen siendo válidos |
| Comparación | Unitarias (entrada MBR movida y EBR borrado, entradas GPT primarias dañadas, origen recortado, otro esquema, sectores antes ilegibles) y de corpus (`comparison_reports_differences_on_modified_copies`: copias modificadas de `mbr_basic` y `gpt_basic` en un directorio temporal; el original queda intacto y sigue coincidiendo) |
| Lector JSON y fuzzing | 4 unitarias del subconjunto JSON (rechazos, anidamiento de 100 000 niveles, línea y columna) y `seeded_fuzz_never_panics` (1500 documentos mutados y 500 aleatorios: sin pánicos; solo pasan mutaciones sin efecto) |
| CLI | `cli/tests/partition_backup.rs` (3): `partitions --save-backup` (código 2 sin `--overwrite`, nunca sobre el origen, origen intacto), `partition-backup-check` igual (código 0, JSON) y sobre una copia modificada (código 4, `region_changed`, `partition_changed`), copia alterada o truncada (código 1), `find-partitions --save-backup` con la búsqueda y validación de argumentos |

FFI e interfaz de la copia del particionado: pendientes.

Lector NTFS (issue #20, ADR-025, `formatos/ntfs.md`):

- `core/mr-fs-ntfs/src` (33 unitarias): arranque (`boot`: cada campo fuera de rango con su código,
  codificaciones de clúster grande y de tamaño de registro, avisos, fuzzing de 20 000 sectores),
  registros (`record`: fixups correctos y desajuste en cada tramo, USA no válida, `BAAD`, otra firma,
  truncado, límites de cabecera, registro no en uso, fixups de un nodo de 4 KiB, fuzzing de 20 000
  registros con recorrido de atributos y runs), atributos (`attr`: residentes, no residentes, nombres
  y cada límite), runs (`runs`: deltas con signo, dispersos, errores, desbordamiento, límite de runs,
  fusión, fuzzing de 50 000 listas), lectura de tramos con sectores ilegibles (`read`), mapa de la
  MFT (`mft`) y volúmenes sintéticos mínimos (`volume_tests.rs` con `testvol.rs`, clústeres de 512 B,
  1 KiB y 4 KiB): sano, sectores de 4096 B en el origen, arranque principal dañado, ilegible o con la
  copia un sector más allá, copia dañada, distinta, ilegible o fuera del origen, no NTFS, registros
  dañados (fixups, `BAAD`, tamaño asignado), sectores ilegibles de la MFT con `FaultyDevice`
  (también en un registro partido entre runs), registro 0 desde `$MFTMirr`, `$MFTMirr` discrepante o
  ilegible, MFT incompleta o con tamaño no múltiplo, origen truncado y fuzzing de 400 volúmenes.
- `core/mr-fs-ntfs/tests/ntfs_corpus.rs` (3, `--ignored`): `volume_fields_match_every_manifest`
  compara los 10 manifiestos (`serial`, `label`, `version`, `mft_lcn`, `mftmirr_lcn`,
  `mft_record_size`, `index_record_size`, `sector_size`, `cluster_size`, `total_sectors`,
  `mft_entries` = registros + 1 por la entrada virtual que añade TSK, `mft_runs` = runs fusionados y
  recortados al tamaño de datos como los da TSK, `backup_boot_offset`), abre `ntfs_mbr` y `ntfs_gpt`
  por su partición de `mr-part` (`[partition]`: esquema, índice, inicio, sectores y `offset`) y
  comprueba los daños: `ntfs_damaged_boot` se abre desde la copia y lo informa; `ntfs_damaged_mft`
  marca exactamente los registros de `records` y `tsk_unreadable_records` (y sus `regions`);
  `ntfs_damaged_mftmirr` informa los 4 registros de `$MFTMirr` (y su región); las imágenes sanas no
  tienen anomalías ni registros dañados. `file_records_match_state_and_sequence`: las 523 entradas
  `[[files]]` (507 vivas, 13 borradas, 3 sobrescritas): en uso = `live`, secuencia, directorio,
  registro base y atributos recorridos sin errores; las 4 con `mft_damaged` están dañadas;
  `entries_live` y `entries_deleted` (borradas más sobrescritas) cuadran.
  `unreadable_mft_sectors_on_the_corpus`: `ntfs_basic` con `FaultyDevice` en un sector de 3 registros
  (solo esos cambian, a ilegibles, con su LBA) y en `$MFTMirr` (se informa sin cambiar la MFT).
- CLI: `cli/tests/cli.rs` (`ntfs-info` sobre un origen sin NTFS: código 1 con la sugerencia de
  `--partition`; sin argumentos: 2) y `cli/tests/ntfs_corpus.rs` (1, `--ignored`: `ntfs-info --json`
  de las 10 imágenes frente a su manifiesto, `--partition 1` en las particionadas, y salida de texto).
- `mr-api`: `ntfs_open_rejects_a_source_without_ntfs`.

Atributos, runs, flujos y LZNT1 (issue #21, ADR-026, `formatos/ntfs.md`):

- `core/mr-fs-ntfs/src` (21 unitarias más, 54 en total): `values` (valores tipados y cada límite,
  entradas de `$ATTRIBUTE_LIST`, fuzzing de 20 000 valores), `runs` (`decode_extent`: cobertura
  exacta del tramo de VCN), `file` (reglas de disposición sin registros: tamaños, primera
  extensión, formas mezcladas, compresión con clúster grande, unidad de compresión ignorada, runs
  fuera del volumen), `lznt1` (**oráculo de Windows**: los 26 vectores de `tests/vectors/lznt1`,
  generados con `RtlCompressBuffer`/`RtlDecompressBuffer` por `tests/generators/gen_lznt1_vectors.py`,
  se descomprimen y su tamaño y SHA-256 coinciden con el manifiesto, 4 de ellos de más de una unidad
  de compresión; los 3 casos que Windows rechaza también fallan; límite de salida con bloques
  completos; bloques mal formados; ida y vuelta con un compresor de pruebas; fuzzing de 30 000
  entradas sin pánicos y con la salida acotada) y `stream_tests.rs` (volúmenes sintéticos de
  clústeres de 4 KiB): residentes y ADS, dispersos con tamaño inicializado menor (estados por tramo
  y 200 lecturas parciales), sectores ilegibles con `FaultyDevice` (tramo exacto, LBA y patrón; un
  sector ilegible más allá del tamaño inicializado no se lee), origen truncado (`outside_source`),
  fichero comprimido de 4 unidades (comprimida, sin comprimir, dispersa y final parcial; 100 lecturas
  que cruzan unidades), sector ilegible dentro de una unidad comprimida (datos hasta el bloque
  afectado) y bloque dañado (`corrupt`), `$ATTRIBUTE_LIST` residente y no residente con dos
  extensiones, fichero borrado con secuencias incrementadas, extensiones de otro base, reutilizadas,
  ilegibles, con lista propia, fuera de la MFT, con otro estado de uso, solapadas, repetidas, con
  hueco o más allá del tamaño asignado, errores de flujo tipados, extensiones de `$MFT`
  (`mft_extended` y `mft_incomplete`) y fuzzing de 600 volúmenes con registros mutados (sin
  pánicos, salida del tamaño pedido, patrón en cada tramo no fiable).
- `core/mr-fs-ntfs/tests/ntfs_corpus.rs` (5, `--ignored`; 2 nuevas):
  `file_contents_match_every_manifest` abre **cada** fichero y flujo alternativo de los 10
  manifiestos y compara tamaño, `resident`, `runs` (fusionados, sin dispersos), `sparse`,
  `init_size` (los bytes más allá son `uninitialized`, los dispersos `sparse`), los flujos `$DATA`
  del fichero y el SHA-256 de todo el contenido leído (sin ningún tramo no fiable): 419 vivos, 12
  borrados íntegros, 3 sobrescritos (con `recovered_sha256`, lo que devuelve TSK) y 5 flujos
  alternativos (uno de un directorio) = **439 flujos con el SHA-256 del manifiesto**; las 4 entradas
  con `mft_damaged` dan `RecordUnusable` (dañado); `$STANDARD_INFORMATION` y `$FILE_NAME` válidos y
  ninguna anomalía en ningún fichero. `unreadable_clusters_inside_a_file_are_reported_per_range`:
  `ntfs_basic` con 3 sectores ilegibles dentro de `/datos/historico.bin`: exactamente dos tramos
  `unreadable` con su LBA, patrón `BAD!` en esos bytes y el resto idéntico a la lectura sana.
- CLI: `cli/tests/cli.rs` (`ntfs_cat_and_stat_validate_arguments_before_writing`: origen sin NTFS
  sin crear la salida, 7 errores de uso) y `cli/tests/ntfs_corpus.rs` (2 nuevas, `--ignored`):
  `ntfs-cat` de 116 flujos (ficheros y ADS de `ntfs_sparse_ads`, `ntfs_gpt` con `--partition 1` y
  borrados de `ntfs_deleted`, a la salida estándar o con `-o`) con el SHA-256 del manifiesto y
  `ntfs-stat --json` con sus runs y dispersos; imagen truncada: código 3, tramo listado en la
  salida de errores y patrón solo en esos bytes; salida = origen o dispositivo rechazada (origen
  intacto), flujo inexistente, registro fuera de la MFT y registro dañado.
- `mr-api`: `output_path_is_never_the_source_or_a_device` y
  `create_output_is_atomic_and_never_overwrites_silently` (temporal visible solo hasta confirmar,
  salida existente rechazada sin `overwrite` y sustituida con él, escritura sin confirmar que no
  deja ni fichero final ni temporal, sustitución fallida que conserva el anterior, origen
  rechazado también con `overwrite`). CLI de corpus: salida existente con código 2 sin
  `--overwrite`, sustituida con él y sin temporales.
- Publicación sin sustituir (#58), `mr-api`: `create_output_commit_never_replaces_a_concurrent_file`
  con el enlace duro y con la copia exclusiva forzada (variante interna `Publish::CopyOnly`, solo en
  pruebas): camino normal, destino creado entre `create_output` y `commit` rechazado con
  `OutputExists` sin cambiar el fichero ajeno ni dejar temporal, sustitución con `overwrite`, y 64
  carreras reales con otro hilo que crea el destino en exclusiva (gana exactamente uno y el destino
  tiene su contenido). `hard_link_unsupported_is_recognised_only_for_unsupported_errors` fija qué
  errores del enlace activan la copia. No se prueba en un volumen FAT real (requiere montarlo).

Ficheros borrados (issue #23, ADR-027, `formatos/ntfs.md` «Ficheros borrados»):

- Unitarias (`core/mr-fs-ntfs/src/deleted_tests.rs` y `bitmap.rs`, 13): ver `formatos/ntfs.md`.
- `core/mr-fs-ntfs/tests/ntfs_deleted_corpus.rs` (2, `--ignored`):
  `deleted_entries_match_every_manifest` recorre los 10 manifiestos: **cada** entrada `deleted` u
  `overwritten` (16: 11 de `ntfs_deleted` con un directorio y 5 de `ntfs_overwritten`) aparece con
  su registro, secuencia, nombre, tipo, `path` completo y padre no huérfano; ningún vivo aparece y no
  hay borrados que el manifiesto no conozca; `recoverable` igual al manifiesto (12 `full`, 1
  `partial`, 2 `none`); los `full` se recuperan con el `sha256` del manifiesto y sin ningún tramo no
  fiable; los `partial`/`none` producen el `recovered_sha256`, con `clusters` y
  `overwritten_clusters` iguales, todos los sospechosos con dueño vivo, los dueños iguales a los del
  `reason` (por ruta), los bytes `possibly_overwritten` iguales a los de esos clústeres (recortados
  al tamaño) y el resumen con los dueños; `$Bitmap` sin problemas en todas las imágenes; en
  `ntfs_damaged_mft` los 4 registros dañados están en `problems` como dañados, nunca entre los
  borrados, y recuperarlos da `RecordUnusable` (dañado).
  `unreadable_bitmap_and_data_sectors_are_never_reported_as_recoverable`: un sector ilegible de
  `$Bitmap` deja `superviviente_2.bin` en `none` (10 clústeres desconocidos) aunque su contenido
  siga intacto (se copia tal cual, 40 000 bytes `possibly_overwritten`) y el residente sigue
  `full`; un sector ilegible en un clúster libre de `objetivo_3.bin` da exactamente un tramo
  `unreadable` con su LBA y patrón, además de los `possibly_overwritten`.
- CLI (`cli/tests/ntfs_deleted.rs`): `ntfs_deleted_and_recover_validate_arguments_before_writing`
  (origen sin NTFS sin crear la salida, 8 errores de uso) y, con `--ignored`,
  `ntfs_deleted_lists_the_manifest_entries` (`ntfs-deleted --json` de `ntfs_deleted`,
  `ntfs_overwritten`, `ntfs_damaged_mft` y `ntfs_basic`: registro, secuencia, tipo, ruta,
  recuperabilidad y dueños de cada entrada, ningún vivo, dañados en `problems`; texto; `ntfs_gpt`
  con `--partition 1` sin borrados) y `ntfs_recover_matches_the_manifests_and_refuses_unsafe_outputs`
  (`ntfs-recover` de los 15 ficheros borrados: código 0 y `sha256` los íntegros, código 3,
  `recovered_sha256`, tramos y dueños en la salida de errores los sobrescritos; salida existente
  con código 2 y sustituida con `--overwrite`; salida = origen rechazada; registro dañado sin
  salida; sin temporales).

Campos del manifiesto NTFS que no se comparan todavía: `[[files]]` `path` de los vivos (#22; el de
los borrados se compara con la ruta por la cadena de padres de la MFT).

## 7. Registro

Los resultados de cada hito se anotan en `PROGRESO.md` con: suites ejecutadas, nº de pruebas,
fallos conocidos y versión del corpus.
