# 05 · Algoritmos verificados en código decompilado

Todo lo que aparece en este documento procede de pseudocódigo de Ghidra (no es código fuente
original). Se describe el **comportamiento** en términos de las especificaciones públicas de cada
formato, que es lo que se puede reutilizar en una implementación original. No se copia código.

Leyenda: **OBS** = observado directamente en el decompilado/bytes · **INF** = inferido.

---

## 5.1 Validación del boot sector NTFS: `FUN_00625080` (OBS)

Evidencia: `ev_fa2b43019b1c718e41a4057442bee94df75162c3a1baa7f72bcf550ec5d93b5e`.

Entrada: buffer del sector, tamaño de sector esperado, flag de modo estricto.
Devuelve: `1` válido · `2` válido en modo no estricto · `3` inválido · `4` parcialmente válido / dudoso.

Comprobaciones (offsets del boot sector NTFS):

| Offset | Campo | Regla |
|---|---|---|
| 0x0B | bytes por sector | igual al esperado; si difiere: debe ser potencia de 2, ≥ 0x200 y ≤ 0x10000 (entonces resultado "4") |
| 0x0D | sectores por cluster | potencia de 2 si ≤ 0x80; si > 0x80 se interpreta como `2^(256 − valor)`, exponente ≤ 15 |
| 0x10, 0x11, 0x16 | campos reservados de FAT | deben ser 0 |
| 0x28 | sectores totales (u64) | ≠ 0 |
| 0x30 | LCN de `$MFT` (u64) | ≠ 0 |
| 0x40 | clusters por registro MFT | byte bajo; bytes 1–3 = 0; si bit 7: tamaño `2^(−valor)` con exponente ≤ 16 y ≥ tamaño de cluster; si no: potencia de 2 y `cluster × valor ≤ 64 KiB` |
| 0x44 | clusters por registro de índice | mismas reglas que 0x40 (solo en modo estricto) |
| - | tamaño de cluster | `bps × spc ≤ 2 MiB` (coherente con changelog "cluster size up to 2M") |
| 0x1FE | firma | `0xAA55` |
| 0x03 | OEM ID | `"NTFS    "` (8 bytes) |

## 5.2 Validación de la cabecera GPT: `FUN_005ec9a0` (OBS)

Evidencia: `ev_fa2b43019b1c718e41a4057442bee94df75162c3a1baa7f72bcf550ec5d93b5e`.

Parámetros: cabecera, LBA donde se leyó, nº de sectores del disco, tamaño de sector, modo estricto.
Devuelve `1` correcta, `4` usable con correcciones, `3` inválida.

1. Firma `"EFI PART"` en +0x00.
2. Tamaño de cabecera (+0x0C) ≥ 92 y ≤ tamaño de sector.
3. **CRC-32** de la cabecera con el campo CRC (+0x10) puesto a 0 durante el cálculo, comparado con el valor original.
4. Tamaño de entrada (+0x54) entre 128 y 4096; nº de entradas (+0x50) entre 1 y 1024.
5. LBA de inicio de entradas (+0x48) > 1 y que la tabla quepa en el disco.
6. Coherencia `MyLBA` (+0x18) / `AlternateLBA` (+0x20): una debe ser 1 y la otra el último sector; si no, se marca como dudosa.
7. `FirstUsableLBA` (+0x28) < `LastUsableLBA` (+0x30) y que no solapen las tablas primaria/secundaria.
8. Si la cabecera se encontró en un LBA distinto de `MyLBA` (disco desplazado o imagen parcial), en modo no estricto **rebasa** `PartitionEntryLBA` y `MyLBA` al LBA real (tolerancia a desplazamientos).

## 5.3 NTFS: registros, atributos, runs e índices (OBS)

Evidencias: `ev_815c5901…` (fixup/índice), `ev_cc5ee0e4…`, `ev_1691765f…`, `ev_d24fa02b…`,
`ev_a9e54472…`, `ev_f493bc66…`, `ev_63da8125…`, `ev_c7608b43…`, `ev_a61b5993…`, `ev_8f786667…`
(prefijos; los identificadores completos se conservan en el registro local de evidencias).

