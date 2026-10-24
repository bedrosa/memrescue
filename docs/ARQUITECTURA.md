# Arquitectura técnica de MemRescue

Estado: **inicial (propuesta)**. Las elecciones tecnológicas dependen de ADR-003/004/005/007/008,
pendientes de confirmación.

## 1. Objetivos de diseño

1. **Fiabilidad sobre datos dañados**: ningún parser puede provocar un fallo del proceso ante datos
   corruptos; todo error es un valor, con nivel de confianza.
2. **Nunca empeorar la situación**: solo lectura por defecto; la escritura es explícita, diferida y
   reversible (ADR-006).
3. **Escala**: discos de decenas de TB, millones de ficheros, escaneos de días con reanudación.
4. **Paridad comprobable** con DMDE Professional (ADR-002).
5. **Familiaridad** para usuarios de DMDE con identidad propia.

## 2. Vista de capas

```
┌────────────────────────────────────────────────────────────────────────────┐
│ app (C#/WPF, .NET 10)   MemRescue.exe                                      │
│  Selector de dispositivo · Particiones · Árbol+Panel · Editor hex ·        │
│  Escaneo · Recuperación · RAID · Clonado · Informes · Ajustes              │
├──────────────────────────────── FFI C (memrescue_core.dll) ────────────────┤
│ mr-api        Tareas asíncronas, progreso, cancelación, eventos, handles   │
├────────────────────────────────────────────────────────────────────────────┤
│ mr-recover    Motor de recuperación (copiado, conflictos, filtros, splits) │
│ mr-report     Informes, listas TXT/CSV/HTML, hashes, listas de sectores    │
│ mr-scan       Orquestador de escaneo completo + persistencia (SQLite)      │
│ mr-raw        Escaneo RAW por firmas + calculadoras de tamaño por formato  │
│ mr-vfs        Sistema de archivos virtual (fusión FS + encontrados + RAW)  │
├────────────────────────────────────────────────────────────────────────────┤
│ mr-fs-*       ntfs · fat · exfat · ext · hfsplus · apfs · refs · btrfs ·   │
│               iso9660   (lector + escáner de fragmentos por FS)            │
│ mr-part       MBR/EBR · GPT · APM · búsqueda de particiones perdidas       │
├────────────────────────────────────────────────────────────────────────────┤
│ mr-raid       Dispositivos virtuales: RAID 0/1/4/5/6, rotación custom,     │
│               JBOD, NULL, cálculo automático                               │
│ mr-image      RAW/IMG, imágenes divididas, VHD, VHDX, VMDK, E01 (lectura)  │
│ mr-changes    Capa de cambios diferidos (overlay), deshacer, rollback      │
├────────────────────────────────────────────────────────────────────────────┤
│ mr-io         BlockDevice trait · lector tolerante a errores (reintentos,  │
│               saltos, rellenos, mapa de sectores malos) · caché            │
│ mr-io-win     \\.\PhysicalDriveN, volúmenes, IOCTL, SCSI/ATA pass-through, │
│               SMART/NVMe, bloqueo de volúmenes                             │
├────────────────────────────────────────────────────────────────────────────┤
│ mr-common     Tipos (LBA, rangos, confianza), hashes (CRC-32, MD5, SHA-1,  │
│               SHA-256), descompresores (LZNT1, zlib, LZFSE…), logging      │
└────────────────────────────────────────────────────────────────────────────┘
       mr-clone (copia de sectores) usa mr-io + mr-raid + mr-image
       mr-hex   (modelo del editor + motor de plantillas) usa mr-io + mr-changes
       memrescue-cli usa mr-api
```

Regla de dependencias: cada capa solo depende de las inferiores. Los FS no conocen la UI ni el
escaneo global; exponen dos interfaces: **leer** un volumen y **reconocer fragmentos** en un bloque.

## 3. Abstracciones centrales

### 3.1 `BlockDevice`
Lectura (y escritura opcional) por LBA con tamaño de sector, tamaño total, identidad y
capacidades. Implementaciones: disco físico, volumen, fichero de imagen, contenedores
(VHD/VHDX/VMDK/E01), RAID virtual, partición (vista con offset), overlay de cambios, dispositivo
NULL, dispositivo simulado de pruebas con mapa de errores.

### 3.2 Lector tolerante (`ResilientReader`)
Envuelve un `BlockDevice` y aplica la política de E/S: búfer máximo, reintentos (CRC/seek),
lectura sector a sector tras error, salto de N sectores, lectura hacia atrás tras salto, patrones
de relleno para sectores malos/saltados, omisión de errores por código, reconexión. Mantiene un
**mapa de sectores** (leído / malo / saltado / no intentado) compartido con clonado e informes.

