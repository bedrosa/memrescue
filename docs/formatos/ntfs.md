# NTFS: sector de arranque, MFT, registros FILE, atributos y datos

Crate: `core/mr-fs-ntfs` (`mr_fs_ntfs::NtfsVolume`). Solo lectura. Issues #20, #21 y #22 (M3),
paridad FS-01 a FS-04, ADR-025, ADR-026 y ADR-028 (índices, árbol y rutas). Los borrados (qué datos
siguen disponibles y con qué fiabilidad) son de #23 (FS-16, ADR-027): registros MFT no en uso y
entradas residuales de los índices `INDX`, combinados.

## Especificación usada

Disposición pública del formato en disco de NTFS (documentación del controlador NTFS del núcleo
Linux y documentación pública del formato NTFS de libyal y del proyecto Linux-NTFS) y la
especificación pública de Microsoft [MS-XCA] §2.5 para LZNT1. Implementación original; de la
investigación de la referencia (`investigacion/dmde-fase1/05-algoritmos-verificados.md` §5.1 y
§5.3) solo se han tomado como hechos observados qué campos valida (sin traducir código).

## Interfaz

```rust
NtfsVolume::open(dev: Arc<dyn BlockDevice>) -> Result<NtfsVolume, NtfsError>
volume.info() -> &VolumeInfo            // arranque, MFT, $MFTMirr, etiqueta, versión, anomalías, confidence()
volume.record_count() -> u64            // data_size de $MFT / tamaño de registro
volume.read_record(n) -> Result<RecordOutcome, NtfsError>
volume.records() / records_in(rango)    // iterador (n, RecordOutcome) en orden, por lotes
volume.file(n) -> Result<NtfsFile, NtfsError>              // registro base + extensiones, atributos, anomalías
volume.open_stream(n, "nombre") -> Result<DataStream, _>   // "" = $DATA principal
volume.stream_of(&file, "nombre")
stream.layout() -> &StreamLayout        // tamaños, compresión, extensiones, runs, mapa, avisos
stream.read(offset, len) -> Result<StreamChunk, _>         // bytes + estado de cada tramo
stream.copy_to(&mut impl Write) -> Result<CopyReport, _>   // flujo entero con memoria acotada
lznt1::decompress(src, límite)          // [MS-XCA] con límites estrictos
scan_deleted(&volume, &ScanOptions, &observador) -> Result<Option<DeletedScan>, _>  // #23; None = cancelado
residual::residual_entries(&volume, registro)          // entradas residuales del índice de un directorio
residual::parse_candidates(búfer, desde, hasta, &DirContext)  // validación estricta de restos
ClusterBitmap::open(&volume).runs(lcn, n)              // asignación libre/asignado/desconocido
recover_stream(&volume, n, "nombre", &mut w, &observador) -> Result<Option<RecoveryReport>, _>
stream.with_suspect_clusters(tramos)    // lo leído de esos clústeres: possibly_overwritten
```

- `mr_api::ntfs_open(&Source)` abre el volumen de un origen: imagen sin tabla, partición
  (`open_partition`) o volumen encontrado (`open_found_volume`). Errores en `ApiError::Ntfs`.