### Fixups (Update Sequence Array): `FUN_00613770`
- Magia en +0 igual a la pedida: `FILE` (0x454C4946), `INDX` (0x58444E49), `RCRD`, `BAAD`.
- Offset USA (u16 +4): `usa+2 ≤ size` y `≤ 0x200`. Nº USA (u16 +6) ≠ 0 y `(n−1)×512 = size`.
- Para cada sector de 512 B, el u16 en `0x1FE + k×0x200` debe coincidir con el USN; se restaura el valor guardado.
- **Tolerancia**: un desajuste en un sector que está totalmente fuera del área usada (FILE: bytes en uso +0x18; INDX: +0x1C + 0x18) no invalida el registro.

### Reparación tolerante: `FUN_006135a0`
Devuelve máscara de daños (1 magia, 2 offset USA, 4 tamaño asignado, 8 nº USA, 0x10 sector, 0x20 offset del primer atributo) y normaliza los campos con valores por defecto (FILE 0x400, INDX 0x1000). El escáner acepta un FILE si `(mask & 0xFFEE) == 0`.

### Atributos: `FUN_00615530`, `FUN_006156d0`, `FUN_00615980`
- Primer atributo en u16 +0x14; siguiente = actual + longitud (múltiplo de 4, ≥ 8, dentro del registro); fin con tipo `0xFFFFFFFF`.
- Cabecera ≥ 0x18; no residente ≥ 0x40 (0x48 si comprimido); nombre UTF-16 (+0x0A, longitud +0x09) dentro del atributo; residente: offset de valor ≥ 0x18; no residente: offset de runs ≥ 0x40.
- `FUN_00615770` intenta **reconstruir** una cabecera `$DATA` dañada comprobando que los runs suman exactamente `lastVCN+1` clusters.

### Decodificador de runs: `FUN_00613d60`
- Byte cabecera 0 = fin. Nibble bajo = tamaño de longitud (1–8, sin signo); nibble alto = tamaño de offset (0–8, **con signo**, delta acumulado sobre el LCN previo).
- Offset de tamaño 0 ⇒ run disperso (*sparse*). En modo estricto ambos nibbles < 5.

### Índices / directorios: `FUN_006133a0`, `FUN_006a2c60`, `FUN_00615d20`
- Entradas desde +0x18 + offset; longitud de entrada (u16 +8) ≥ 0x10 y múltiplo de 8; flags +0x0C (bit 1 subnodo, bit 2 última).
- **Todas las entradas de un nodo deben tener la misma referencia padre** (u32 en entrada+0x10); ese valor identifica el directorio propietario → útil para reconstruir árboles a partir de INDX huérfanos.
- `$FILE_NAME`: padre (u32 +0x10), secuencia (u16 +0x16), longitud de nombre (byte entrada+0x50).
- Nombres `$…` con padre 5 se mapean a registros de sistema 0–8 (INF: `$MFT`, `$MFTMirr`, …).

### LZNT1: `FUN_00613930`, lector comprimido `FUN_0061ae40`
- Bloques de 4 KiB; cabecera u16: `&0xFFF` = tamaño−1, bit 0x8000 = comprimido.
- Grupos de 8 tokens por byte de flags; división desplazamiento/longitud empieza en 12/4 bits y se ajusta según la posición; longitud = `(token & máscara) + 3`.
- Unidad de compresión = 16 clusters; LCN disperso = −2; unidad con < 16 clusters reales ⇒ comprimida; sin clusters reales ⇒ ceros.

### `$LogFile`: `FUN_006168d0`, `FUN_00616a30`
- Acepta páginas `RCRD` (o `BAAD` con offset USA 0x1E); lee LSN (+8) y +0x20. **No** se encontró replay redo/undo ni `RSTR` (coherente con "process FS journal (Beta)").

