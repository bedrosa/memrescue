# NTFS: sector de arranque, MFT, registros FILE, atributos y datos

Crate: `core/mr-fs-ntfs` (`mr_fs_ntfs::NtfsVolume`). Solo lectura. Issues #20 y #21 (M3), paridad
FS-01, FS-03 y FS-04, ADR-025 y ADR-026. Los índices y el árbol son de #22; los borrados (qué datos
siguen disponibles y con qué fiabilidad), de #23.

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
```

- `mr_api::ntfs_open(&Source)` abre el volumen de un origen: imagen sin tabla, partición
  (`open_partition`) o volumen encontrado (`open_found_volume`). Errores en `ApiError::Ntfs`.
- `mr_api::ntfs_file`, `ntfs_open_stream`, `ntfs_read_stream` (con el estado de cada tramo) y
  `ntfs_copy_stream` (#21), por número de registro MFT y nombre de flujo; `check_output_path`
  valida el destino de una extracción (nunca el origen, nunca un dispositivo).
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
- FFI e interfaz: **pendientes** (#22, cuando aparezca el árbol de ficheros). `mr-ffi` solo traduce
  los nuevos errores (`MR_ERR_CORRUPT`, `MR_ERR_OUT_OF_RANGE` o el código de E/S).

Módulos y frontera con los siguientes issues:

| Módulo | Contenido | Lo amplía |
|---|---|---|
| `boot` | `parse_boot`, codificaciones de clúster y de tamaño de registro | - |
| `record` | `apply_fixups` (sirve también para `INDX`), `FileRecord`, cabecera, `FileReference` | #22 (INDX) |
| `attr` | iterador acotado de cabeceras de atributo (residente/no residente, nombre) | - |
| `values` | `$STANDARD_INFORMATION`, `$FILE_NAME`, `$OBJECT_ID`, `$VOLUME_INFORMATION`, entradas de `$ATTRIBUTE_LIST` | #22 (`$FILE_NAME` de los índices) |
| `runs` | `decode_runs`, `decode_extent`, `merge_contiguous`, `sparse_ranges` | - |
| `file` | `NtfsFile` (base + extensiones), `FileAnomaly`, `StreamLayout`, `StreamError`, `StreamIssue` | #23 (fiabilidad de los borrados) |
| `stream` | `DataStream`: lectura con estado por tramo, compresión, `copy_to` | #22, #23 |
| `lznt1` | descompresor [MS-XCA] | - |
| `mft` | `MftLayout` (tramos en disco de cada registro), `RecordOutcome`, `Records` | #22, #23 |
| `volume` | apertura, copia del arranque, `$MFTMirr`, `$Volume`, extensiones de `$MFT`, `file`, `open_stream` | - |

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

## MFT, estados de registro y límites

- La MFT se localiza con el `$DATA` sin nombre del registro 0 en el LCN del arranque: en uso,
  registro base, no residente desde el VCN 0, runs que cubren exactamente `last_vcn + 1` y tamaño
  entre un registro y el volumen. Si no es utilizable se usa el registro 0 de `$MFTMirr`
  (`mft_record0_from_mirror`, compensada); si tampoco, `NtfsError::MftUnusable`.
- `record_count = data_size / tamaño de registro`, como mucho 2^32 (`MAX_RECORDS`). Avisos:
  `mft_lcn_mismatch`, `mft_size_not_multiple`, `mft_extended` (la MFT se completó con las
  extensiones de `$MFT`) y `mft_incomplete` (ni con ellas se cubre toda la MFT).
- Estados (`RecordState`): `in_use`, `not_in_use` (borrado o libre: #23 los distinguirá),
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
- Corpus (`core/mr-fs-ntfs/tests/ntfs_corpus.rs`, 5, `--ignored`): ver `PRUEBAS.md`. 439 flujos
  con el SHA-256 del manifiesto (419 vivos, 12 borrados, 3 sobrescritos con `recovered_sha256` y 5
  flujos alternativos), con tamaño, residencia, runs, dispersos y tamaño inicializado.
- CLI: `cli/tests/cli.rs` (origen sin NTFS, argumentos de `ntfs-cat`/`ntfs-stat`) y
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
  reutilizado, el contenido es el actual del disco (#23 informará la fiabilidad).
- El corpus de ntfs-3g no tiene ficheros comprimidos ni `$ATTRIBUTE_LIST`: la compresión se
  comprueba con los vectores de Windows y volúmenes sintéticos, y las extensiones con volúmenes
  sintéticos.
- No se reparan registros desde `$MFTMirr` salvo el registro 0 para localizar la MFT.
- No se busca la MFT si el registro 0 falla en las dos copias (lo hará el escaneo de M6).
- El corpus es de ntfs-3g: sin volúmenes de Windows, sin clústeres grandes ni sectores de 4 KiB
  reales (cubiertos solo por las pruebas sintéticas).
