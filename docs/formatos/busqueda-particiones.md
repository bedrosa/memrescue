# Búsqueda exprés de particiones perdidas (`mr-part::search`)

Issue #15 (PT-05, PT-06), ADR-023. Diseño propio de MemRescue a partir de
[INV-05b](../investigacion/dmde-fase2/INV-05b-generacion-de-candidatos.md) §8 (sala limpia, ADR-001):
solo especificaciones públicas de los formatos y convenciones de alineación.

## Qué hace

Busca volúmenes NTFS, FAT12/16/32 y exFAT en un número **acotado** de posiciones candidatas, junto con
tablas MBR/EBR (modo estricto) y cabeceras GPT encontradas en esas posiciones. Nunca recorre el disco:
el trabajo depende del presupuesto, no del tamaño del origen (1 GiB y 2 TiB con el mismo contenido
hacen las mismas lecturas). Solo se ejecuta **a petición** (decisión del propietario para M2: no se
lanza al abrir un origen). Es de solo lectura e informativa: no modifica nada ni aplica
desplazamientos en silencio.

Entradas: el dispositivo (con su mapa de validez) y la tabla leída por `read_partition_table` (sus
lecturas no cuentan para el presupuesto). Salida: `SearchResult` con los volúmenes, las tablas
encontradas, las anomalías de la búsqueda, las estadísticas y el estado final.

## Candidatos por niveles

Cada candidato es un desplazamiento en bytes con su nivel, su clase (inicio de volumen, final de
volumen o tabla) y su origen. Cola ordenada por `(nivel, posición, orden de inserción)` y deduplicada
por posición.

| Nivel | Candidatos |
|---|---|
| P0 declarados | Byte 0; inicio y último sector de cada partición de la tabla (también las dudosas; las GPT de una imagen desplazada, en coordenadas de la imagen) |
| P1 derivados | Final de cada volumen confirmado (exacto, alineado a 1 MiB, a cilindro CHS y a cilindro + 63); entradas de tablas encontradas (los enlaces de EBR, relativos a la extendida); con un desplazamiento aceptado, las particiones de la tabla y las posiciones de MBR y GPT del disco corregidas |
| P2 rejilla inicial | LBA 63; `k × 16065` y `k × 16065 + 63` (sectores de 512 B) para `k ≤ cylinders`; múltiplos de 1 MiB dentro de `start_grid_bytes` |
| P3 huecos | Por cada hueco sin volumen confirmado: su inicio, alineado a 1 MiB, a cilindro y + 63; su último sector y el último sector de los dos MiB anteriores a su final; como mucho `probes_per_gap`. Se recalculan tras cada volumen |
| P4 rejilla final | Último sector con el sector del dispositivo, 512 y 4096; último sector de cada MiB dentro de `end_grid_bytes`, contado desde el final y desde el inicio |

Reglas: los candidatos de P2 a P4 deben estar alineados al sector del dispositivo; los que caen
dentro de un volumen ya confirmado como válido se descartan. Las tablas no se buscan dentro de un
volumen válido ni de una partición válida de la tabla (un sector con aspecto de MBR dentro de datos
es un señuelo).

## Sondeo y evidencia

En cada candidato se lee una ventana (`probe_window`, 8 KiB) y se prueban los reconocedores en el
byte 0 y las copias cercanas (FAT32 en el sector 6, exFAT en el 12; en P0 también con sectores de
4096 B). Una estructura encontrada en `q` propone varias hipótesis: que sea la principal (inicio en
`q`) o una de sus copias (inicio en `q -` posición de la copia). Se evalúan leyendo la principal, las
copias y una estructura básica, y se elige la de mejor evidencia, prefiriendo la principal en los
candidatos de inicio y la copia en los de final.

