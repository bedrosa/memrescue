# NTFS: sector de arranque, MFT y registros FILE

Crate: `core/mr-fs-ntfs` (`mr_fs_ntfs::NtfsVolume`). Solo lectura. Issue #20 (M3), paridad FS-01
(parcial), ADR-025. Los atributos completos, los runs de cualquier atributo, los datos, LZNT1 y los
flujos alternativos son de #21; los índices y el árbol, de #22; los borrados, de #23.

## Especificación usada

Disposición pública del formato en disco de NTFS (documentación del controlador NTFS del núcleo
Linux y documentación pública del formato NTFS de libyal y del proyecto Linux-NTFS). Implementación
original; de la investigación de la referencia (`investigacion/dmde-fase1/05-algoritmos-verificados.md`
§5.1 y §5.3) solo se han tomado como hechos observados qué campos valida (sin traducir código).

## Interfaz

```rust
NtfsVolume::open(dev: Arc<dyn BlockDevice>) -> Result<NtfsVolume, NtfsError>
volume.info() -> &VolumeInfo            // arranque, MFT, $MFTMirr, etiqueta, versión, anomalías, confidence()
volume.record_count() -> u64            // data_size de $MFT / tamaño de registro
volume.read_record(n) -> Result<RecordOutcome, NtfsError>
volume.records() / records_in(rango)    // iterador (n, RecordOutcome) en orden, por lotes
```

- `mr_api::ntfs_open(&Source)` abre el volumen de un origen: imagen sin tabla, partición
  (`open_partition`) o volumen encontrado (`open_found_volume`). Errores en `ApiError::Ntfs`.
- CLI: `memrescue-cli ntfs-info <imagen> [--partition N | --found N | --range I:N] [--json]`
  (arranque usado y estado de la copia, campos, serie, etiqueta, versión, runs de `$MFT`, comparación
  con `$MFTMirr`, recuento de registros por estado y lista de dañados e ilegibles). Código de salida
  3 si algún registro tiene sectores ilegibles.
- FFI e interfaz: **pendientes** (#22, cuando aparezca el árbol de ficheros). `mr-ffi` solo traduce
  los nuevos errores (`MR_ERR_CORRUPT`, `MR_ERR_OUT_OF_RANGE` o el código de E/S).

Módulos y frontera con los siguientes issues:

| Módulo | Contenido | Lo amplía |
|---|---|---|
| `boot` | `parse_boot`, codificaciones de clúster y de tamaño de registro | - |
| `record` | `apply_fixups` (sirve también para `INDX`), `FileRecord`, cabecera, `FileReference` | #22 (INDX) |
| `attr` | iterador acotado de cabeceras de atributo (residente/no residente, nombre) | #21 (contenido) |
| `runs` | `decode_runs` con aritmética comprobada, `merge_contiguous` | #21 (datos, compresión, dispersos) |
| `mft` | `MftLayout` (tramos en disco de cada registro), `RecordOutcome`, `Records` | #22, #23 |
| `volume` | apertura, copia del arranque, `$MFTMirr`, `$Volume` | - |

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

**Runs** (mínimo para la MFT): nibble de longitud 1 a 8, de desplazamiento 0 a 8; longitud > 0;
LCN acumulado con signo, nunca negativo y con `LCN + longitud ≤ clústeres del volumen`;
desbordamientos comprobados; como mucho 32 768 runs.

## MFT, estados de registro y límites

- La MFT se localiza con el `$DATA` sin nombre del registro 0 en el LCN del arranque: en uso,
  registro base, no residente desde el VCN 0, runs que cubren exactamente `last_vcn + 1` y tamaño
  entre un registro y el volumen. Si no es utilizable se usa el registro 0 de `$MFTMirr`
  (`mft_record0_from_mirror`, compensada); si tampoco, `NtfsError::MftUnusable`.
- `record_count = data_size / tamaño de registro`, como mucho 2^32 (`MAX_RECORDS`). Avisos:
  `mft_lcn_mismatch`, `mft_size_not_multiple`, `mft_incomplete` (los runs del registro 0 no cubren
  toda la MFT: extensiones por `$ATTRIBUTE_LIST`, que esta versión no sigue).
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
- Corpus (`core/mr-fs-ntfs/tests/ntfs_corpus.rs`, 3, `--ignored`): ver `PRUEBAS.md`.
- CLI: `cli/tests/cli.rs` (origen sin NTFS) y `cli/tests/ntfs_corpus.rs` (`ntfs-info --json` de las
  10 imágenes, `--partition 1` en las particionadas).

## Limitaciones

- No se siguen las extensiones de `$MFT` en otros registros (`$ATTRIBUTE_LIST`): los registros no
  cubiertos por los runs del registro 0 son `unmapped` y se informa `mft_incomplete`.
- No se reparan registros desde `$MFTMirr` salvo el registro 0 para localizar la MFT.
- No se busca la MFT si el registro 0 falla en las dos copias (lo hará el escaneo de M6).
- El corpus es de ntfs-3g: sin volúmenes de Windows, sin clústeres grandes ni sectores de 4 KiB
  reales (cubiertos solo por las pruebas sintéticas).
