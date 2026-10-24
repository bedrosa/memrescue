# Copia del particionado (`memrescue-partition-backup`, versión 1)

Issue #18 (PT-10). Formato **propio** de MemRescue para guardar y volver a cargar el particionado
detectado en un origen. Implementación en `core/mr-part/src/backup/` (`capture`, `LayoutBackup`,
`compare`), API en `mr-api` (`partition_backup`, `save_partition_backup`, `load_partition_backup`,
`compare_partition_backup`) y CLI (`partitions --save-backup`, `find-partitions --save-backup`,
`partition-backup-check`).

## Qué guarda y para qué

Una copia contiene a la vez:

1. La **identidad** del origen: tamaño en bytes, tamaño de sector con que se abrió y número de sectores
   (la descripción es informativa).
2. El **particionado interpretado completo**, tal como lo devuelve `read_partition_table`: esquema,
   particiones con su origen, tipo, nombre, confianza y anomalías, anomalías de la tabla y, con GPT, la
   lectura de las dos copias (estado, campos de cada cabecera), la copia elegida, el desplazamiento de
   la imagen, los sectores que faltan al final y las particiones GPT con su GUID único y atributos.
3. Los **bytes** de los sectores de tabla leídos (regiones), cada uno con su SHA-256: el sector de la
   LBA 0, cada EBR de la cadena y, con GPT, la cabecera y la tabla de entradas de **cada** copia en su
   posición de la imagen. No es solo la interpretación: es una copia real de los bytes.
4. Opcionalmente, el resultado completo de la **búsqueda exprés** (`find-partitions`): volúmenes
   encontrados con sus indicadores, tablas encontradas, anomalías, estadísticas y opciones.

Usos: conservar el estado de un disco antes de trabajar con él, documentar un caso y comprobar después
si el particionado de un origen ha cambiado. **Restaurar en disco queda fuera de alcance**: las
escrituras están bloqueadas hasta M12 (ADR-006). Cargar una copia solo la valida y la compara con un
origen de solo lectura; nunca se escribe nada en ningún disco ni imagen.

## Documento

Texto UTF-8 sin BOM con un único objeto JSON (RFC 8259, subconjunto estricto descrito más abajo). Lo
escribe MemRescue con dos espacios de sangría y salto de línea `\n` final; los espacios entre elementos
no tienen significado.

```json
{
  "format": "memrescue-partition-backup",
  "version": 1,
  "body_sha256": "<64 dígitos hexadecimales>",
  "body": {
    "created_by": "MemRescue 0.2.0",
    "source": { ... },
    "layout": { ... },
    "regions": [ ... ],
    "search": null
  }
}
```

| Campo | Tipo | Significado |
|---|---|---|
| `format` | cadena | Siempre `memrescue-partition-backup`. Otro valor (o ausente): no es una copia |
| `version` | entero | Versión del formato: **1**. Otra versión se rechaza (no se intenta interpretar) |
| `body_sha256` | cadena | SHA-256 de la forma canónica de `body` (ver «Integridad») |
| `body` | objeto | Contenido de la copia |

### `body`

| Campo | Tipo | Significado |
|---|---|---|
| `created_by` | cadena | Programa y versión que la escribió |
| `source` | objeto | `description` (cadena, ≤ 4096 caracteres, informativa), `size_bytes`, `sector_size`, `sector_count` (enteros). `sector_count` = `size_bytes / sector_size`; `sector_size` es una potencia de 2 entre 512 y 65536 |
| `layout` | objeto | Particionado interpretado (`PartitionTable`, abajo) |
| `regions` | lista | Bytes de los sectores de tabla (abajo), como mucho 1029 |
| `search` | objeto o `null` | Resultado de la búsqueda exprés (`SearchResult`, abajo); debe tener el mismo `sector_size` y `sectors` que `source` y opciones válidas |

### Regiones

```json
{ "kind": "gpt_primary_entries", "offset": 1024, "length": 16384, "state": "data",
  "sha256": "...", "data": ["<128 dígitos hex>", "..."] }
{ "kind": "gpt_primary_header", "offset": 512, "length": 512, "state": "unreadable",
  "unreadable_lbas": [1] }
```