| Sistema | Principal | Copias | Estructura básica (S) | Inicio declarado (U) |
|---|---|---|---|---|
| NTFS | Sector de arranque («NTFS    », BPB con campos FAT a cero, `$MFT` y `$MFTMirr` dentro del volumen) | Último sector: `inicio + total × S` y variante `+1` | Registro 0 de `$MFT` con firma `FILE` | Sectores ocultos (0x1C) |
| FAT12/16/32 | BPB (fatgen103; tipo por número de clústeres, disposición coherente, FAT con sitio) | FAT32: `BPB_BkBootSec`; FAT12/16 no tienen | Primera entrada de la FAT con el byte de medio | Sectores ocultos (0x1C) |
| exFAT | Región de arranque de 12 sectores con su suma (sector 11) | Región de respaldo (sector 12) | `FAT[0] = F8FFFFFF`, `FAT[1] = FFFFFFFF` | `PartitionOffset` |

Una copia solo cuenta si es válida **y coherente** con la principal (NTFS: BPB y número de serie;
FAT: BPB; exFAT: sector 0 sin `VolumeFlags` ni `PercentInUse`). Los reconocedores implementan el
rasgo `Recognizer` (`search/fs.rs`); añadir ext, XFS, HFS+, APFS, Btrfs o ReFS en hitos posteriores es
implementarlo y registrarlo, sin cambiar la búsqueda.

**Sectores ilegibles.** Toda lectura pasa por `sector::read_trusted`: un sector `Unreadable` o
`Skipped` nunca llega a un reconocedor, aunque su relleno imite una firma. El indicador queda «?» y
el volumen lleva la anomalía con la LBA. Las lecturas fallidas cuentan para el presupuesto.

## Indicadores propios (PT-06)

Equivalentes en propósito a los de la referencia, con un estado más para lo no verificable:

| Indicador | Significado | Estados |
|---|---|---|
| E | Una entrada de la tabla del origen (o de una tabla encontrada) describe el volumen: mismo inicio y tamaño declarado igual o mayor | `+` / `x` |
| P | Estructura principal (sector de arranque o región de arranque) válida | `+` `x` `?` `o` |
| C | Copia válida y coherente con la principal | `+` `x` `?` `o` `-` |
| S | Estructuras básicas del sistema de archivos | `+` `x` `?` `o` `-` |
| U | Inicio declarado por el propio volumen igual a su posición | `+` `x` `-` |
| A | Inicio alineado a 1 MiB o a una convención CHS | `+` `x` |
| Estado | `tabla #N` (la partición N de la tabla es este volumen: no se duplica), `encontrada` (fuera de la tabla, borrada o perdida) o `parcial` (empieza antes de la imagen) | |

Símbolos: `+` sí, `x` ausente o dañada (los sectores se leyeron), `?` no verificable (sector
ilegible), `o` fuera de la imagen, `-` no aplica. Formato compacto: `Ex P+ C+ Sx U+ A+`.

## Confianza (ADR-003)

- **Válida**: principal y copia coherentes; o principal y estructuras sin copia en el formato (FAT12/16);
  o principal, estructuras y entrada de tabla. Y ninguna anomalía sospechosa.
- **Sospechosa**: el resto de volúmenes informados, con sus motivos (anomalías de gravedad
  sospechosa): principal dañada (restaurable desde la copia, enlaza con PT-08), principal o copia
  ilegibles, volumen parcial, copia ausente o incoherente, termina fuera de la imagen, otro tamaño de
  sector que el del origen, inicio no alineado al sector, desplazamiento detectado, solape con un
  volumen válido, o solo la principal sin nada que la confirme.
- Sin principal ni copia válidas no se informa ningún volumen.

La puntuación (P 4, C 3, S 2, E 2, U 1, A 1) solo ordena y desempata.

Códigos de anomalía (texto y ABI): `found_primary_damaged` 301, `found_primary_unreadable` 302,
`found_primary_outside_image` 303, `found_backup_missing` 304, `found_backup_unreadable` 305,
`found_backup_outside_image` 306, `found_backup_mismatch` 307, `found_beyond_image_end` 308,
`found_sector_size_mismatch` 309, `found_unaligned` 310, `found_shift_detected` 311,
`found_self_location_mismatch` 312 (aviso), `found_overlap` 313, `found_unconfirmed` 314; de la
búsqueda: `image_shifted` 320 (`lba` = posición en la imagen de la LBA 0 del disco, `value` = sectores
que faltan al inicio, como en el manifiesto del corpus) y `mbr_displaced` 321.

