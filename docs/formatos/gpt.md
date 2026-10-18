# GPT (GUID Partition Table)

Módulo: `core/mr-part/src/gpt.rs` (`mr_part::gpt::read_gpt`). Solo lectura. Issue #13, paridad PT-02 y
PT-03.

## Especificación usada

UEFI Specification, capítulo 5 «GUID Partition Table (GPT) Disk Layout» (documento público). La
implementación es original y se basa solo en ese capítulo. Todos los enteros son little-endian; los GUID
se guardan en el orden de bytes del disco (los tres primeros campos en little-endian) y
`gpt::format_guid` los escribe en la forma canónica.

- **Cabecera** (LBA 1 la primaria, última LBA del disco la de respaldo): firma `EFI PART`, revisión,
  tamaño de cabecera, CRC-32 de la cabecera (calculada sobre `HeaderSize` bytes con el propio campo a
  cero), `MyLBA`, `AlternateLBA`, `FirstUsableLBA`, `LastUsableLBA`, GUID de disco,
  `PartitionEntryLBA`, número y tamaño de entradas y CRC-32 de la tabla de entradas.
- **Entrada**: GUID de tipo (cero = sin usar), GUID único, `StartingLBA`, `EndingLBA` (inclusiva),
  atributos de 64 bits y nombre de 36 unidades UTF-16LE.

## Interfaz

```rust
pub fn read_gpt(dev: &(impl BlockDevice + ?Sized)) -> mr_io::Result<GptRead>
```

`GptRead` contiene: `sector_size` (con el que se interpretó la GPT) y `device_sector_size`; el examen de
cada copia (`primary`, `backup`: estado de la cabecera, estado de las entradas, LBA declarada, LBA de
la imagen donde se leyó y campos de la cabecera si su CRC es correcta); `selected` (`Primary`, `Backup`
o ninguna); `shift` y `trailing_sectors_missing` (tolerancias, abajo); `partitions` (`GptPartition`:
un `Partition` común con `Origin::Gpt { index }`, `PartitionKind::Gpt(tipo)`, nombre y `active` = bit 2
«Legacy BIOS Bootable», más el GUID único y los atributos completos); `anomalies` y `confidence`.

Solo devuelve `Err` ante un fallo de E/S no recuperable. Toda lectura pasa por
`sector::read_trusted`: un sector `Unreadable` o `Skipped` **nunca** se interpreta (su contenido es
relleno), produce `Anomaly::UnreadableSector` (LBA del dispositivo) y deja la cabecera o la tabla en
estado `Unreadable`.

Estados de cabecera: `Valid`, `Crc` (firma presente pero CRC-32 o tamaño de cabecera incorrectos),
`Invalid` (CRC correcta pero campos incoherentes), `Missing` (sin firma), `OutsideImage` y
`Unreadable`. Estados de entradas: `Valid`, `Crc`, `OutsideImage`, `Unreadable` y `Unchecked` (cabecera
no válida).

## Validaciones

Cabecera (en este orden; de la 4 a la 9 solo si la CRC es correcta):

1. Firma `EFI PART` (si no, `Missing`).
2. `HeaderSize` entre 92 y el tamaño de sector (si no, `HeaderSize`, estado `Crc`).
3. CRC-32 de la cabecera (`HeaderCrc`, estado `Crc`).
4. Revisión mayor 1 (`RevisionUnsupported`, invalida); una revisión 1.x con x ≠ 0 se acepta e informa
   (`RevisionMinor`).
5. `MyLBA`: 1 en la primaria; en la de respaldo, la posición indicada por la primaria
   (`MyLbaMismatch`).
6. `AlternateLBA`: en la primaria, posterior a `LastUsableLBA`; en la de respaldo, 1
   (`AlternateLbaMismatch`).
7. Rango utilizable: `FirstUsableLBA ≤ LastUsableLBA`, sin incluir la cabecera
   (`UsableRangeInvalid`).
8. Tamaño de entrada 128 × 2^n entre 128 y `MAX_ENTRY_SIZE` (4096), número de entradas ≤
   `MAX_ENTRY_COUNT` (32 768) y tabla ≤ `MAX_ENTRY_ARRAY_BYTES` (4 MiB) (`EntryArrayLimits`). Se
   comprueba **antes** de reservar memoria.
9. Tabla de entradas entre la cabecera y el rango utilizable (primaria) o entre el rango utilizable y
   la cabecera (respaldo) (`EntriesLocationInvalid`).