| Campo | Significado |
|---|---|
| `kind` | `mbr` (sector de la LBA 0), `ebr`, `gpt_primary_header`, `gpt_primary_entries`, `gpt_backup_header`, `gpt_backup_entries` |
| `offset`, `length` | Posición y longitud **en bytes** del origen (`1 ≤ length ≤ 4 MiB + 64 KiB`; la región cabe en `size_bytes`) |
| `state` | `data`: todos los sectores eran fiables; `unreadable`: alguno no lo era |
| `sha256` | (`data`) SHA-256 de los bytes |
| `data` | (`data`) Bytes en hexadecimal en minúsculas, 64 bytes (128 dígitos) por cadena; la última puede ser más corta y no vacía; en total, exactamente `length` bytes |
| `unreadable_lbas` | (`unreadable`) LBA del dispositivo (sectores de `source.sector_size`) no fiables, dentro de la región y sin repetir. **No se guarda contenido**: el relleno de un sector ilegible nunca son datos |

Qué regiones se guardan, en este orden y sin repetir `(kind, offset, length)`:
- `mbr`: la LBA 0 (un sector del dispositivo), siempre.
- `ebr`: cada EBR leído de la cadena (esquemas MBR e híbrido), en orden de cadena.
- Con lectura GPT (esquemas GPT e híbrido), por cada copia con posición en la imagen: la cabecera (un
  sector de la GPT, que puede ser de otro tamaño que el del dispositivo) y, si tiene campos legibles y
  su tabla ocupa entre 1 byte y 4 MiB, la tabla de entradas redondeada a sectores de la GPT, situada con
  el desplazamiento de su propia cabecera (`MyLBA` frente a la posición leída).
- Las posiciones que quedan fuera del origen (imagen recortada o desplazada) no se guardan.

### `layout` y tipos comunes

Cada estructura se escribe como objeto con **todos** sus campos, también los nulos, con los mismos
nombres que en `mr-part`. Al leer se exigen exactamente esos campos. Los enteros son exactos de 64 bits
(con signo donde el tipo lo es); `null` representa la ausencia de un valor opcional.

| Objeto | Campos |
|---|---|
| `PartitionTable` | `scheme` (`none`, `mbr`, `gpt`, `hybrid`), `sector_size`, `partitions` (lista de `Partition`), `anomalies` (lista de `Anomaly`), `gpt` (`GptRead` o `null`) |
| `Partition` | `origin` (`Origin`), `start_lba`, `sector_count`, `kind` (`PartitionKind`), `name` (cadena o `null`), `active`, `confidence` (`Confidence`), `anomalies` |
| `Origin` | `{"type": "Primary", "slot"}`, `{"type": "Logical", "ebr_lba", "slot"}` o `{"type": "Gpt", "index"}` |
| `PartitionKind` | `{"table": "mbr", "type": <byte>}` o `{"table": "gpt", "type": <GUID>}` |
| `Confidence` | `{"level": "valid"}`, `{"level": "valid_with_fixes" \| "suspicious", "reasons": [cadenas]}` o `{"level": "invalid", "reason": cadena}` |
| `GptRead` | `sector_size`, `device_sector_size`, `primary` y `backup` (`CopyReport`), `selected` (`primary`, `backup` o `null`), `shift` (con signo: LBA disco = LBA imagen + `shift`), `trailing_sectors_missing`, `partitions` (lista de `GptPartition`), `anomalies`, `confidence` |
| `CopyReport` | `header` (`valid`, `crc`, `invalid`, `missing`, `outside_image`, `unreadable`), `entries` (`valid`, `crc`, `outside_image`, `unreadable`, `unchecked`), `disk_lba`, `image_lba` (o `null`), `fields` (`GptHeader` o `null`) |
| `GptHeader` | `revision`, `header_size`, `header_crc32`, `my_lba`, `alternate_lba`, `first_usable_lba`, `last_usable_lba`, `disk_guid`, `entries_lba`, `entry_count`, `entry_size`, `entries_crc32` |
| `GptPartition` | `partition` (`Partition`), `unique_guid`, `attributes` (64 bits sin signo) |

**GUID**: forma textual habitual `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` en minúsculas; los tres primeros
grupos son los bytes 0-3, 4-5 y 6-7 del disco en little endian y los dos últimos, los bytes 8-15 en
orden. Es biyectiva con los 16 bytes del disco.

**Anomalías** (`Anomaly`): objeto con `family` (`device`, `mbr` o `gpt`), `code` (el código estable de
la CLI y de los manifiestos, comprobado al leer), `type` (la variante) y los campos de la variante:

| Familia | `type` y campos |
|---|---|
| `device` | `UnreadableSector {lba}` |
| `mbr` | `NoTable {lba, reason}`, `EbrInvalid {lba, reason}`, `Empty {lba}`, `SignatureMissing {lba}`, `Superfloppy {lba, fs}`, `InvalidBootIndicator {lba, slot, value, used}`, `MultipleActive {lba, slots}`, `MultipleExtended {lba, slots}`, `ZeroLength {lba, slot, kind}`, `OutsideDisk {lba, origin, start}`, `PartiallyOutsideDisk {lba, origin, end, disk_sectors}`, `Overlap {lba, first, second}`, `UnknownType {lba, origin, kind}`, `LogicalOutsideExtended {lba, origin}`, `ExtraDataEntries {lba, count}`, `EbrLoop {lba, target}`, `LinkOutsideDisk {lba, target}`, `LinkOutsideExtended {lba, target}`, `ChainLimit {lba, limit}`, `ProtectiveStart {lba, start}`, `ProtectiveSizeMismatch {lba, declared, expected}`, `Hybrid {lba}`, `HybridMismatch {lba, slot}` |
| `gpt` | `HeaderMissing {copy, lba}`, `HeaderOutsideImage {copy, disk_lba}`, `HeaderCrc {copy, lba, stored, computed}`, `HeaderSize {copy, lba, size}`, `RevisionUnsupported {copy, lba, revision}`, `RevisionMinor {copy, lba, revision}`, `MyLbaMismatch {copy, lba, my_lba, expected}`, `AlternateLbaMismatch {copy, lba, alternate_lba}`, `UsableRangeInvalid {copy, lba, first, last}`, `EntriesLocationInvalid {copy, lba, entries_lba}`, `EntryArrayLimits {copy, lba, entry_count, entry_size}`, `EntriesCrc {copy, lba, stored, computed}`, `EntriesOutsideImage {copy, disk_lba}`, `BackupInUse {primary_header, primary_entries}`, `CopiesDisagree {field}`, `ImageShifted {offset}`, `ImageTruncated {missing}`, `UsableRangeOutsideImage {lba, first, last}`, `SectorSizeMismatch {device, used}`, `EntryInverted {index, first, last}`, `EntryOutsideUsable {index}`, `EntryOutsideImage {index}`, `EntryOverlap {index, other}`, `DuplicateUniqueGuid {index, other}`, `NameInvalid {index}` |

Valores enumerados: `reason` = `blank`, `no_entry_inside_disk`, `invalid_boot_indicator`,
`multiple_extended`, `gpt_header`, `outside_device`; `fs` = `NTFS`, `exFAT`, `FAT32`, `FAT`; `copy` =
`primary`, `backup`; `field` = `disk_guid`, `usable_range`, `entry_count`, `entry_size`,
`entries_crc32`, `location`; `primary_header` y `primary_entries` como en `CopyReport`. El significado
de cada anomalía está en `formatos/gpt.md` y en ADR-022.

### `search` (opcional)

| Objeto | Campos |
|---|---|
| `SearchResult` | `status` (`complete`, `budget_exhausted`, `cancelled`), `sector_size`, `sectors`, `volumes`, `tables`, `anomalies`, `shift_bytes` (o `null`), `stats`, `probed`, `pending` (5 enteros, P0 a P4), `volume_limit_reached`, `options` |
| `FoundVolume` | `fs` (`ntfs`, `fat12`, `fat16`, `fat32`, `exfat`), `start_byte`, `length_bytes`, `fs_sector_size`, `primary`, `backup`, `structures`, `entry`, `self_location`, `aligned` (`yes`, `no`, `unverifiable`, `outside_image`, `not_applicable`), `primary_byte`, `backup_byte` (o `null`), `declared_start`, `serial`, `tier` (`p0` a `p4`), `origin`, `table_partition` y `found_table` (índice o `null`), `confidence`, `anomalies`, `score` |
| Anomalías de volumen | `{"type", ...}`: `PrimaryDamaged {byte}`, `PrimaryUnreadable {lba}`, `PrimaryOutsideImage {byte}`, `BackupMissing {byte, compensated}`, `BackupUnreadable {lba, compensated}`, `BackupOutsideImage {byte, compensated}`, `BackupMismatch {byte}`, `BeyondImageEnd {missing_bytes}`, `SectorSizeMismatch {fs, device}`, `Unaligned {byte}`, `ShiftDetected {delta_bytes}`, `SelfLocationMismatch {declared_byte, actual_byte}`, `Overlap {other_start}`, `Unconfirmed {}` |
| `FoundTable` | `kind` (`mbr`, `gpt`), `lba`, `declared_lba`, `entries` (`start_byte`, `length_bytes`, `mbr_type`, `extended`) |
| Anomalías de búsqueda | `ImageShifted {delta_bytes, sector_size, evidence}`, `MbrDisplaced {lba}` |
| `stats` | `reads`, `bytes`, `cache_hits`, `unverifiable_sectors`, `reads_by_tier` (5 enteros) |
| `options` | `max_reads`, `max_bytes`, `max_volumes`, `probe_window`, `start_grid_bytes`, `end_grid_bytes`, `cylinders`, `probes_per_gap`, `families` (límites de `formatos/busqueda-particiones.md`) |

