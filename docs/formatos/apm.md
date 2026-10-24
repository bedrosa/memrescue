# APM (Apple Partition Map)

Módulo: `core/mr-part/src/apm.rs` (`mr_part::apm::read_apm`), integrado en
`mr_part::read_partition_table` como esquema `Apm`. Solo lectura. Issue #17, paridad PT-04 (M2.5).

## Especificación usada

Apple, *Inside Macintosh: Devices*, capítulo «SCSI Manager», estructuras `Block0` (Driver Descriptor
Map) y `Partition` (entrada del mapa), documentación pública. La implementación es original y se basa
solo en esa descripción. Todos los enteros son **big-endian**.

- **Driver Descriptor Map** (bloque 0): firma `sbSig` = `ER` (0x4552) en +0, tamaño de bloque del
  dispositivo `sbBlkSize` (u16) en +2 y número de bloques `sbBlkCount` (u32) en +4. Los descriptores de
  controladores que siguen no se usan.
- **Entrada del mapa** (una por bloque, desde el bloque 1): firma `pmSig` = `PM` (0x504D) en +0,
  `pmMapBlkCnt` (u32, número de entradas del mapa, igual en todas) en +4, `pmPyPartStart` (u32, primer
  bloque) en +8, `pmPartBlkCnt` (u32, bloques) en +12, `pmPartName` (32 bytes) en +16, `pmParType`
  (32 bytes) en +48, `pmLgDataStart` y `pmDataCnt` (u32, área de datos lógica relativa a la partición) en
  +80 y +84 y `pmPartStatus` (u32, indicadores de estado) en +88. Nombre y tipo son cadenas terminadas en
  NUL.

## Interfaz

```rust
pub fn read_apm(dev: &(impl BlockDevice + ?Sized)) -> mr_io::Result<Option<ApmRead>>
```

`None` si no hay indicios de APM. `ApmRead` contiene: `ddm`, `ddm_block_size` y `ddm_block_count` (DDM
tal como está), `block_size` (con el que se interpreta el mapa), `disk_blocks` (bloques del origen),
`map_entries` (`pmMapBlkCnt` de la primera entrada), `map_found`, `partitions` (`ApmPartition`: un
`Partition` común con `Origin::Apm { index }`, `PartitionKind::Apm(tipo)`, nombre y `active` = bit 31 de
`pmPartStatus` «partición de arranque», más el estado y el área de datos), `anomalies` y `confidence`.
`PartitionTable::apm` guarda la lectura completa y `PartitionTable::apm_entry(p)` los datos de una
partición. `ApmType::text` da el tipo tal como está y `PartitionKind::type_name` un nombre legible para
los tipos documentados (`Apple_partition_map`, `Apple_Free`, `Apple_HFS`, `Apple_HFSX`, `Apple_Driver*`,
`Apple_Boot`, `Apple_Bootstrap`, `Apple_UFS`, `Apple_APFS`... sin distinguir mayúsculas).

Solo devuelve `Err` ante un fallo de E/S no recuperable. Toda lectura pasa por `sector::read_trusted`
(cada entrada son 512 bytes desde el byte `i × B`, que pueden ocupar parte de un sector o varios): un
sector `Unreadable` o `Skipped` **nunca** se interpreta y produce `Anomaly::UnreadableSector`; la
entrada afectada no se lista.

## Detección y esquema

1. `read_partition_table` lee primero el MBR. Solo si la LBA 0 no tiene una tabla MBR utilizable
   (esquema `None` sin volumen en la LBA 0) se busca un APM: un MBR clásico, protector o híbrido manda.
2. El tamaño de bloque `B` candidato es, por este orden, `sbBlkSize` (si el DDM tiene firma y un tamaño
   admitido), el tamaño de sector del origen y 512. Se toma el primero cuyo bloque 1 tiene la firma `PM`;
   si varios la tienen (el bloque 1 de 2048 B de un mapa de 512 B es su entrada 4) se prefiere el que
   describe el propio mapa (`Apple_partition_map` desde el bloque 1).
3. Con una entrada `PM` el esquema es `Apm` y la GPT no se mira. Con un DDM válido y sin mapa, el
   esquema es `Apm` con `apm_map_missing` solo si tampoco hay GPT. «ER» con un tamaño de bloque no
   admitido y sin mapa no es un APM.
4. Las particiones conservan las posiciones **en bloques de `B`** y `PartitionTable::sector_size` es `B`.
   Si `B` difiere del sector del origen se informa `apm_sector_size_mismatch` (aviso: es normal en CD) y
   abrir una partición se rechaza pidiendo reabrir el origen con sectores de `B` bytes (ADR-022: las LBA
   no se convierten en silencio).
5. La LBA 0 sigue informando `mbr_missing` (aviso): el bloque 0 es el DDM, no un MBR.

## Validaciones