### 3.3 Resultado con confianza
Todos los validadores devuelven `Valid`, `ValidWithFixes(lista)`, `Suspicious(motivos)` o
`Invalid(motivo)`, y pueden operar en modo estricto o tolerante. (Patrón observado en DMDE, doc
`investigacion/dmde-fase1/05-algoritmos-verificados.md`, implementado de forma original.)

### 3.4 Volumen y FS virtual
Un `Volume` expone un árbol de nodos (`Node`: id, nombre(s), flujos, extents, tiempos, atributos,
estado normal/borrado/encontrado/RAW, origen del dato, confianza). `mr-vfs` fusiona: estructura
original + fragmentos encontrados + resultados RAW, con carpetas sintéticas propias
(equivalentes funcionales a "raíz", "huérfanos", "sin nombre", "RAW").

### 3.5 Escaneo completo
Recorre el dispositivo con estrategia "a saltos" (gruesa → fina), entrega cada bloque a los
reconocedores de cada FS y de RAW, agrupa fragmentos en **candidatos de volumen** con
parámetros y puntuación, y persiste todo en SQLite para pausar/reanudar y reconstruir sin
re-escanear.

## 4. Gestión de tareas

- Toda operación larga es una `Task` con id, estado, progreso (fracción + texto + contadores),
  cancelación cooperativa, pausa cuando aplique, y registro de eventos.
- Pool de hilos de CPU (`rayon`) para análisis; E/S secuencial por dispositivo para no castigar
  discos dañados (paralelismo de E/S configurable solo en imágenes/SSD sanos).
- Eventos que requieren decisión del usuario (conflicto de nombre, error de lectura con política
  "preguntar") se publican como **peticiones** que la UI o la CLI responden; con política
  automática no bloquean.
- La API FFI expone: crear tarea, consultar progreso, suscribirse a eventos, responder peticiones,
  cancelar.

## 5. Interfaz (organización funcional)

Inspirada en el flujo de trabajo de DMDE (manual oficial), con diseño y textos propios:

1. **Selector de origen**: discos físicos, volúmenes, imágenes, RAID guardados, logs de escaneo;
   botón de parámetros de E/S; opción de búsqueda rápida de particiones al abrir.
2. **Particiones**: lista con indicadores de estructuras (tabla, entrada, boot, copia, FS básico,
   espejo), encontradas/eliminadas, modo avanzado para modificar, cambios virtuales hasta aplicar.
3. **Ventana principal**: árbol de directorios + panel de ficheros + panel de búsqueda + editor,
   con disposiciones intercambiables; marcadores de estado (borrado / encontrado / RAW / confianza).
4. **Escaneo completo**: parámetros, progreso, lista de volúmenes candidatos con calidad, versiones.
5. **Reconstrucción**: modos rápido / por defecto / completo / personalizado y control Más/Menos.
6. **Recuperación**: categorías, filtros, destino, conflictos, errores, informe y hashes.
7. **RAID**: constructor, rotación personalizada, cálculo automático con restricciones.
8. **Copia de sectores**: origen/destino, rangos, inversa, log/reanudación, mapfile ddrescue.
9. **Editor hex**: vistas por plantilla, navegación por estructuras, modo edición con colores de estado.
10. Menús organizados por **Disco / Herramientas / Ventanas / Editor / Modo / Edición** para que el
    usuario de DMDE encuentre las funciones en el mismo lugar lógico.

Todo control sin implementar se muestra deshabilitado con indicación "pendiente" (no se ocultan
ni se simulan).

## 6. Seguridad y calidad

- `#![forbid(unsafe_code)]` en todos los crates salvo `mr-io-win` y la frontera FFI, que aíslan el `unsafe`.
- Límites explícitos de memoria/tiempo en parsers (tamaños máximos, profundidad de árboles, ciclos).
- Fuzzing de cada parser; pruebas con imágenes corrompidas de forma determinista.
- Escritura física deshabilitada en compilación hasta el hito correspondiente (ADR-006).

## 7. Estado implementado (M1 iteración 2, 2026-10-17)