## 5.4 Escaneo NTFS sin sistema de archivos (OBS + INF)

- `FUN_006aa3f0` (bucle): recorre el disco por fragmentos del tamaño de cluster, con un paso que se **divide por 2 en cada pasada** (escaneo grueso a fino), y prueba cada posición de 512 B buscando `FILE`, `BAAD` o `INDX`.
- `FUN_006a9b60` (por coincidencia): `BAAD` se trata como `FILE`; los FILE válidos se agregan a candidatos de volumen; los INDX válidos usan su VCN (+0x10) para calcular el inicio de la asignación de índice y verificar con un registro vecino.
- `FUN_006a8c50` agrupa coincidencias en **volúmenes candidatos** (registros de 0x90 bytes con contadores de votos y comprobación de proporción 3/4: significado exacto INF).
- Parámetros de volumen guardados en el log: `FILE` (tamaño de registro), `CLUS` (tamaño de cluster), `INDX` (tamaño de índice), `coffs` (desplazamiento de cluster), `start`, `size` (INF por formato de cadena 0x6aded5).
- `FUN_00625640` estima el tamaño del volumen con el registro 6 (`$Bitmap`: tamaño×8×cluster) y el 8 (`$BadClus:$Bad`).

## 5.5 Superbloque ExtFS: `FUN_006420e0` (OBS)

Evidencia: `ev_9677167c96c5fbd9d8aad256293677f4bbffa202c412963cfd4545312990b339`.

| Offset | Campo (ext2/3/4) | Regla |
|---|---|---|
| 0x38 | `s_magic` | `0xEF53` |
| 0x18 | `s_log_block_size` | ≤ 14 |
| 0x20 | `s_blocks_per_group` | ≠ 0 |
| 0x28 | `s_inodes_per_group` | ≠ 0 |
| 0x60 | `s_feature_incompat` | si bit 0x80 (`INCOMPAT_64BIT`): comprueba el campo en +0x150 (< 0x400000) y que `log2(+0x150) + 10 + log_block_size ≤ 30` |

El significado del campo +0x150 no se ha confirmado (INF: límite relacionado con tamaño de grupo/flex).

## 5.6 Superbloque de contenedor APFS: `FUN_0079d5f0` (OBS)

Evidencia: `ev_9677167c96c5fbd9d8aad256293677f4bbffa202c412963cfd4545312990b339`.

- `nx_magic` en +0x20 = `NXSB` (0x4253584E).
- Tipo de objeto (+0x18) `& 0xFFFF == 1` (`OBJECT_TYPE_NX_SUPERBLOCK`); si los bits altos `& 0xC0000000 == 0x80000000` (objeto físico) ⇒ resultado 1, si no ⇒ 4.
- `nx_block_size` (+0x24) potencia de 2 entre 4096 y 65536 y ≤ tamaño del buffer.
- Llama a `FUN_0079c320(bloque, tamaño)`; INF: verificación del checksum Fletcher-64 del objeto (no decompilado).

## 5.7 Otros algoritmos confirmados

| Algoritmo | Función | Detalle |
|---|---|---|
| MD5 | `FUN_00458720` | transformación de 64 pasos RFC 1321 |
| CRC-32 | `FUN_0045a370` | tabla 0x8a7e44, polinomio reflejado 0xEDB88320 |
| Descompresión zlib | `FUN_005dbba0` | `inflateInit_` / `inflate(Z_FINISH)` / `inflateEnd` (zlib 1.2.7) |
| Btrfs | `FUN_00646610` | referencia a `_BHRfS_M` (magia del superbloque en +0x40); no decompilado |

## Patrón de diseño común

Todos los validadores devuelven un **nivel de confianza** (válido / válido con reservas / inválido)
en lugar de un booleano y, en modo no estricto, **corrigen** campos incoherentes. Esto es central en
el producto: los mismos parsers sirven para montar volúmenes sanos y para reconstruir volúmenes
dañados a partir de fragmentos encontrados por escaneo.