Cualquier fallo de 4 a 9 (salvo `RevisionMinor`) deja la cabecera `Invalid`. Después, la tabla de
entradas: dentro de la imagen (`EntriesOutsideImage`), legible y con la CRC-32 declarada
(`EntriesCrc`, con la LBA de la imagen del inicio de la tabla).

Entradas de la copia elegida (cada anomalía va en su partición): `EndingLBA < StartingLBA`
(`EntryInverted`, confianza `Invalid`, `sector_count` = 0), fuera del rango utilizable
(`EntryOutsideUsable`), fuera de la imagen (`EntryOutsideImage`), solapes (`EntryOverlap`, en las dos
entradas; barrido ordenado O(n log n)), GUID único repetido (`DuplicateUniqueGuid`) y nombre con UTF-16
mal formado (`NameInvalid`, se sustituye por U+FFFD; el nombre acaba en el primer NUL). Estas
anomalías hacen `Suspicious` la partición pero no cambian la confianza de la tabla.

Si las dos copias son válidas, se comparan GUID de disco, rango utilizable, número y tamaño de
entradas, CRC de la tabla y `AlternateLBA` de la primaria frente a `MyLBA` de la copia
(`CopiesDisagree { field }`, una anomalía por campo).

## Selección de copia

1. Se examina la primaria en la LBA 1.
2. Si su cabecera es válida, el respaldo se busca en su `AlternateLBA`. Si no, se busca en la última
   LBA de la imagen; si allí hay un respaldo válido cuyo `MyLBA` no coincide con su posición, la
   diferencia es el desplazamiento de la imagen y la primaria se vuelve a buscar en `1 - shift` (el
   primer examen se descarta, salvo los sectores ilegibles, que se conservan).
3. Se usa la primaria si cabecera y entradas son válidas; si no, la de respaldo si lo son las suyas
   (`BackupInUse` con los estados de la primaria como motivo); si no, ninguna (`selected = None`,
   confianza `Invalid`).

Es la regla de UEFI 5.3.2 (primaria preferida; respaldo si la primaria no es válida) y la misma que
aplica el validador independiente del corpus.

Confianza de la tabla: `Valid` sin anomalías; `ValidWithFixes` si solo se descartó o no se pudo leer
una copia (o se usó el respaldo); `Suspicious` con desplazamiento, truncado, rango utilizable fuera de
la imagen, desacuerdo entre copias, tamaño de sector distinto del del dispositivo o revisión menor
desconocida; `Invalid` sin copia utilizable.

## Tamaño de sector

Se interpreta primero con el tamaño lógico del dispositivo. Si no aparece ninguna firma en las
posiciones examinadas (y ninguna es ilegible), se prueban los otros tamaños de `PROBED_SECTOR_SIZES`
(512 y 4096) mientras el dispositivo tenga sectores completos de ese tamaño; si así se encuentra una GPT
utilizable se devuelve con `sector_size` = el usado y `SectorSizeMismatch { device, used }`. Las lecturas
con otro tamaño siguen pasando por `read_trusted` (un sector lógico de 4096 B sobre un dispositivo de
512 B es fiable solo si lo son sus 8 sectores).

## Tolerancias (PT-03)

- **Imagen desplazada** (falta o sobra el principio): `shift` = `MyLBA` de la copia encontrada menos su
  posición en la imagen (`LBA disco = LBA imagen + shift`), `ImageShifted { offset }`. Si la primaria
  cae antes del inicio de la imagen, `HeaderOutsideImage` con su LBA de disco.
- **Imagen recortada al final**: si la última LBA del disco declarada (`AlternateLBA` de la primaria o
  `MyLBA` del respaldo) queda más allá de la imagen, `trailing_sectors_missing` y `ImageTruncated`; el
  respaldo es `HeaderOutsideImage`.
- `UsableRangeOutsideImage` si el rango utilizable de la cabecera elegida no cabe en la imagen.
- El desplazamiento **nunca** se aplica en silencio: las particiones conservan las LBA del disco y quien
  llama obtiene la de la imagen con `GptRead::image_lba` (o restando `shift`).

## Límites

- Tabla de entradas ≤ 4 MiB, entradas ≤ 4096 B y ≤ 32 768 entradas: memoria de trabajo acotada a unas
  dos tablas (primaria y respaldo, leídas por trozos de `MAX_TRUSTED_SECTORS`).
- Aritmética comprobada (`checked_*`, `i128` para las conversiones con desplazamiento); ningún dato del
  origen provoca pánicos ni bucles (fuzzing determinista).