| Componente | Ubicación | Qué hace hoy | Pruebas |
|---|---|---|---|
| `mr-common` | `core/mr-common` | `Lba`, `ByteRange` con aritmética comprobada; `Confidence`; hashes CRC-32/MD5/SHA-1/SHA-256 incrementales | 6 unitarias |
| `mr-io` | `core/mr-io` | `BlockDevice` (+ `read_sectors`, `sector_span`, `allocation`); `path_policy` (lista blanca de rutas, ADR-014); `ImageFile`; `format` (detección de contenedores, ADR-015); `resilient` (lectura tolerante con mapa de validez, salto y lectura hacia atrás, ADR-019); `RangeView` (rango como dispositivo); `FaultyDevice` (ilegibles, intermitentes, dispersos, recuento de lecturas); `MemDevice`; `NullDevice` (ceros, asignación dispersa) y `PatternDevice` (patrón constante) | 59 unitarias |
| `mr-image` | `core/mr-image` | `open_image`/`open_split` (apertura unificada); VHD fijo/dinámico; VHDX fijo/dinámico + CRC-32C; VMDK monolithicSparse y streamOptimized + intérprete de descriptores; imágenes divididas; `Reblocked` (ADR-017/018) | 56 unitarias + 15 de divididas + 8 contra qemu-img |
| `mr-part` | `core/mr-part` | Base de M2: tipos comunes (`PartitionTable`, `Partition`, `Scheme`, `Origin`, `Anomaly`, `Severity`) y `sector::read_trusted` (lectura con mapa de validez: un sector ilegible o saltado nunca se interpreta como estructura). **MBR/EBR (#12)**: `mbr::read_mbr`/`read_mbr_at` (primarias, cadena de EBR con ambos modos de direccionamiento y tipos 0x05/0x0F/0x85, detección de bucles y enlaces fuera del disco o de la extendida, límite de 1024 EBR, MBR protector e híbrido, disco sin particionar), `MbrAnomaly` tipadas con código estable y gravedad, confianza por partición (ADR-022), nombres de tipo; **GPT (#13)** en `gpt` (cabeceras, copias, tolerancias). **Integración** (`table`): `read_partition_table` une MBR y GPT (esquema, particiones GPT con nombre, copia en uso, desplazamiento, `hybrid_mbr_mismatch`), códigos y gravedad de las anomalías GPT, nombres de tipo GPT, `open_coordinates` (rechaza el desplazamiento salvo petición explícita, ADR-022 adenda). **Búsqueda exprés de particiones perdidas (#15, ADR-023)** en `search`: candidatos por niveles P0-P4 con presupuesto acotado y caché determinista (`search/reader.rs`), reconocedores NTFS, FAT12/16/32 y exFAT tras el rasgo `Recognizer` (`search/fs.rs`), tablas MBR/EBR estrictas y cabeceras GPT en posiciones candidatas (`search/tables.rs`), evidencia e indicadores, confianza, hipótesis de desplazamiento con dos evidencias; detalle en `formatos/busqueda-particiones.md` | unitarias de MBR (23), GPT, integración (6), `sector` y búsqueda (16); corpus GPT en `tests/gpt_corpus.rs`, búsqueda en `tests/search_corpus.rs` (7) y tabla completa en `cli/tests/corpus` |
| `mr-fs-ntfs` | `core/mr-fs-ntfs` | **NTFS (#20, ADR-025)**: sector de arranque con validación estricta y copia del último sector (situada por su propio total, informada), `$MFT` desde los runs del `$DATA` del registro 0 (o de su copia en `$MFTMirr`), registros FILE con la secuencia de actualización comprobada en cada tramo (un desajuste deja el registro dañado), cabecera e iterador acotado de atributos, *mapping pairs* con aritmética comprobada, lectura de registros por número y enumeración por lotes con un estado por registro (en uso, no en uso, vacío, sin inicializar, sin run, fuera del origen, ilegible, dañado), comparación con `$MFTMirr`, etiqueta y versión; todo por `read_trusted` (un sector ilegible nunca se interpreta). **Ficheros y datos (#21, ADR-026)**: valores tipados (`values`), fichero = registro base + extensiones de `$ATTRIBUTE_LIST` con anomalías tipadas (`file`, también para completar `$MFT`), disposición de cada flujo con extensiones y runs comprobados (`StreamLayout`), lectura con estado por tramo (datos, disperso, sin inicializar, ilegible, sin mapa, fuera del origen, comprimido dañado) y memoria acotada (`stream`), LZNT1 de [MS-XCA] (`lznt1`); **borrados (#23, ADR-027)**: `$Bitmap` por páginas (`bitmap`), borrados de la MFT con recuperabilidad por clúster e índice acotado de dueños (`deleted`), recuperación con el tramo `possibly_overwritten` (`recover`). **Árbol (#22, ADR-028)**: índices `$I30` con nodos `INDX` comprobados uno a uno (`index`), directorios contrastados con la MFT, árbol con huérfanos, rutas y ruta de un registro (`dir`), `$UpCase` y colación (`upcase`); `testvol` (volúmenes sintéticos) se exporta con la característica `test-support` para las pruebas de `mr-api` y `mr-ffi`. Detalle en `formatos/ntfs.md` | 67 unitarias (con fuzzing de arranque, registros, runs, valores, LZNT1, volúmenes, ficheros, `$Bitmap`, clasificación y borrados; vectores LZNT1 de Windows) + 7 de corpus NTFS (`--ignored`); #22: 13 unitarias de índices y árbol (con fuzzing de nodos y volúmenes) y 3 de corpus |
| `mr-store` | `core/mr-store` | SQLite v1 con propietario exclusivo, `synchronous=FULL`, `quick_check`, transiciones atómicas (ADR-016) | 11 unitarias |
| `mr-api` | `core/mr-api` | Tareas (pánicos capturados), `OpenSource` (`Arc<dyn BlockDevice>` + metadatos), `open_with`/`open_split_source`/`open_range`, `read_sectors`/`read_sectors_mapped`, hashes estrictos, verificación, `partitions` y `open_partition(origen, tabla, índice, apply_shift)` (partición como `RangeView`, recortada al final del origen si hace falta; desplazamiento solo explícito); `ntfs_open` (volumen NTFS de un origen, #20); `ntfs_file`, `ntfs_open_stream`, `ntfs_read_stream`, `ntfs_copy_stream` y `check_output_path` (#21); `ntfs_list_dir`, `ntfs_resolve`, `ntfs_path_of`, `start_ntfs_tree_task` (árbol cancelable con progreso), `ntfs_stream_source` (fichero como origen de 512 B para el visor) y `ntfs_copy_file`/`start_ntfs_copy_task` (copia con `create_output`) (#22) | 26 unitarias (4 del árbol NTFS) + 1 de extremo a extremo con qemu-img (15 conversiones del corpus) |
| `mr-ffi` | `core/mr-ffi` | `memrescue_core.dll`, ABI 3 ([API_FFI.md](API_FFI.md)); desde #22, sistema de archivos NTFS (`mr_ntfs_*`, `src/ntfs.rs`) | 38 unitarias (5 de NTFS sobre un volumen sintético) |
| `memrescue-cli` | `cli/` | `version`, `probe`, `image-info`, `split-list`, `partitions` (MBR y GPT: tabla, copia GPT, desplazamiento, confianza y anomalías; `--json`), `find-partitions` (búsqueda exprés de particiones perdidas con indicadores; `--json`, `--max-reads`), `ntfs-info` (arranque usado, MFT, `$MFTMirr` y estado de cada registro; `--json`), `ntfs-stat` (atributos, flujos, runs; `--json`) y `ntfs-cat` (contenido de un flujo con `--stream` y `-o`, por `--record` o por ruta; código 3 y patrón solo en los tramos no fiables), `ntfs-ls` (directorio o, con `-r`, subárbol con huérfanos; `--json`; #22), `sectors` (con estado por sector), `verify` (`--jump`, `--read-back`), `hash`, `db-init`, `jobs`; opciones `--force-raw`, `--split`, `--range`, `--partition N`, `--found N`, `--apply-shift` | 9 extremo a extremo + 3 contra corpus + 1 contra el corpus NTFS |
| `MemRescue.Core.Interop` | `app/src/MemRescue.Core.Interop` | `Engine`, `Source` (`OpenRange`, `ReadSectorsMapped`, `Description`), `HashTask`, `VerifyTask`, `SectorState`, `HexDump` | 52 xUnit |
| `MemRescue` (WPF) | `app/src/MemRescue` | Menús del flujo de referencia; orígenes (contenedores, divididas con confirmación, rangos como dispositivo); visor con estado por sector, aviso de no fiables y barra de posición; propiedades; hashes; legibilidad con salto/lectura hacia atrás; registro; `--open`, `--selftest`, `--screenshot` | 10 pruebas UI Automation + autodiagnóstico 512 y 4096 B |
| Generador y validador | `tests/generators` | Imágenes MBR/GPT deterministas + validador independiente + pruebas negativas | 128 comprobaciones + 9 corrupciones |

No existe todavía: sistemas de archivos más allá de la lectura NTFS de #20 a #22 (borrados #23), escaneo, recuperación, RAID,
clonado, contenedores diferenciales/EWF/QCOW/VDI/DMG, acceso a dispositivos.

## 8. Estructura del repositorio

```
memrescue/
  core/            workspace Rust (crates mr-*)
  cli/             memrescue-cli
  app/             MemRescue (WPF) + MemRescue.Core.Interop + pruebas xUnit
  tests/
    generators/    scripts para crear imágenes sintéticas
    corpus/        imágenes pequeñas versionadas + manifiestos con hashes esperados
  docs/            esta documentación + investigacion/
```