## Imágenes desplazadas

Evidencias de un desplazamiento `δ = posición en la imagen - posición en el disco`: volúmenes cuyo
inicio declarado difiere de su posición, cabeceras GPT encontradas cuyo `MyLBA` no coincide y el
desplazamiento informado por el lector GPT. La hipótesis se acepta solo cuando **dos evidencias
independientes** coinciden en el mismo `δ ≠ 0` y ninguna partición de la tabla coincide con un
volumen en `δ = 0`. Se informa (`image_shifted`), añade candidatos P1 corregidos (si aparece la tabla
del disco en `δ`, `mbr_displaced`) y marca los volúmenes que dependen de ella
(`found_shift_detected`). **Nunca se aplica en silencio**: abrir uno de esos volúmenes exige
`apply_shift` (API), `MR_PARTITION_OPEN_APPLY_SHIFT` (ABI), `--apply-shift` (CLI) o confirmación en
la interfaz. Un volumen que empieza antes de la imagen (δ negativo) se informa como parcial desde su
copia y no se puede abrir.

## Presupuesto, progreso y cancelación

| Opción | Defecto | Límite |
|---|---|---|
| `max_reads` (lecturas físicas; los aciertos de caché no cuentan) | 4096 | 1 048 576 |
| `max_bytes` | 32 MiB | 4 GiB |
| `max_volumes` | 256 | 4096 |
| `probe_window` | 8 KiB | 64 KiB (múltiplo de 512) |
| `start_grid_bytes` / `end_grid_bytes` | 64 MiB / 16 MiB | 1 TiB |
| `cylinders` | 64 | 4096 |
| `probes_per_gap` | 8 | 64 |
| `families` | todas | NTFS, FAT, exFAT, MBR, GPT |

Cada nivel P2 a P4 puede gastar como mucho el 40 % de `max_reads` y ninguno invade la reserva del
25 % para P0 y P1. Al agotar el presupuesto, un cupo o `max_volumes`, el estado es
`BudgetExhausted` con los candidatos pendientes por nivel; nunca se continúa en silencio. La caché
(64 ventanas + 1 MiB, FIFO) es determinista: mismo origen y opciones dan las mismas lecturas, en el
mismo orden, y el mismo resultado. La cancelación se comprueba antes de cada lectura física y
devuelve el resultado parcial con estado `Cancelled` (la tarea de `mr-api` termina cancelada con
resultado). El progreso es `lecturas / max_reads`. No hay límite de tiempo: la cancelación lo cubre.

En el corpus, las búsquedas completas hacen entre 3 y 64 lecturas; en una imagen vacía de 512 MiB,
213 (rejillas completas).

## Integración

- `mr-api`: `start_partition_search_task(src, opts)`, `search_partitions`, `open_found_volume(parent,
  result, index, apply_shift)` (rechaza parciales y desalineados; recorta al final del origen).
- ABI 3 (ampliación compatible): ver `API_FFI.md`.
- CLI: `find-partitions <imagen> [--json] [--max-reads N]`; `--found N [--apply-shift]` para trabajar
  con un volumen en `sectors`, `verify`, `hash` o `image-info`.
- Interfaz: pestaña «Particiones», «Buscar particiones perdidas» con progreso y cancelación, lista de
  volúmenes con indicadores y confianza y «Abrir volumen en el visor».

## Limitaciones

- Solo NTFS, FAT y exFAT en M2; los demás sistemas de archivos y sus copias lejanas, en hitos
  posteriores. Las tablas encontradas se interpretan con el sector del dispositivo.
- Un volumen en una posición que no está en ninguna rejilla, sin tabla que lo declare, sin vecino
  confirmado del que encadenar y sin copia al final de un MiB no se encuentra: para eso está el
  escaneo completo (M6).
- La copia NTFS de NT 3.51 (mitad del volumen) no se comprueba.
- Los boot sectors del corpus son sintéticos (sin MFT), por eso los NTFS del corpus muestran `Sx`.