- Se leen como mucho 3 cabeceras y 2 tablas por tamaño de sector probado.

## Correspondencia con el manifiesto del corpus

| Código del manifiesto | Anomalía |
|---|---|
| `gpt_{primary,backup}_header_missing` | `HeaderMissing { copy, lba }` |
| `gpt_{primary,backup}_header_crc` | `HeaderCrc` o `HeaderSize` |
| `gpt_{primary,backup}_header_outside_image` | `HeaderOutsideImage { copy, disk_lba }` (LBA imagen = `disk_lba - shift`) |
| `gpt_{primary,backup}_entries_crc` | `EntriesCrc { copy, lba }` |
| `gpt_{primary,backup}_entries_outside_image` | `EntriesOutsideImage { copy, disk_lba }` |
| `gpt_usable_range_outside_image` | `UsableRangeOutsideImage { lba }` |
| `image_shifted` (`lba` = `-offset`, `value` = `offset`) | `ImageShifted { offset }` |

Los estados `gpt_*_header`, `gpt_*_entries` y `gpt_selected` corresponden a `HeaderState`,
`EntriesState` y `selected`; `leading_sectors_missing` y `trailing_sectors_missing` a `shift` y
`trailing_sectors_missing`. `BackupInUse` e `ImageTruncated` no tienen código (se comprueban con
`gpt_selected` y `trailing_sectors_missing`). Los códigos de MBR/EBR (`mbr_*`, `ebr_*`, `hybrid_mbr`,
`protective_mbr_size_mismatch`, `mbr_displaced`) y `superfloppy` no son de este analizador (#12, #15);
tampoco `image_shifted` en imágenes sin GPT, que deduce la búsqueda de volúmenes.

## Pruebas

- Unitarias (`cargo test -p mr-part`, 21 en `gpt.rs`, discos GPT sintéticos construidos en memoria):
  disco sano de 512 y 4096 B; GPT de 4096 B en un dispositivo de 512 B y al revés; CRC de cabecera,
  tamaño de cabecera fuera de rango, CRC de entradas (primaria → respaldo y respaldo dañado → primaria);
  ninguna copia válida; revisión mayor y menor; `MyLBA`/`AlternateLBA` incoherentes; rango utilizable y
  ubicación de la tabla; límites de tamaño y número de entradas (sin reservar memoria); entradas de
  256 B y tablas vacías; solapes, GUID repetido, entrada invertida, fuera del rango y nombre mal formado;
  copias en desacuerdo; imagen desplazada (+63, +1 y -8) y recortada; sectores ilegibles con
  `FaultyDevice` (cabecera, entradas, ambas cabeceras, 4096 sobre 512); dispositivos de 1 a 8 sectores;
  GUID y nombres; y un fuzzing determinista de 3000 iteraciones (bytes alterados, campos extremos con
  CRC recalculadas, recortes y desplazamientos) sin pánicos, con memoria acotada y en menos de 60 s.
- Corpus (`core/mr-part/tests/gpt_corpus.rs`, 4 pruebas, `cargo test -p mr-part --test gpt_corpus --
  --ignored`): cada manifiesto con estado GPT (13) comparado en estados, LBA de las cabeceras, copia
  elegida, desplazamiento, sectores finales ausentes, campos de la cabecera, particiones (tipo, GUID
  único, nombre, LBA y marcador leído en la LBA de la imagen) y el conjunto exacto de anomalías GPT;
  las imágenes sin GPT (con `unreadable_lbas` a través de `FaultyDevice`) no dan ninguna GPT;
  `gpt_4kn` abierta con 512 B y `gpt_basic` con 4096 B; y sectores GPT ilegibles sobre el corpus
  (cabecera, entradas, respaldo, las dos, 4Kn, imagen desplazada y la cabecera elegida de cada imagen).

## Limitaciones

- No se reconstruye una copia a partir de la otra ni se combina una cabecera con la tabla de la otra
  copia (si la primaria tiene la cabecera válida y la tabla dañada se usa el respaldo entero).
- Si la primaria es válida no se busca un respaldo en la última LBA cuando su `AlternateLBA` apunta a
  otra parte (se informa el estado de lo que haya en `AlternateLBA`).
- El desplazamiento solo se deduce del respaldo encontrado en la última LBA de la imagen; una imagen
  desplazada **y** recortada a la vez sin primaria no se reconoce (lo cubrirá la búsqueda de #15).
- La clasificación del esquema (GPT, híbrido o MBR) y la comprobación del MBR protector corresponden al
  analizador MBR (#12) y a la integración.