- `mr_api::ntfs_file`, `ntfs_open_stream`, `ntfs_read_stream` (con el estado de cada tramo) y
  `ntfs_copy_stream` (#21), por número de registro MFT y nombre de flujo; `check_output_path`
  valida el destino de una extracción (nunca el origen, nunca un dispositivo; si un fichero del
  origen o el destino no se pueden resolver, rechaza la escritura: adenda de ADR-026, #76).
- CLI: `memrescue-cli ntfs-info <imagen> [--partition N | --found N | --range I:N] [--json]`
  (arranque usado y estado de la copia, campos, serie, etiqueta, versión, runs de `$MFT`, comparación
  con `$MFTMirr`, recuento de registros por estado y lista de dañados e ilegibles). Código de salida
  3 si algún registro tiene sectores ilegibles.
- CLI (#21): `ntfs-stat <imagen> --record N [ORIGEN] [--json]` (cabecera, `$STANDARD_INFORMATION`,
  `$FILE_NAME` con espacio de nombres, padre y fechas UTC, `$OBJECT_ID`, cada atributo con el
  registro que lo contiene, cada flujo con tamaños, compresión, runs fusionados, dispersos, tramos
  sin mapa, extensiones y avisos, y las anomalías del fichero) y `ntfs-cat <imagen> --record N
  [--stream NOMBRE] [-o SALIDA] [ORIGEN]` (contenido a la salida estándar o a un fichero). La
  salida no puede ser el propio origen ni uno de sus segmentos (comparación de rutas canónicas), ni
  una ruta de dispositivo (la misma lista blanca léxica que los orígenes, ADR-014), ni un
  directorio. Si la salida ya existe, se rechaza (código 2) salvo con `--overwrite`. La escritura
  va a un temporal nuevo del mismo directorio (`.<nombre>.<pid>-<n>.mrtmp`) que se publica con el
  nombre final solo al terminar bien; ante un error o una interrupción el temporal se borra, así que
  nunca queda un fichero parcial con el nombre final (ni se pierde el anterior al sustituirlo). Sin
  `--overwrite`, la publicación tampoco sustituye un fichero creado por otro proceso después de la
  comprobación inicial (#58): es un enlace duro del temporal al nombre final, que falla de forma
  atómica si el nombre ya existe (código 2, el fichero ajeno no cambia), seguido del borrado del
  temporal. En sistemas de ficheros sin enlaces duros (FAT, exFAT) se crea el destino en exclusiva
  (`create_new`) y se copia el temporal: nunca sustituye nada, pero mientras dura la copia el
  destino existe incompleto; si la copia falla se borra ese destino (lo creó la propia operación).
  Con `--overwrite`, el renombrado sustituye el anterior de forma atómica. La
  política vive en `mr_api::create_output` / `OutputFile::commit` para que la reutilice la
  recuperación de M4. **Limitación**: no se detectan enlaces duros al origen (otra ruta al mismo
  fichero); solo se compara la ruta canónica. Los tramos no fiables se escriben con el patrón `BAD!`
  **solo** en esos bytes, se listan en la salida de errores (desplazamiento, longitud, estado y LBA)
  y el código de salida es 3. Un registro no en uso (borrado) se lee igual, con un aviso.
- #23: `mr_api::ntfs_deleted(volumen, ctx)` y `start_ntfs_deleted_task(Arc<NtfsVolume>)`
  (cancelable, progreso en registros leídos de las dos pasadas, entradas evaluadas y directorios
  cuyo índice se lee; al cancelar, `ApiError::Cancelled`), `ntfs_recover_record(origen, volumen,
  registro, salida, sobrescribir)` y `ntfs_recover_stream(..., flujo, ..., ctx)`: evalúan y abren el
  flujo antes de crear la salida (un error no deja ficheros) y escriben con `create_output`.
  `ntfs_recover_deleted(origen, volumen, registro, secuencia, flujo, salida, sobrescribir, ctx)` y
  `start_ntfs_recover_task` (la de la interfaz y la FFI) exigen además un registro base no en uso
  con la secuencia de la entrada y con `$FILE_NAME`: una entrada solo residual (registro
  reutilizado, dañado o libre) nunca se recupera.
- CLI (#23): `ntfs-deleted <imagen> [ORIGEN] [--json]` (borrados con registro, secuencia, tipo,
  tamaño, ruta o `<huérfano N>/nombre`, estado del padre, recuperabilidad, resumen de cada flujo
  no íntegro, orígenes y, en las solo residuales, su estado; registros no interpretables aparte;
  recuentos de la búsqueda en los índices; código 3 si hay registros MFT o nodos de índice
  ilegibles o `$Bitmap` no se puede usar). En JSON, cada entrada lleva `status`,
  `status_message`, `origins` e `index_entries` (directorio, VCN o `null` en la raíz,
  desplazamiento, referencia y nombre de cada entrada de índice) y el resultado, `index_entries`
  con los recuentos (directorios, nodos, ilegibles, no válidos, libres, bytes de espacio libre,
  aceptadas, rechazadas, repetidas, copias de entradas vivas, solo residuales, truncado).
  `ntfs-recover <imagen> --record N -o SALIDA [--stream NOMBRE] [--overwrite] [ORIGEN]` (política
  de salida de `ntfs-cat -o`; en la salida de errores, la recuperabilidad, los tramos posiblemente
  sobrescritos con LCN y dueño y los tramos con patrón; código 3 si algún byte no es fiable).
- FFI (#23): `mr_ntfs_deleted_*` (tarea, estados 16 a 19 de `mr_ntfs_entry`, detalle con
  recuperabilidad, orígenes y clústeres, textos) y `mr_ntfs_recover_*` (tarea, informe y tramos
  sospechosos con su dueño); C# `NtfsVolume.StartDeletedScan` / `StartRecover`. Interfaz:
  «Mostrar borrados» y «Recuperar…» en la pestaña «Sistema de archivos» (ver «Ficheros borrados»).

Módulos y frontera con los siguientes issues:

| Módulo | Contenido | Lo amplía |
|---|---|---|
| `boot` | `parse_boot`, codificaciones de clúster y de tamaño de registro | - |
| `record` | `apply_fixups` (sirve también para `INDX`), `FileRecord`, cabecera, `FileReference` | - |
| `attr` | iterador acotado de cabeceras de atributo (residente/no residente, nombre) | - |
| `values` | `$STANDARD_INFORMATION`, `$FILE_NAME`, `$OBJECT_ID`, `$VOLUME_INFORMATION`, entradas de `$ATTRIBUTE_LIST` | #22 (`$FILE_NAME` de los índices) |
| `runs` | `decode_runs`, `decode_extent`, `merge_contiguous`, `sparse_ranges` | - |
| `file` | `NtfsFile` (base + extensiones), `FileAnomaly`, `StreamLayout`, `StreamError`, `StreamIssue` | - |
| `stream` | `DataStream`: lectura con estado por tramo, compresión, `copy_to`, clústeres sospechosos (#23) | #22 |
| `bitmap` (#23) | `ClusterBitmap`: `$Bitmap` por páginas, `alloc_runs` | - |
| `deleted` (#23) | `scan_deleted`, `ClaimIndex`, `assess_layout`, `DeletedEntry`, `DeletedStatus`, `EntryOrigin`, `StreamAssessment`, `ParentState`, combinación con las residuales | M4 |
| `residual` (#23) | entradas residuales de los índices: `parse_candidates`, `collect_directory`, `ResidualEntry`, `ResidualStats` | - |
| `recover` (#23) | `recovery_stream`, `recover_stream`, `RecoveryReport` | M4 |
| `lznt1` | descompresor [MS-XCA] | - |
| `mft` | `MftLayout` (tramos en disco de cada registro), `RecordOutcome`, `Records` | #22, #23 |
| `volume` | apertura, copia del arranque, `$MFTMirr`, `$Volume`, extensiones de `$MFT`, `file`, `open_stream`, `attribute_stream`, `stream_view`, `upcase` | - |
| `index` (#22) | `$INDEX_ROOT`, nodos `INDX`, `$BITMAP`, `IndexReader` (`walk`, `find`; `slot_count`, `slot_allocated`, `read_slot` y `root_value` para #23), `IndexIssue` | - |
| `dir` (#22) | `DirEntry`, `EntryState`, `list_dir`, `lookup`, `resolve_path`, `path_of`, `tree`, huérfanos | - |
| `upcase` (#22) | `$UpCase`, plegado simple, colación | - |
| `testvol` | volúmenes sintéticos (también para otros crates con la característica `test-support`) | - |

## Índices, directorios y rutas (#22, ADR-028)

```rust
volume.list_dir(record) -> Result<DirListing, NtfsError>     // entradas contrastadas con la MFT + anomalías
volume.lookup(dir, "nombre") -> Result<Option<DirEntry>, _>  // sin distinguir mayúsculas
volume.resolve_path("/a/b") -> Result<DirEntry, NtfsError>   // NotFound, NotADirectory, InvalidPath
volume.path_of(record) -> Result<String, NtfsError>          // Orphan { reason }
volume.tree(&TreeOptions, &dyn TreeObserver) -> Result<Tree, _>  // nodos, anomalías, huérfanos
volume.upcase() -> Arc<Upcase>                                // $UpCase o plegado simple
index::IndexReader::open(&volume, &file)                      // $I30: walk() y find()
```

- API: `mr_api::ntfs_list_dir`, `ntfs_resolve`, `ntfs_path_of`, `ntfs_tree` y
  `start_ntfs_tree_task` (cancelable, con progreso), `ntfs_stream_source` (un fichero como origen de
  512 B para el visor) y `ntfs_copy_file` / `start_ntfs_copy_task` (copia con `create_output`). FFI
  `mr_ntfs_*` y C# `NtfsVolume` (`API_FFI.md`). CLI: `ntfs-ls <imagen> [RUTA] [-r] [--json]` y
  `ntfs-cat` / `ntfs-stat` con una RUTA en lugar de `--record`. Interfaz: pestaña «Sistema de
  archivos» (desde Disco → Abrir sistema de archivos NTFS del origen o desde la pestaña Particiones
  sobre una partición o un volumen encontrado) con árbol, lista (nombre, tamaño, fechas, registro y
  estado), «Abrir en el visor» y «Guardar como…».

**`$INDEX_ROOT`** (residente, nombre `$I30`): tipo indexado 0x30 (`$FILE_NAME`), colación 1 (nombres
de fichero), tamaño de nodo potencia de 2 entre 512 B y 64 KiB (si difiere del arranque se informa,
`index_block_size_mismatch`, y manda el del índice) y la cabecera de nodo en 0x10.

**Cabecera de nodo** (raíz en 0x10, `INDX` en 0x18): primera entrada ≥ 0x10 y múltiplo de 8, bytes
en uso ≥ primera entrada, en uso ≤ asignados ≤ lo que queda del nodo (`index_header_bounds`).

**Entradas**: longitud ≥ 0x10 (0x18 con subnodo), múltiplo de 8 y dentro de los bytes en uso
(`index_entry_length`); clave dentro de la entrada y no vacía salvo en la última
(`index_entry_key_length`); la clave es un `$FILE_NAME` válido (si no, `index_entry_key`: esa entrada
se ignora y el resto se lee); el recorrido termina en la entrada con el indicador de última
(`index_missing_last_entry`). Tras el primer error de estructura el recorrido del nodo se detiene y
las entradas anteriores se conservan (se contrastan igual con la MFT).

**Nodos `INDX`** (`$INDEX_ALLOCATION` de `$I30`, leído por el mapa de validez como cualquier flujo):
VCN en clústeres si el nodo mide al menos un clúster y en unidades de 512 B si es menor; puntero a
subnodo alineado al tamaño del nodo y dentro del flujo (`index_subnode_out_of_range`); bit a 1 en
`$BITMAP` de `$I30` (`index_node_unallocated`: su contenido es residual y no se lista; sin `$BITMAP`
legible se informa `index_bitmap_unusable` y no se comprueba); bytes fiables
(`index_block_unreadable`, ADR-019); firma `INDX` (`index_block_signature`); secuencia de
actualización con `apply_fixups`, sin tolerancia (`index_block_fixup_mismatch`,
`index_block_usa_invalid`); VCN de la cabecera igual al del puntero (`index_block_vcn`). Un nodo que
no cumple se informa y se salta; el resto del árbol B se sigue leyendo.

**Recorrido**: profundidad máxima 32 (`index_depth`), 2^20 nodos (`index_node_limit`) y 2^22 entradas
(`index_entry_limit`); un nodo al que se vuelve a llegar no se sigue (`index_loop`); entradas de un
nodo fuera del orden de colación, `index_order` (informativo, y la búsqueda recorre todo el índice).

**Contraste con la MFT** (`EntryState`): `live` solo si el registro existe, es interpretable, es base,
está en uso y su secuencia es la de la referencia; si no, `stale_not_in_use` (libre: borrado),
`stale_reused` (otra secuencia), `record_unusable` (con el estado del registro: dañado, ilegible...) o
`invalid_reference` (fuera de la MFT o extensión). Solo las vivas son ficheros; las demás se listan
con su estado (las libres y reutilizadas son también entradas residuales de #23, origen
`index_stale`). Además: `entry_name_not_in_record` (el registro no
tiene ese nombre en este directorio), `entry_parent_mismatch` (la clave dice estar en otro
directorio), `entry_duplicate` y `entry_self_reference` (la entrada «.» de la raíz no se lista ni se
informa). Tamaño, fechas y atributos salen del registro (`$DATA` principal y
`$STANDARD_INFORMATION`); si el registro no es utilizable, de la clave del índice
(`times_from_index`).

**Nombres**: las entradas DOS de un registro con otra entrada no DOS en el mismo directorio no se
listan (`dos_name`); Win32 y DOS en una entrada se lista una vez; los enlaces duros aparecen en cada
directorio (y dos veces si están en el mismo). UTF-16 decodificado con U+FFFD en las unidades sueltas
(`name_lossy`); las comparaciones usan las unidades originales. Son de sistema los registros 0-15 y
el contenido de `$Extend`.

**`$UpCase` y colación**: tabla del registro 10 si mide 128 KiB, es legible con datos fiables y pasa
la comprobación ASCII; si no, plegado simple (`char::to_uppercase` de un solo carácter del plano
básico; sustitutos sin cambio) y `UpcaseSource::SimpleFold` con el motivo. Colación: unidades
plegadas una a una, el más corto antes a igualdad de prefijo; a igualdad, desempate por las unidades
sin plegar (orden estable).

**Rutas**: `/` o `\`, componentes vacíos ignorados, `.` y `..` rechazados. Cada componente se busca
bajando por el árbol B según la colación; si el camino tiene un nodo dañado o desordenado, si solo
hay coincidencias sin distinguir mayúsculas (puede haber una exacta en otro nodo) o si el nombre es
8.3, se recorre el índice entero. Se prefiere la coincidencia exacta y después la que no distingue
mayúsculas; solo entradas vivas. `path_of` sube por el `$FILE_NAME` preferido (no DOS) de cada
registro con detección de ciclos y 1024 niveles; vale para registros no en uso, pero cada antecesor
debe ser un directorio en uso con la secuencia de la referencia (si no, `Orphan` con el motivo:
`orphan_parent_out_of_range`, `_unusable`, `_not_in_use`, `_reused`, `_not_directory`, `orphan_loop`,
`orphan_too_deep`, `orphan_no_name`).

**Árbol**: en anchura desde la raíz, solo se recorren directorios vivos; un directorio alcanzado dos
veces no se vuelve a recorrer (`tree_loop`), profundidad máxima configurable (256, `tree_depth`),
nodos máximos (10 millones, estado `limit`), cancelable (estado `cancelled` con el árbol parcial).
Después revisa toda la MFT (registros 16 en adelante) y lista como huérfanos los registros base en
uso con nombre que no se alcanzaron, con el motivo (los anteriores más `orphan_not_indexed`: el
padre es alcanzable pero su índice no lo lista, p. ej. por un nodo perdido; y `orphan_parent_orphan`).
Así lo que colgaba de un nodo dañado no desaparece y su ruta se puede reconstruir con `path_of`.

**Fichero como origen** (visor): sectores de 512 B; tamaño redondeado al sector (los bytes tras el
final del flujo en el último sector son ceros y los sectores enteros tras el final, dispersos); un
tramo no fiable del flujo es un error de lectura, así que la lectura tolerante lo entrega con el
patrón y el estado ilegible. Conserva la ruta y los segmentos de la imagen (una salida nunca puede
ser la imagen).

## Lectura con el mapa de validez

Toda lectura pasa por `mr_part::sector::read_trusted` (ADR-019): un sector `Unreadable` o `Skipped`
nunca se interpreta. Las estructuras NTFS se direccionan en bytes, así que cada lectura informa qué
bytes cubre cada sector no fiable: un registro con **algún** sector no fiable es `Unreadable` (con
las LBA del origen) y no se intenta interpretar; un arranque ilegible no es un arranque. El tamaño
de sector del origen puede ser distinto del del arranque (registros más pequeños que un sector):
se informa (`sector_size_mismatch`) y se lee igual, por bytes.

## Sector de arranque (validación estricta)

| Campo | Regla |
|---|---|
| 0x03 OEM | `NTFS    ` |
| 0x1FE | `55 AA` |
| 0x0B bytes por sector | 512, 1024, 2048 o 4096 |
| 0x0D sectores por clúster | potencia de 2 de 1 a 128, o 0xF4..0xFF = `2^(256 - v)` (clústeres grandes) |
| tamaño de clúster | ≤ 2 MiB |
| 0x0E, 0x10, 0x11, 0x13, 0x16, 0x20 | campos heredados de FAT a 0 |
| 0x28 total de sectores | ≠ 0, al menos un clúster, `total × bytes` sin desbordar |
| 0x30 LCN de `$MFT`, 0x38 LCN de `$MFTMirr` | distintos de 0 y dentro de los clústeres del volumen |
| 0x40 registro MFT, 0x44 registro de índice | positivo = clústeres, negativo `-n` = `2^n` bytes; potencia de 2 entre 512 B y 64 KiB |

Avisos que no invalidan: sin salto x86 en 0x00 (`boot_no_jump`) y relleno distinto de 0 tras los
tamaños de registro (`boot_record_size_padding`). Fuzzing: 20 000 sectores mutados o aleatorios;
todo lo aceptado cumple los límites.

**Copia.** Con el principal válido, se comprueba la copia en el sector siguiente al último que
cuenta el arranque (`total_sectors × bytes_por_sector`) y, si ahí no coincide, un sector más allá
(`boot_backup_shifted`). Estados: `matches` (mismo BPB 0x0B..0x50), `differs`, `invalid`,
`unreadable`, `outside_source` (aviso: p. ej. una partición recortada) o `in_use`.

**Principal no válido o ilegible.** Se buscan copias en el último y el penúltimo sector del origen
(para cada tamaño de sector posible) y solo se acepta una cuyo **propio total de sectores** la sitúa
exactamente ahí (o un sector antes). Se usa y se informa (`boot_primary_invalid` o
`boot_primary_unreadable`, gravedad compensada: confianza `ValidWithFixes`). Si no hay ninguna,
`NtfsError::NotNtfs` con el examen de ambas posiciones.

## Registros FILE

1. Tamaño exacto del registro (si no, `record_truncated`); un registro entero a ceros es `Empty`
   (nunca inicializado), no dañado.
2. Firma `FILE`; `BAAD` (marcado por el propio sistema de archivos) es `record_baad`; cualquier otra,
   `record_bad_signature`.
3. Secuencia de actualización (tramos de 512 B): desplazamiento par, ≥ 8 y con la matriz entera en
   el primer tramo; número de elementos = tramos + 1 (`record_usa_invalid`). **Cada** tramo debe
   terminar en el USN; un desajuste en cualquier tramo es `record_fixup_mismatch` y el registro no
   se usa (no hay tolerancia para tramos fuera de lo usado: un registro a medio escribir nunca se
   acepta en silencio).
4. Cabecera: tamaño asignado = tamaño de registro (`record_allocated_size`); primer atributo
   alineado a 8, después de la matriz y dentro del registro (`record_attrs_offset`); bytes en uso
   ≤ asignados y con sitio para la marca de fin (`record_bytes_in_use`). El número de registro de la
   cabecera (NTFS 3.1) se conserva; si no coincide con la posición se puede consultar
   (`number_mismatch`), sin rechazar.
5. Campos expuestos: LSN, secuencia, enlaces, indicadores (en uso, directorio), bytes en uso y
   asignados, referencia al registro base (48 + 16 bits), siguiente identificador de atributo.

**Atributos** (iterador acotado a los bytes en uso): tipo, longitud ≥ cabecera (0x18 residente,
0x40 no residente), múltiplo de 8 y dentro de lo usado; indicador de forma 0 o 1; nombre dentro del
atributo y fuera de la cabecera; valor residente con desplazamiento ≥ 0x18 y dentro del atributo;
no residente con *mapping pairs* tras la cabecera (0x48 si está comprimido), VCN final ≥ inicial
(o atributo vacío) y, si empieza en el VCN 0, inicializado ≤ datos ≤ asignado. Tras el primer error
el iterador se detiene; si falta la marca de fin, `attr_missing_end`. Cada paso avanza al menos la
cabecera, así que el recorrido termina siempre.

**Runs** (`decode_runs`): nibble de longitud 1 a 8, de desplazamiento 0 a 8 (0 = run disperso,
sin clústeres en disco); longitud > 0 (se lee con signo: una longitud negativa es un error); LCN
acumulado con signo desde 0 en cada extensión, nunca negativo y con `LCN + longitud ≤ clústeres
del volumen`; VCN y LCN con desbordamiento comprobado; como mucho 32 768 runs; sin byte 0 final o
con un run incompleto, `runs_truncated`. `decode_extent` exige además que los runs de una extensión
terminen exactamente en su VCN final + 1 (`runs_coverage`). Cada violación tiene su código
(`runs_length_size`, `runs_offset_size`, `runs_length`, `runs_negative_lcn`, `runs_overflow`,
`runs_outside_volume`, `runs_too_many`). Fuzzing: 50 000 listas aleatorias.

## Atributos tipados (#21)

Valores residentes con su disposición pública y límites comprobados (`values`, error tipado
`ValueError`: `value_too_short`, `file_name_length`, `file_name_namespace`):

| Atributo | Campos | Reglas |
|---|---|---|
| `$STANDARD_INFORMATION` (0x10) | creación, modificación, modificación MFT, acceso (FILETIME), atributos DOS, versiones, clase; desde NTFS 3.0 propietario, seguridad, cuota y USN | ≥ 48 bytes; los campos de 3.0 si hay 72 |
| `$FILE_NAME` (0x30) | padre (número y secuencia), 4 fechas, tamaños asignado y real (copias del momento en que se escribió), atributos, reparación/EA, espacio de nombres (POSIX, Win32, DOS, Win32+DOS), nombre UTF-16 | ≥ 0x42 bytes; nombre no vacío y dentro del valor; espacio de nombres 0..3 |
| `$OBJECT_ID` (0x40) | identificador (16 B) y, si mide 64, volumen y objeto de origen y dominio | ≥ 16 bytes |
| `$VOLUME_NAME` (0x60), `$VOLUME_INFORMATION` (0x70) | etiqueta; versión mayor/menor e indicadores | ≥ 12 bytes |
| `$ATTRIBUTE_LIST` (0x20) | entradas: tipo, longitud, nombre, VCN inicial, registro (número y secuencia), identificador | longitud ≥ 0x20 y múltiplo de 8, dentro de la lista; nombre tras la cabecera (0x1A) y dentro de la entrada; tipo ≠ 0 y ≠ fin; relleno final a ceros admitido; como mucho 32 768 entradas |

Las fechas se muestran en UTC (ISO 8601 con 100 ns) solo en la CLI; el motor las conserva crudas.

## Ficheros: registro base y extensiones (`$ATTRIBUTE_LIST`)

`NtfsVolume::file(n)` exige un registro interpretable (si no, `NtfsError::RecordUnusable` con su
estado: dañado, ilegible, vacío...) y que sea un registro base (si no, `NotBaseRecord`). Sirve
también para registros no en uso (borrados). Si el registro tiene `$ATTRIBUTE_LIST` (residente o no
residente, como mucho 1 MiB leído por el mapa de validez; si una parte no es fiable se usan solo
las entradas de la parte legible), se siguen sus registros de extensión con estas reglas, cada una
con su anomalía tipada (`FileAnomaly`); un registro que no las cumple no aporta atributos y el resto
del fichero sí se usa:

| Regla | Código |
|---|---|
| Como mucho 4096 registros de extensión, cada uno leído una sola vez | `extension_too_many` |
| Registro dentro de la MFT | `extension_out_of_range` |
| Registro interpretable | `extension_unusable` (con su estado) |
| Su referencia al base es exactamente el base: número y secuencia | `extension_base_mismatch` |
| Su secuencia es la de la entrada de la lista (si no, el registro se ha reutilizado) | `extension_sequence_mismatch` |
| No tiene su propia `$ATTRIBUTE_LIST` (no se sigue: evita ciclos) | `extension_has_attr_list` |
| Mismo estado de uso que el base (informativo) | `extension_in_use_mismatch` |
| Cada entrada tiene su atributo (tipo e identificador) en el registro indicado | `listed_attribute_missing` |
| Una sola `$ATTRIBUTE_LIST`; entradas bien formadas; lista legible | `attr_list_multiple`, `attr_list_*`, `attr_list_unreadable` |

Holgura única: en un registro que ya no está en uso, el sistema de archivos incrementa la secuencia
al liberarlo, así que se admite exactamente una unidad más que la referencia (ficheros borrados con
extensiones). Además, si `$MFT` tiene extensiones (los runs del registro 0 no cubren toda la MFT),
al abrir el volumen se siguen con las mismas reglas y, si completan la MFT, se usan
(`mft_extended`, aviso); si no, `mft_incomplete`.

## Flujos: disposición (`StreamLayout`)

Un flujo es el conjunto de atributos del mismo tipo y nombre (`$DATA` sin nombre = el principal;
con nombre = flujo alternativo, ADS). Residente: el valor es el contenido. No residente: sus
extensiones (una por registro) se ordenan por VCN y se comprueban:

| Regla | Si no se cumple |
|---|---|
| Ni cifrado (EFS: lo que hay en disco no es el contenido) | error `stream_encrypted` |
| No mezcla partes residentes y no residentes | error `stream_mixed_forms` |
| Hay una extensión en el VCN 0 (da los tamaños) | error `stream_missing_first_extent` |
| Inicializado ≤ datos ≤ asignado; asignado múltiplo del clúster | error `stream_sizes`, `stream_allocated_not_cluster_multiple` |
| Comprimido: unidad de compresión 2^4 = 16 clústeres y clúster ≤ 4 KiB | error `stream_compression_unit`, `stream_compression_cluster_size` |
| Extensiones sin repetir VCN, sin solaparse y sin pasar del tamaño asignado | error `stream_duplicate_extent`, `stream_extent_overlap`, `stream_exceeds_allocated` |
| Runs de cada extensión válidos y que cubren exactamente su tramo | aviso (código del run) y ese tramo queda **sin mapa** |
| Extensiones contiguas | aviso `stream_extent_gap` y el hueco queda sin mapa |
| Las extensiones cubren los datos inicializados | aviso `stream_runs_short` y el resto queda sin mapa |
| Unidad de compresión sin indicador de comprimido ni de disperso | aviso `stream_compression_unit_ignored` |

Los errores impiden saber qué hay en el flujo (`NtfsError::Stream`); los avisos dejan tramos sin
mapa que se leen como no fiables. El mapa resultante (`segments`) cubre `[0, clústeres de los
datos)` con tramos en disco, dispersos o sin mapa.

## Lectura de flujos con el mapa de validez

`DataStream::read(offset, len)` (como mucho 16 MiB por petición; el disco se lee por tramos de
4 MiB) devuelve los bytes y el estado de cada tramo:

| Estado | Contenido | ¿Fiable? |
|---|---|---|
| `data` | leído del disco (por `read_trusted`), valor residente o unidad descomprimida | sí |
| `sparse` | run disperso o unidad de compresión sin clústeres: ceros por definición | sí |
| `uninitialized` | más allá del tamaño inicializado: ceros por definición, **nunca se lee** el disco | sí |
| `unreadable` | sector ilegible o saltado del origen (con la LBA y el número de sectores) | no |
| `unmapped` | sin extensión que lo describa | no |
| `outside_source` | clústeres más allá del final del origen (volumen truncado) | no |
| `corrupt` | unidad de compresión que no se puede descomprimir | no |
| `possibly_overwritten` | leído del disco, de un clúster que un borrado ya no tiene en exclusiva (solo al recuperar, #23) | no, pero **sin** patrón: bytes tal como están |

Los tramos no fiables llevan el patrón `BAD!` (configurable con `with_fill`), alineado al
desplazamiento del flujo, y nunca se presentan como datos. `copy_to` copia el flujo entero en
trozos de 1 MiB y devuelve los bytes por estado y la lista de tramos no fiables (hasta 10 000,
unidos si son contiguos en bytes y en LBA; los recuentos son siempre exactos).

**Compresión.** El flujo se divide en unidades de 16 clústeres. Por cada unidad: si algún clúster
no tiene mapa, toda la unidad es `unmapped`; si todos son dispersos, ceros (`sparse`); si todos
están en disco, la unidad está sin comprimir y se lee tal cual; si no, sus clústeres en disco (en
orden de VCN) son un flujo LZNT1 que se descomprime con el límite de la unidad (64 KiB como mucho);
si termina antes, el resto de la unidad son ceros del formato. Una unidad con un sector no fiable
entre sus clústeres comprimidos se descomprime hasta el primer bloque afectado: lo anterior es
`data` y el resto de la unidad `unreadable`. Un error de descompresión deja `data` los bloques
completos anteriores y `corrupt` el resto.

## LZNT1 ([MS-XCA] §2.5)

Bloques con cabecera de 16 bits (12 bits de tamaño de datos - 1, firma, bit de comprimido); bloque
sin comprimir copiado tal cual; comprimido en grupos de un byte de indicadores y 8 elementos
(literal o referencia de 16 bits cuya división desplazamiento/longitud depende de la posición en el
bloque: desplazamiento con los bits de `posición - 1`, entre 4 y 12). Límites: ningún bloque produce
más de 4096 bytes (`lznt1_chunk_overflow`); toda referencia apunta dentro de lo ya descomprimido
**en ese bloque** (`lznt1_displacement`); un bloque que se sale de la entrada es `lznt1_truncated`;
la salida nunca supera el límite (`lznt1_output_limit`). Ante un error, la salida conserva solo los
bloques completos anteriores. Comportamiento fijado con el oráculo de Windows: un bloque corto
seguido de otro se completa con ceros hasta 4096; la firma de la cabecera no se comprueba (Windows
tampoco); una cabecera 0 o un byte final suelto terminan la entrada.

**Oráculo independiente.** `tests/generators/gen_lznt1_vectors.py` (solo Windows, sin
administrador) llama por ctypes a `RtlCompressBuffer` y `RtlDecompressBuffer` de `ntdll.dll`
(COMPRESSION_FORMAT_LZNT1, motores estándar y máximo) con entradas deterministas (xorshift con
semilla fija; texto, alfabetos de 4 y 16 símbolos, aleatorio, rachas de bytes, periodo 3, mezcla;
de 1 byte a 3 unidades de compresión + 123 bytes), comprueba la ida y vuelta con Windows y guarda en
`tests/vectors/lznt1/` cada vector en hexadecimal y un `manifest.toml` con tamaño y SHA-256 de los
datos sin comprimir y comprimidos; 9 vectores más construidos a mano fijan los casos límite con el
resultado de `RtlDecompressBuffer` (6 correctos y 3 errores). Los vectores están versionados: la CI
no necesita Windows.

## Ficheros borrados (#23, ADR-027)

Módulos `deleted` (descubrimiento, clasificación y combinación), `residual` (entradas residuales de
los índices), `bitmap` (`$Bitmap`) y `recover`.

**Descubrimiento** (`scan_deleted(volumen, opciones, observador)`, pasada 1 por la MFT):

| Registro | Resultado |
|---|---|
| Base, no en uso, registro FILE válido con algún `$FILE_NAME` (en el propio registro o por su `$ATTRIBUTE_LIST`) | entrada borrada (fichero o directorio) |
| Base, no en uso, sin `$FILE_NAME` | registro libre (solo se cuenta) |
| Extensión no en uso | no es una entrada: se une a su base por la lista (holgura de secuencia de ADR-026) o, si la lista del base falta o no se puede leer, por su referencia al base (número y secuencia igual o una más; `backref_extensions`) |
| Dañado, ilegible o fuera del origen | `problems` (como mucho 10 000; recuentos exactos): nunca es un borrado ni se recupera |
| En uso | nunca es un borrado |

De cada entrada: registro y secuencia actual, nombre (`$FILE_NAME` que no sea DOS si lo hay) con
su espacio de nombres y todos los nombres, indicador de directorio, padre (número y secuencia) y su
estado, ruta, tamaños (del `$DATA` principal o, sin él, del `$FILE_NAME`), fechas
(`$STANDARD_INFORMATION` o, sin él, del `$FILE_NAME`), extensiones y anomalías.

| Estado del padre | Regla |
|---|---|
| `live` | directorio en uso con la misma secuencia |
| `deleted` | directorio no en uso con la misma secuencia o una más (borrado a la vez o antes) |
| `orphan_out_of_range`, `orphan_unusable`, `orphan_sequence_mismatch`, `orphan_not_directory` | fuera de la MFT, registro no interpretable, reutilizado (otra secuencia) o no es un directorio |

La ruta se construye por la cadena de padres de la MFT (directorios vivos o borrados con la
secuencia coherente, como mucho 1024 niveles y 2 millones de directorios recordados) solo si llega
a la raíz (registro 5); si no, se muestra `<huérfano N>/nombre`. No usa los índices `$I30` (#22): los
índices aportan otra fuente de entradas (ver «Entradas residuales de los índices»).

**Dueños** (pasada 2): con los clústeres de los flujos borrados como consulta, se recorren los
registros en uso (base y extensiones; los de una extensión cuentan para su base) y se guardan solo
las intersecciones de los runs de **cualquier** atributo no residente con la consulta
(`ClaimIndex`, como mucho 4 millones de intersecciones de 24 B; si se llena,
`claims_truncated` y la asignación sigue saliendo de `$Bitmap`). Igual con los runs de los demás
borrados. Las dos pasadas son la única lectura de la MFT; la memoria depende de los borrados, no del
volumen.

**`$Bitmap`** (registro 6, `$DATA` sin nombre, un bit por clúster, el menos significativo
primero, 1 = asignado) se lee por el mapa de validez en páginas de 64 KiB (64 en memoria). Un byte
de un tramo no fiable, los clústeres más allá del mapa (`bitmap_short`) y todos si el registro no se
puede usar (`bitmap_unusable`, `bitmap_not_in_use`) son de asignación **desconocida**: nunca se
suponen libres.

**Clasificación de cada clúster** con contenido (dentro del tamaño inicializado; en un flujo
comprimido, unidades de compresión enteras), en este orden:

| Estado | Regla | ¿Original? |
|---|---|---|
| `owned` | en los runs de un fichero vivo (aunque `$Bitmap` diga libre); se nombra el dueño | no |
| `allocated` | asignado en `$Bitmap` sin dueño conocido | no |
| `unknown` | asignación desconocida | no |
| `shared_with_deleted` | libre, pero en los runs de otro borrado (no se sabe cuál escribió el último) | no |
| `free` | libre y sin otro dueño | sí |

**Recuperabilidad** de cada flujo `$DATA` (el principal da la del fichero; un directorio sin `$DATA`
no tiene):

| Valor | Regla |
|---|---|
| `full` | residente con el registro válido, vacío, o todos los clústeres con contenido `free` y ningún tramo sin mapa |
| `none` | ningún clúster con contenido `free` ni disperso, o disposición inutilizable (cifrado, extensiones incoherentes...) |
| `partial` | el resto |

Cada flujo lleva los recuentos (`clusters`, `free`, `owned`, `allocated`, `unknown`, `shared`,
`unmapped`, `sparse`), los tramos de clústeres con su estado (como mucho 4096 por flujo en el
listado; sin límite al recuperar), los dueños, los motivos tipados y un resumen
(`N de M clústeres posiblemente sobrescritos, asignados ahora a RUTA`). La evaluación no lee los
datos: los sectores ilegibles aparecen al recuperar.

**Recuperación** (`recover_stream`, `recovery_stream`): se evalúa el registro (centrado en él: los
demás borrados se recorren igual) y se abre el flujo con sus clústeres no libres marcados
(`DataStream::with_suspect_clusters`). Lo leído de ellos tiene el estado de tramo
`possibly_overwritten` (no fiable, **sin** patrón: son los bytes del disco); una unidad comprimida
con un clúster sospechoso es entera `possibly_overwritten`. El informe (`RecoveryReport`) tiene el
recuento por estado, los tramos no fiables, los tramos sospechosos en bytes con su LCN, estado y
dueño, y el resumen. Un registro en uso se copia tal cual; uno dañado o ilegible es
`RecordUnusable`; uno libre (sin `$FILE_NAME`) también.

**Entradas residuales de los índices** (`residual`, después de las dos pasadas; se omite al
recuperar un fichero y con `ScanOptions::residual = false`). Se lee el índice `$I30` de cada
directorio (registro base con el indicador de directorio, vivo o borrado; la raíz y los registros
16 en adelante) y se buscan cuatro tipos de restos:

| Origen (`EntryOrigin`) | Dónde | Qué se examina |
|---|---|---|
| `index_slack` | nodo asignado (raíz o `INDX` con su bit de `$BITMAP` a 1) de un directorio vivo | el espacio libre: entre los bytes en uso y los asignados de la cabecera de nodo |
| `index_free_node` | nodo `INDX` con su bit de `$BITMAP` a 0 de un directorio vivo | todo el nodo desde la primera entrada hasta los bytes asignados |
| `deleted_directory_index` | raíz y nodos `INDX` de un directorio borrado (registro no en uso) | todo el nodo |
| `index_stale` | parte en uso de un nodo asignado de un directorio vivo | las entradas cuyo registro no es interpretable como vivo con esa secuencia (ver «Contraste con la MFT»), salvo las que apuntan a un registro dañado o ilegible: son daños de un fichero vivo, no borrados (el listado los informa como `record_unusable`) |

Sin `$BITMAP` utilizable no se sabe qué nodos están libres: todos se tratan como asignados (su espacio
libre se examina y su parte en uso se contrasta con la MFT). Cada nodo `INDX` se lee entero por el
mapa de validez y solo se usa si **todos** sus bytes son fiables (un sector ilegible nunca se
interpreta: `nodes_unreadable`), tiene firma `INDX`, secuencia de actualización sin tolerancia, VCN
de la cabecera igual al de su posición y cabecera de nodo dentro de límites (si no, `nodes_invalid`).
La raíz sale del registro (ya validado con sus fixups). Hasta 2^20 nodos por índice y, en total,
`ScanOptions::max_residual` entradas (1 millón; `truncated`).

**Validación estricta de cada candidata** (`parse_candidates`, posiciones alineadas a 8 dentro del
tramo, sin leer nunca fuera de él):

1. Indicadores sin bits desconocidos y sin el de última entrada.
2. Longitud exactamente `alinear8(0x10 + clave) (+ 8 si tiene subnodo)` y dentro del tramo.
3. Clave de exactamente `0x42 + 2 × caracteres` bytes que es un `$FILE_NAME` válido (longitud de
   nombre no nula, espacio de nombres 0..3), sin unidades nulas ni `/`.
4. Padre de la clave = el directorio que se lee: mismo número y misma secuencia (en un directorio
   borrado, la de antes de liberarlo, una menos que la actual).
5. Referencia plausible: registro 16 o mayor, dentro de la MFT, distinto del propio directorio y
   con secuencia distinta de 0.

Lo que no cumple 1 a 4 no es una candidata (se avanza 8 bytes); lo que cumple 1 a 4 pero no 5 se
cuenta como **resto rechazado** (`rejected`). Una candidata aceptada salta su longitud. Una misma
entrada (registro, secuencia, directorio, nombre y origen) vista varias veces cuenta una vez
(`duplicates`).

**Combinación con la MFT** (`DeletedStatus`), según lo que es hoy el registro de la referencia:

| Registro | Resultado |
|---|---|
| En uso, base, misma secuencia | copia de una entrada viva: se descarta (`live_copies`); nunca se presenta un vivo como borrado |
| No en uso con la secuencia de la referencia o una más, y es un borrado de la MFT | se une a esa entrada: un origen más, el nombre del índice si no lo tenía y la referencia de índice (`index_refs`); aparece **una sola vez** con todos sus orígenes |
| No en uso con esa secuencia, sin entrada de la MFT (libre, sin `$FILE_NAME`) | `residual_record_free` |
| En uso con otra secuencia, extensión de otro fichero o liberado con otra secuencia | `residual_record_reused`: el contenido es de otro fichero, nada que recuperar |
| Dañado, ilegible, vacío, fuera del origen... | `residual_record_unusable` con el estado: no se conocen sus runs |

Las entradas solo residuales se agrupan por (registro, secuencia de la referencia): varios nombres
(p. ej. largo y DOS) son una entrada con el nombre que no es DOS. Llevan el nombre, el padre, los
tamaños, los atributos y las fechas de la clave del índice (`TIMES_FROM_INDEX` en la FFI), la ruta
por la cadena de padres de la MFT y recuperabilidad siempre `none` (nunca se presentan como
recuperables; `ntfs_recover_deleted` las rechaza).

**Lo que deja ntfs-3g (corpus).** Al quitar una entrada, ntfs-3g mueve las siguientes hacia el
principio del nodo y escribe la entrada final encima: el espacio libre conserva restos de las
entradas movidas, pero con la cabecera pisada (la referencia queda con bytes de la entrada final y
apunta fuera de la MFT). En `ntfs_deleted` hay 4 restos así (1 en el nodo de `/docs`, 3 en el de
`/docs/cartas`): los 4 se rechazan y ninguno se acepta. El índice del directorio borrado
`/docs/cartas` (registro 68) conserva su última entrada en uso, `sello.bin` (registro 103), que se
une a su entrada de la MFT (orígenes `mft_record` y `deleted_directory_index`). El corpus no tiene
nodos `INDX` libres ni entradas solo residuales; en las demás imágenes no hay restos que pasen la
validación ni rechazados. Esos casos se prueban con volúmenes sintéticos.

**Interfaz.** En la pestaña «Sistema de archivos», «Mostrar borrados» (`FsShowDeletedToggle`) lanza
la búsqueda (una vez por origen, con progreso) y rehace el árbol: los directorios borrados aparecen
bajo su padre como `nombre (borrado)` (`FsDeletedDir<registro>`), los borrados sin ruta en
`<borrados sin ruta>` (`FsDeletedOrphans`) y la lista de cada directorio añade sus borrados
(`FsDeleted<registro>`) y entradas solo residuales (`FsResidual<registro>_<secuencia>`) con su estado,
orígenes y la columna «Recuperable». «Recuperar…» (`FsRecoverButton`) solo se habilita en un fichero
borrado con su registro: pide el destino (con confirmación si existe), recupera con
`StartRecover` (escritura atómica, nunca sobre el origen) e informa de los bytes posiblemente
sobrescritos y de cada tramo con su dueño (aviso y registro de actividad).

## MFT, estados de registro y límites

- La MFT se localiza con el `$DATA` sin nombre del registro 0 en el LCN del arranque: en uso,
  registro base, no residente desde el VCN 0, runs que cubren exactamente `last_vcn + 1` y tamaño
  entre un registro y el volumen. Si no es utilizable se usa el registro 0 de `$MFTMirr`
  (`mft_record0_from_mirror`, compensada); si tampoco, `NtfsError::MftUnusable`.
- `record_count = data_size / tamaño de registro`, como mucho 2^32 (`MAX_RECORDS`). Avisos:
  `mft_lcn_mismatch`, `mft_size_not_multiple`, `mft_extended` (la MFT se completó con las
  extensiones de `$MFT`) y `mft_incomplete` (ni con ellas se cubre toda la MFT).
- Estados (`RecordState`): `in_use`, `not_in_use` (borrado o libre: `scan_deleted` los distingue),
  `empty`, `uninitialized` (más allá del tamaño inicializado: no se lee), `unmapped` (sin run o en
  un run disperso), `outside_source` (volumen truncado), `unreadable`, `damaged`.
- Enumeración por lotes de hasta 64 registros contiguos en disco; un registro que cruza dos runs se
  lee por partes. Cada registro se clasifica por separado: uno dañado o ilegible no impide leer los
  demás.

## `$MFTMirr` y `$Volume`

Se comparan con la MFT tantos registros como dice el `$DATA` del registro 1 (normalmente 4, como
mucho 64), en la posición de sus runs; si el registro 1 no es utilizable, 4 registros desde el LCN
del arranque (`mftmirr_record_unusable`). La comparación excluye la matriz de la secuencia de
actualización (el USN puede diferir entre copias). Cada discrepancia es `mftmirr_mismatch`
(compensada) con su tipo: `differs`, `mirror_invalid`, `mft_invalid` o `both_invalid`. La lectura de
la MFT no cambia. La etiqueta (`$VOLUME_NAME`) y la versión (`$VOLUME_INFORMATION`) salen del
registro 3; si no es utilizable, `volume_record_unusable` (aviso).

## Confianza

La del volumen es la peor gravedad de sus anomalías (escala de ADR-022): avisos sin efecto,
compensadas = `ValidWithFixes` (copia del arranque usada o dañada, registro 0 desde la copia,
discrepancias de `$MFTMirr`), sospechosas = `Suspicious` (tamaño de sector distinto, volumen
truncado, MFT incoherente). Los daños de registros concretos no cambian la confianza del volumen:
son el estado de cada registro.

## Pruebas

- Unitarias (`core/mr-fs-ntfs/src`, 33): cada campo del arranque fuera de rango, codificaciones de
  clúster y de registro, avisos; fixups correctos, desajuste en cada tramo, USA desplazada o con otro
  número de elementos, `BAAD`, otra firma, registro truncado, cabecera fuera de límites, registro no
  en uso; iteración de atributos (residentes, no residentes, nombres) y cada límite; runs con signo,
  dispersos, cada error y desbordamiento; lectura de tramos con sectores ilegibles; mapa de la MFT
  con registros partidos entre runs; volúmenes sintéticos mínimos (`testvol.rs`, construidos según
  la disposición pública, con clústeres de 512 B, 1 KiB y 4 KiB) con arranque principal dañado,
  ilegible o con la copia desplazada, copia dañada, distinta, ilegible o fuera, registros dañados
  (fixups, `BAAD`, tamaño), sectores de la MFT ilegibles con `FaultyDevice` (también en un registro
  partido), registro 0 desde la copia, `$MFTMirr` discrepante o ilegible, MFT incompleta y origen
  truncado. Fuzzing determinista: 20 000 arranques, 20 000 registros, 50 000 listas de runs y 400
  volúmenes mutados (sin pánicos, salida acotada).
- #21 (`core/mr-fs-ntfs/src`, 21 unitarias más): valores tipados y cada límite; entradas de
  `$ATTRIBUTE_LIST`; runs de una extensión; reglas de disposición de flujos; LZNT1 contra los 26
  vectores de Windows, límite de salida, bloques mal formados, ida y vuelta con un compresor de
  pruebas y fuzzing de 30 000 entradas; volúmenes sintéticos (`stream_tests.rs`) con residentes,
  ADS, dispersos con tamaño inicializado menor, sectores ilegibles con `FaultyDevice`, origen
  truncado, ficheros comprimidos (unidad comprimida, sin comprimir, dispersa y final parcial),
  sector ilegible y bloque dañado dentro de una unidad comprimida, `$ATTRIBUTE_LIST` residente y no
  residente, borrados con secuencias incrementadas, extensiones ajenas, reutilizadas, ilegibles, con
  lista propia, fuera de la MFT, solapadas, repetidas o con hueco, errores de flujo, extensiones de
  `$MFT` y fuzzing de 600 volúmenes con registros mutados.
- #23, entradas residuales (`core/mr-fs-ntfs/src/residual_tests.rs`, 7, con
  `testvol::residual_volume`): candidatas estrictas (entrada completa, con subnodo, cortada por el
  tramo, otro directorio, directorio borrado con su secuencia, tramos fuera del búfer y cada
  entrada mal formada de `testvol::malformed_slack`); espacio libre con un borrado dos veces, un
  registro reutilizado, uno dañado, una copia de una entrada viva y restos mal formados; nodo libre
  válido con un borrado, un registro libre y nombres DOS y largo de un registro dañado; nodo libre
  con un sector ilegible (`FaultyDevice`: nunca se interpreta) y nodo libre con fixups rotos; raíz
  de índice de un directorio borrado; combinación con la MFT (una entrada por fichero con todos
  sus orígenes, estados, recuperabilidad `none` de las solo residuales, nombre largo preferido,
  fechas de la clave, ningún vivo); búsqueda opcional, omitida con `focus` y con límite; entradas
  caducadas de `tree_volume` (reutilizado sí; registro dañado no; fuera de la MFT rechazada);
  fuzzing de 5000 tramos de candidatas (aleatorios y mutados) y 300 volúmenes con los nodos
  mutados (sin pánicos, sin vivos como borrados, sin repetidos, nada recuperable sin registro).
  Corpus (`ntfs_deleted_corpus.rs`, 2 más): lo que muestra el corpus (arriba) en las 10 imágenes,
  cada borrado del manifiesto exactamente una vez y ningún vivo; y un sector ilegible en el nodo
  del directorio borrado (sello.bin queda solo con la MFT). `mr-api` (1: borrados con residuales
  y `ntfs_recover_deleted` que rechaza vivos, reutilizados, libres, dañados y otra secuencia sin
  crear la salida), `mr-ffi` (3: estructuras, tarea de borrados con estados 16-19, orígenes y
  textos, tarea de recuperación con la política de salida), C# (`NtfsTests`: 3 más con el corpus),
  CLI (`ntfs-deleted --json` y texto con los recuentos de los índices y los orígenes) e interfaz
  (`NtfsDeletedUiTests`).
- #23 (`core/mr-fs-ntfs/src`, 13 unitarias más; `deleted_tests.rs` y `bitmap.rs`): bits de
  `$Bitmap` y sus límites (bytes no fiables, fuera del mapa, desbordamiento) con fuzzing de 5000
  consultas contra un modelo; páginas leídas una vez; `$Bitmap` ilegible, corto o no en uso;
  `ClaimIndex` (solo intersecciones, límite); clasificación de clústeres contra un modelo (500
  casos); volumen sintético con borrados íntegro, parcial con dueño vivo, asignado sin dueño,
  residente con ADS, directorio borrado con hijo, huérfanos (padre no directorio y reutilizado),
  clúster compartido por dos borrados, extensión por la lista y por referencia al base, registro
  libre y dañado; recuperación con `possibly_overwritten` recortado al tamaño, sector ilegible en un
  clúster libre, unidad comprimida con un clúster sospechoso; cancelación, progreso, límite de
  entradas y foco; fuzzing de 300 volúmenes con registros mutados.
- Corpus (`core/mr-fs-ntfs/tests/ntfs_corpus.rs`, 5, `--ignored`): ver `PRUEBAS.md`.
- Corpus de borrados (`core/mr-fs-ntfs/tests/ntfs_deleted_corpus.rs`, 2, `--ignored`): ver
  `PRUEBAS.md`. 439 flujos
  con el SHA-256 del manifiesto (419 vivos, 12 borrados, 3 sobrescritos con `recovered_sha256` y 5
  flujos alternativos), con tamaño, residencia, runs, dispersos y tamaño inicializado.
- #22 (`core/mr-fs-ntfs/src/dir_tests.rs`, 13, y `upcase`, 2): volumen sintético con árbol
  (`testvol::tree_volume`: raíz con dos nodos `INDX`, `/Docs` con un nodo hoja, nombres DOS, enlace
  duro, sustituto suelto, entradas a registros libres, reutilizados, dañados y fuera de la MFT,
  huérfanos de cada tipo, `$UpCase` opcional): listados con cada estado, DOS, enlaces duros, Unicode,
  rutas sin distinguir mayúsculas (con `$UpCase` y con plegado simple), `path_of`, ciclos y padres
  reutilizados; cada daño de nodo (fixups, USA, VCN, firma, cabecera, sector ilegible, bit de
  `$BITMAP`, puntero fuera de rango, ciclo, `$BITMAP` ausente) con el resto listado y lo perdido como
  huérfano; nodos de 1 KiB con VCN de 512 B; ciclo de directorios, profundidad, límite de nodos y
  cancelación; reglas de cabecera y entradas; fuzzing de 20 000 nodos y 300 volúmenes mutados.
  Corpus (`ntfs_corpus.rs`, 3 más): árbol de los 10 manifiestos, cada ruta viva resuelta con su
  SHA-256 y un nodo `INDX` ilegible del corpus. `mr-api` (4: listado, árbol con cancelación, fichero
  como origen, copia con la política de salida), `mr-ffi` (5), C# (`NtfsTests`), CLI (`ntfs-ls -r
  --json` de las 10 imágenes, `ntfs-cat RUTA`) e interfaz (`NtfsUiTests`).
- CLI: `cli/tests/cli.rs` (origen sin NTFS, argumentos de `ntfs-cat`/`ntfs-stat`/`ntfs-ls`) y
  `cli/tests/ntfs_corpus.rs` (`ntfs-info --json` de las 10 imágenes, `ntfs-cat` y `ntfs-stat --json`
  de 116 flujos, imagen truncada con código 3 y patrón solo en los bytes no fiables, salidas
  rechazadas).

## Limitaciones

- La salida de `ntfs-cat` se compara con el origen por ruta canónica: un enlace duro al origen
  no se detecta (la escritura atómica por renombrado no modificaría el origen, pero sí sustituiría
  ese nombre).
- Los flujos cifrados (EFS) no se leen (`stream_encrypted`); su lectura en bruto es FS-05 (M4).
- Solo se abren flujos `$DATA` (principal y ADS); otros atributos no residentes se leen
  internamente cuando hace falta (`$ATTRIBUTE_LIST`).
- La lectura de un fichero borrado sigue sus runs tal como están: si los clústeres se han
  reutilizado, el contenido es el actual del disco. `ntfs-recover` lo marca como
  `possibly_overwritten`; `ntfs-cat` no evalúa (solo avisa de que el registro no está en uso).
- Borrados: una escritura en un clúster libre sin asignarlo no se detecta (el contenido se da por
  original); de dos borrados que reclaman el mismo clúster libre no se sabe cuál es el original
  (ninguno es `full`); las rutas salen de la cadena de padres de la MFT, no de los índices; el
  corpus no tiene borrados con `$ATTRIBUTE_LIST`, comprimidos ni huérfanos (solo sintéticos).
- Entradas residuales: un resto con la cabecera pisada (lo habitual en ntfs-3g) se rechaza aunque
  su clave sea legible: no se adivina la referencia; tampoco se buscan entradas desalineadas ni en
  nodos con algún sector ilegible o con fixups que no cuadran (la parte legible no se usa). Una
  entrada solo residual no tiene datos recuperables: los runs de su registro se perdieron o son de
  otro fichero (la búsqueda de contenido sin registro es del escaneo completo, M6). Los índices de
  directorios que no son `$I30` no se examinan. El corpus de ntfs-3g no tiene nodos libres ni
  entradas solo residuales (solo sintéticos); los volúmenes de Windows, que no mueven las entradas
  igual, no se han probado.
- El corpus de ntfs-3g no tiene ficheros comprimidos ni `$ATTRIBUTE_LIST`: la compresión se
  comprueba con los vectores de Windows y volúmenes sintéticos, y las extensiones con volúmenes
  sintéticos.
- No se reparan registros desde `$MFTMirr` salvo el registro 0 para localizar la MFT.
- Solo se interpretan índices `$I30` (nombres de fichero); `$SII`, `$SDH`, `$O`, `$Q` y `$R` no.
- El corpus de ntfs-3g tiene índices de uno o dos niveles y sin nombres DOS: los árboles B profundos,
  los nodos menores que un clúster y los nombres 8.3 se comprueban con volúmenes sintéticos.
- La copia de un fichero desde la interfaz o la FFI hereda la limitación de `create_output`: un enlace
  duro a la imagen no se detecta.
- El plegado simple (sin `$UpCase` utilizable) no coincide con Windows en todos los caracteres.
- No se busca la MFT si el registro 0 falla en las dos copias (lo hará el escaneo de M6).
- El corpus es de ntfs-3g: sin volúmenes de Windows, sin clústeres grandes ni sectores de 4 KiB
  reales (cubiertos solo por las pruebas sintéticas).