| Código | Gravedad | Regla |
|---|---|---|
| `apm_ddm_missing` | sospechosa | Bloque 0 sin firma `ER` (el mapa se lee igualmente) |
| `apm_block_size_invalid` | sospechosa | `sbBlkSize` no es potencia de dos entre 512 y 4096 (`value` = declarado) |
| `apm_block_size_mismatch` | sospechosa | El mapa está en bloques de otro tamaño que el del DDM (`value` = declarado) |
| `apm_disk_size_mismatch` | aviso si declara menos bloques de los que hay; sospechosa si más (imagen recortada) | `sbBlkCount` distinto del tamaño del origen (`value` = declarado) |
| `apm_map_missing` | inválida | DDM válido sin entrada `PM` en el bloque 1 |
| `apm_map_count_invalid` | sospechosa | `pmMapBlkCnt` de la primera entrada es 0 o mayor que 512 (se lee como mucho 512) |
| `apm_map_count_mismatch` | sospechosa | Una entrada declara otro `pmMapBlkCnt` (`value` = el suyo) |
| `apm_map_outside_image` | sospechosa | El mapa declara entradas más allá del final del origen (`value` = primera que falta) |
| `apm_entry_signature` | sospechosa | Entrada sin firma `PM`: no se lista (`value` = entrada) |
| `apm_entry_zero_length` | sospechosa | Entrada con tamaño 0: no se lista |
| `apm_entry_outside_disk` | inválida | La entrada empieza fuera del disco (se lista) |
| `apm_entry_partially_outside_disk` | sospechosa | La entrada empieza dentro y termina fuera |
| `apm_entry_overlap` | sospechosa | Dos entradas se solapan (barrido ordenado: como mucho una por entrada) |
| `apm_entry_data_outside` | sospechosa | `pmLgDataStart + pmDataCnt` sale de la partición |
| `apm_sector_size_mismatch` | aviso | El mapa usa bloques de un tamaño distinto del sector del origen (`value` = `B`) |

`lba` es el sector del origen que contiene el bloque afectado (0 para las del DDM; sin LBA
`apm_map_outside_image` y `apm_sector_size_mismatch`); `value` es el número de entrada salvo donde la
tabla indica otra cosa. La confianza de cada partición es la peor de sus anomalías (ADR-003, ADR-022) y la
de la tabla, la peor de todas.

Se listan todas las entradas con firma y tamaño mayor que 0, en orden de mapa, incluidas
`Apple_partition_map` (el propio mapa) y `Apple_Free` (espacio libre declarado): forman parte de la
tabla y el usuario debe verlas; abrir una de ellas abre ese tramo del disco como cualquier otra.

## Límites

- Como mucho `MAX_MAP_ENTRIES` = 512 entradas y nunca más de las que caben en el origen.
- Aritmética en `u64` con campos de 32 bits (sin desbordamientos posibles) y salida acotada: una anomalía
  por entrada y regla.
- Nombres y tipos: bytes hasta el primer NUL, ASCII imprimible; los demás bytes se muestran como `?`
  (los nombres de Mac OS clásico pueden estar en Mac Roman y no se adivina la codificación).

## Frontera C, CLI e interfaz

- ABI 3 ampliada de forma compatible: `MR_SCHEME_APM` = 4, `MR_ORIGIN_APM` = 4 (`slot` = número de
  entrada desde 1), `attributes` = `pmPartStatus`, `mr_partition_name` = `pmPartName`, nueva
  `mr_partition_apm_type` (tipo tal como está) y códigos de anomalía 400-414 (`API_FFI.md`).
- CLI: `partitions` muestra «Apple Partition Map (APM)», el tamaño de bloque y las entradas; en JSON,
  `"scheme":"apm"`, objeto `"apm"` (`block_size`, `disk_blocks`, `map_entries`, `ddm`,
  `ddm_block_size`, `ddm_block_count`) y, por partición, `"origin":"apm"`, `"type"` (texto del mapa),
  `"name"` y `"status"`. `--partition N` abre la partición como cualquier otra.
- Interfaz: la pestaña «Particiones» la muestra por las rutas existentes (esquema, origen «APM n», tipo
  «`Apple_HFS` (HFS / HFS+)»).

## Pruebas

Ver `PRUEBAS.md` (sección del Apple Partition Map): 13 unitarias con fuzzing, 3 de corpus en
`mr-part`, corpus por la CLI, FFI y C#. Corpus: `apm_basic`, `apm_entry_damaged` y `apm_entry_outside`
(generador determinista y validador independiente con pruebas negativas propias).

## Limitaciones

- Imágenes sintéticas según la especificación; falta un oráculo de terceros (`pdisk`, `hdiutil`) para el
  corpus.
- No se interpretan los descriptores de controladores del DDM ni los campos de arranque de las entradas.
- Los sistemas de archivos HFS/HFS+ de las particiones no se leen (fuera de alcance por ahora).