Significado de cada campo en `formatos/busqueda-particiones.md`.

## Integridad

- Cada región `data` lleva el SHA-256 de sus bytes.
- `body_sha256` es el SHA-256 de la **forma canónica** de `body`: el mismo árbol JSON escrito sin
  espacios (`{"k":v,...}`, `[a,b]`), con los miembros en el orden del fichero, enteros en decimal sin
  ceros a la izquierda, y cadenas con solo estos escapes: `\"`, `\\` y `\u00XX` (minúsculas) para
  U+0000-U+001F y U+007F; el resto de caracteres, tal cual en UTF-8. Cambiar espacios o saltos de
  línea (también CRLF) no altera la suma; cambiar cualquier valor del cuerpo, sí.

La suma detecta daños y ediciones accidentales; no es una firma: quien edite el cuerpo a propósito
puede recalcularla. Aun así, los bytes editados de una región se detectan por su propia suma.

## Lectura estricta

Se rechaza (sin interpretar nada) un documento que:
- supere **32 MiB**, no sea UTF-8 o empiece por BOM;
- no sea JSON del subconjunto: solo enteros (`-?(0|[1-9][0-9]*)`, ≤ 20 dígitos; sin fracciones ni
  exponentes), cadenas con los escapes de RFC 8259 (sustitutos UTF-16 emparejados, sin caracteres de
  control sin escapar, ≤ 64 KiB), `true`, `false`, `null`, listas (≤ 2^20 elementos) y objetos
  (≤ 256 miembros, sin claves repetidas); anidamiento ≤ 32; nada después del valor principal
  (un fichero truncado siempre falla aquí);
- tenga otro `format` o `version`;
- no cuadre `body_sha256`;
- tenga un campo ausente, desconocido, de otro tipo o fuera del rango de su tipo (`u8`, `u32`, `u64`,
  `i64`), una variante o valor enumerado desconocido o un `code` que no corresponda a su anomalía;
- tenga una región incoherente (longitud, datos, suma, fuera del origen, repetida, sectores ilegibles
  fuera de ella) o una identidad o búsqueda incoherentes.

El error indica la línea y la columna (sintaxis) o la ruta del campo (`$.body.regions[0].sha256`).

## Ida y vuelta

Leer lo escrito devuelve un valor **igual** (`==`) al capturado, incluida la búsqueda, y volver a
escribirlo produce el mismo texto byte a byte. El compilador garantiza que la escritura y la lectura
cubren todas las variantes y todos los campos de las estructuras (las mismas listas generan las dos
direcciones).

## Comparación con un origen

`compare` (CLI `partition-backup-check <copia> <imagen>`) vuelve a capturar el origen con las mismas
reglas y, sin escribir nada, informa de:

| Código | Diferencia |
|---|---|
| `source_size`, `sector_size` | Tamaño del origen o tamaño de sector distintos |
| `region_changed` | Los bytes de una región de la copia, releídos en la misma posición, ya no coinciden (o ahora son ilegibles, o la posición queda fuera del origen), o el origen tiene una región de tabla que la copia no tenía |
| `scheme_changed`, `table_sector_size` | Esquema o tamaño de sector de la tabla distintos |
| `gpt_selected_copy`, `image_shift` | Copia GPT en uso o desplazamiento distintos |
| `partition_removed`, `partition_added`, `partition_changed` | Particiones emparejadas por su entrada de tabla (`origin`); en las cambiadas, los campos que difieren (inicio, tamaño, tipo, nombre, activa, confianza, anomalías, GUID único, atributos) |
| `anomaly_removed`, `anomaly_added` | Anomalías de la tabla que desaparecen o aparecen |

La búsqueda exprés guardada no se repite al comparar (se conserva como información). La CLI termina con
código 4 si hay diferencias.

## Limitaciones (versión 1)

- Sin restauración: escribir la copia en un disco o imagen llegará con la capa de cambios (M12,
  ADR-006, PT-07 y PT-09).
- Las regiones con algún sector ilegible no guardan ningún byte (tampoco los de sus sectores sanos).
- Con un MBR clásico la GPT no se lee (el MBR manda, ADR-022) y por tanto no se guarda.
- APM (#17) no está incluido: se añadirá como variantes nuevas (cambio de versión si altera el
  significado de lo existente).
- FFI e interfaz gráfica pendientes: por ahora, API de Rust y CLI.
