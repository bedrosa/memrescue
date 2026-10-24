# INV-03b · RAID virtual: mapeo, delay, reconstrucción y detección automática de parámetros

Fecha: 2026-10-24 · Issue #28 (hito M5 · Virtual RAID) · Alimenta la implementación de `mr-raid`
(RD-01 a RD-10 de `PARIDAD_DMDE_PRO.md`).

> Sala limpia (ADR-001): en esta fase **no** se ha ejecutado DMDE ni se ha analizado su binario (ni
> decompilación, ni desensamblado, ni cadenas, ni herramientas de ingeniería inversa). El punto de partida
> que cita `ROADMAP.md` (direcciones de cadenas) y el documento de la fase 1 sobre RAID **no** se han
> consultado. Las fuentes son el manual público de DMDE, especificaciones públicas (SNIA DDF), la
> documentación y el formato en disco públicos de Linux md (incluida la función pública de mapeo del
> controlador md, de la que solo se recogen fórmulas, sin copiar código), el artículo público sobre la
> matemática de RAID-6, documentación pública de otras herramientas para la práctica general y el código de
> este repositorio. Del manual solo se recogen hechos breves con su cita; el diseño de §6 a §9 es propio de
> MemRescue.

Leyenda:

- **OBS** = observado en la documentación pública de DMDE (se indica URL y sección).
- **REPO** = consta en un documento o en el código de este repositorio (se indica dónde).
- **SPEC** = especificación o documentación pública de terceros (se indica URL y sección).
- **INF** = inferencia propia, razonada a partir de lo anterior.
- **DESC** = desconocido: no se puede afirmar con las fuentes permitidas.

## 1. Fuentes consultadas

| Fuente | URL | Uso |
|---|---|---|
| Manual de DMDE: *Construct RAID* | <https://dmde.com/manual/raids.html> | Tipos, parámetros, delay, rotación personalizada, cálculo automático |
| SNIA, *Common RAID Disk Data Format* (DDF) 1.2 | <https://www.snia.org/sites/default/files/SNIA-DDFv1.2.pdf> | §4.2 (PRL/RLQ, ecuaciones de reparto), §4.2.19.2 (campo de Galois), §5.5 (cabecera ancla) |
| H. P. Anvin, *The mathematics of RAID-6* (rev. 2011) | <https://www.kernel.org/pub/linux/kernel/people/hpa/raid6.pdf> | §1 (GF(2^8)), §2 (P, Q y recuperación), §4 (corrupción de un disco) |
| Página de manual de `mdadm(8)` | <https://man7.org/linux/man-pages/man8/mdadm.8.html> | `--layout`, `--chunk`, `--data-offset`, versiones de metadatos |
| Wiki de Linux RAID (archivada): *RAID superblock formats* | <https://archive.kernel.org/oldwiki/raid.wiki.kernel.org/index.php/RAID_superblock_formats.html> | Superbloques 0.90 y 1.x: posición, número mágico y campos |
| Linux, `drivers/md/raid5.c` (`raid5_compute_sector`, `raid6_d0`, `raid6_idx_to_slot`) | <https://github.com/torvalds/linux/blob/master/drivers/md/raid5.c> | Fórmulas de las disposiciones md (solo hechos; sin copiar código) |
| Runtime Software, *RAID Calculator* (ayuda) | <https://www.runtime.org/help_raid_chm/raid_calculator.htm> | Equivalencia de nombres backward/forward, dynamic ↔ left/right, symmetric/asymmetric |
| Foros públicos de recuperación (HDDGuru) sobre paridad retrasada | <https://forum.hddguru.com/viewtopic.php?p=119070>, <https://forum.hddguru.com/viewtopic.php?p=100230> | Solo práctica general: existencia de controladoras con paridad retrasada; valores citados por usuarios |
| Comentarios públicos del controlador NTFS de Linux (`fs/ntfs/layout.h`) | <https://code.woboq.org/linux/linux/fs/ntfs/layout.h.html> | Número de registro MFT en la cabecera `FILE` (NTFS 3.1) |
| Documentación de ext4 del kernel | <https://www.kernel.org/doc/html/latest/filesystems/ext4/globals.html> | `s_block_group_nr` en las copias del superbloque |
| UEFI 2.10, cap. 5 | <https://uefi.org/specs/UEFI/2.10/05_GUID_Partition_Table_Format.html> | §5.3.2 (cabecera GPT, `MyLBA`, CRC del array de entradas) |
| Documentos del repositorio | `DECISIONES.md` (ADR-001, ADR-019, ADR-020, ADR-024), `mr-io` (`BlockDevice`, `RangeView`, `NullDevice`, `resilient`), INV-05b, `PARIDAD_DMDE_PRO.md` §8 | Contexto, contratos y restricciones |

En esta sesión se recuperaron y leyeron el manual de DMDE, la especificación DDF 1.2 (texto extraído del PDF
público), el artículo de RAID-6, la página de `mdadm(8)`, la página de superbloques de md, la función de
mapeo de md y la ayuda de Runtime. Las citas de NTFS, ext4 y UEFI repiten referencias ya usadas en INV-05b.

## 2. Lo que dice el manual de DMDE (comportamiento visible)

| # | Afirmación | Etiqueta y fuente |
|---|---|---|
| M1 | Construye un RAID virtual con discos, particiones o imágenes cuando ni la controladora ni el sistema operativo pueden montarlo; también puede detectar los parámetros desconocidos. | OBS: raids.html, *Construct RAID* |
| M2 | Tipos: RAID-0, RAID-1, RAID-4, RAID-5, RAID-6, rotación personalizada y JBOD/Spanned. | OBS: raids.html, *RAID types* |
| M3 | RAID-0: si falta un disco, solo se recuperan ficheros pequeños que caben en un bloque. | OBS: raids.html, *RAID-0* |
| M4 | RAID-1: sin tamaño de stripe; solo lectura; un disco sano basta por sí solo. | OBS: raids.html, *RAID-1* |
| M5 | RAID-4: un disco de paridad dedicado; todos los datos se recuperan sin uno cualquiera de los discos sustituyéndolo por un dispositivo NULL. | OBS: raids.html, *RAID-4* |
| M6 | RAID-5: paridad distribuida, **cuatro tipos de rotación**; algunas RAID hardware usan **paridad retrasada**; se recupera todo con un disco NULL. | OBS: raids.html, *RAID-5* |
| M7 | RAID-6: dos paridades de distinto tipo; **solo un disco NULL**, porque solo se admite un tipo de paridad; solo lectura. | OBS: raids.html, *RAID-6* |
| M8 | Rotación personalizada (menú *Striping*): enteros no negativos = orden de los bloques de datos, −1 = paridad XOR, −2 = bloque sin uso. El ejemplo del manual es una rejilla de 4 discos × 4 filas con la segunda paridad de RAID-6 marcada como −2. | OBS: raids.html, *RAID: Custom rotation* |
| M9 | JBOD/Spanned: discos unidos uno tras otro, sin alternancia ni stripe; lo de un disco ausente se pierde; lectura y escritura. | OBS: raids.html, *JBOD/Spanned* |
| M10 | Parámetros: *Stripe Size* (bytes del bloque de rotación; no se usa en RAID-1 ni JBOD), *Delay* («rotation delay», implementaciones especiales de RAID-5 y RAID-6), *Offset* (inicio de los datos en cada disco), *Offset and Size* por disco en sectores, disco NULL, *Striping...*, *Save/Load*, *Read ahead* (mejora el rendimiento con stripes pequeños; desaconsejado con sectores defectuosos). | OBS: raids.html, *RAID parameters* |
| M11 | El RAID-5 software de Windows usa la rotación *backward dynamic* («bkdyn») y stripe de 64 KiB (ejemplo para acotar el cálculo automático). | OBS: raids.html, *RAID: Auto Calculate (Scan)* |
| M12 | El cálculo automático estima parámetros desconocidos y/o el orden de los discos de arrays con stripes; algunos discos pueden faltar, pero el **número original de discos debe indicarse exactamente**; clasifica estadísticamente las disposiciones más probables; cuanto más precisos los parámetros dados (rango estrecho de stripe, una sola rotación), más rápido y exacto. | OBS: raids.html, *RAID: Auto Calculate (Scan)* |
| M13 | *LBA Offset Step*: prueba desplazamientos a intervalos fijos (16 → sectores 0, 16, 32…; 2048 para la alineación de 1 MiB de Windows; algunos sistemas antiguos alinean a 63); paso 1 = exhaustivo y muy lento; paso 0 desactiva la detección del desplazamiento; se prefiere indicar el contenedor o la partición si se conocen. | OBS: raids.html, *LBA Offset Step* |
| M14 | El orden conocido se fija como lista de números de disco separados por comas. | OBS: raids.html, *Disk order* |
| M15 | Un tipo, rotación u orden incorrectos producen una recuperación incorrecta aunque el árbol de directorios parezca correcto. | OBS: raids.html, *Construct RAID* |
| M16 | El manual **no** nombra las otras tres rotaciones de RAID-5, no define el delay más allá de la frase citada, no describe la métrica del cálculo automático ni qué estructuras lee, no menciona RAID anidados (10, 50, 60) ni metadatos de controladoras o de md. | OBS (por ausencia): raids.html |
| M17 | La búsqueda exprés de particiones se ejecuta también al construir un RAID. | OBS: partitions.html, *Partitions Window* (recogido en INV-05b M1) |

## 3. Especificaciones y práctica pública

### 3.1 Terminología

- SPEC (DDF §4.2.1): *strip* = bloque contiguo de un miembro (lo que DMDE llama *stripe size*, md
  *chunk*); *stripe* = la fila de strips con el mismo índice en todos los miembros. En este documento:
  **bloque** (tamaño `c` bytes) y **fila** `s`. `n` = miembros totales (incluidos los ausentes), `k` =
  bloques de datos por fila, `x` = desplazamiento lógico en bytes.
- INF: «Stripe Size» de DMDE se interpreta como `c` (bytes por bloque de un miembro), coherente con M10
  («block size for data rotation») y con el ejemplo de 64 KiB de M11.

### 3.2 Disposiciones RAID-5 (SPEC: `raid5_compute_sector` de md; DDF §4.2.8 a §4.2.10)

Fila `s = ⌊L / k⌋` y posición de datos `d = L mod k`, con `L = ⌊x / c⌋` y `k = n − 1`. `p` = miembro de paridad.

| Nombre md (DMDE/Runtime) | Paridad `p` | Miembro del dato `d` |
|---|---|---|
| `left-asymmetric` (backward) | `(n − 1) − (s mod n)` | `d` si `d < p`; si no, `d + 1` |
| `right-asymmetric` (forward) | `s mod n` | `d` si `d < p`; si no, `d + 1` |
| `left-symmetric` (backward dynamic) | `(n − 1) − (s mod n)` | `(p + 1 + d) mod n` |
| `right-symmetric` (forward dynamic) | `s mod n` | `(p + 1 + d) mod n` |
| `parity-first` | `0` | `d + 1` |
| `parity-last` (RAID-4) | `n − 1` | `d` |

- SPEC (Runtime, *RAID Calculator*): backward = left-asymmetric, backward dynamic = left-symmetric,
  forward = right-asymmetric, forward dynamic = right-symmetric; la variante «dynamic» se llama así por el
  RAID-5 de discos dinámicos de Windows, y es la habitual del RAID-5 software en Windows y Linux.
- INF (M6, M11 y lo anterior): las «cuatro rotaciones» de DMDE son, con alta probabilidad, estas cuatro;
  «bkdyn» = left-symmetric. DESC: los nombres exactos de las otras tres en la interfaz de DMDE.
- SPEC (`mdadm(8)`, `--layout`): disposición por defecto `left-symmetric`; chunk por defecto 512 KiB al
  crear (64 KiB con `--build`).
- SPEC (DDF §4.2.9, ec. 14 y 15): «Rotating Parity N with Data Restart» (PRL=05, RLQ=02) coloca la paridad
  en `N − 1 − (s mod N)` y salta la paridad al colocar datos: equivale a left-asymmetric. INF por las
  figuras de §4.2.8 y §4.2.10 y por las equivalencias de md (`ddf-zero-restart`, `ddf-N-restart`,
  `ddf-N-continue` en `mdadm(8)`): RLQ=00 = right-asymmetric y RLQ=03 = left-symmetric.

### 3.3 Disposiciones RAID-6 (SPEC: md; DDF §4.2.19 a §4.2.21)

`k = n − 2`; `q` = miembro de la segunda paridad.

| Nombre md | `p` | `q` | Miembro del dato `d` |
|---|---|---|---|
| `left-symmetric` | `(n − 1) − (s mod n)` | `(p + 1) mod n` | `(p + 2 + d) mod n` |
| `right-symmetric` | `s mod n` | `(p + 1) mod n` | `(p + 2 + d) mod n` |
| `left-asymmetric` / `right-asymmetric` | como en RAID-5 | `p + 1`, o `0` si `p = n − 1` | si `p = n − 1`: `d + 1`; si no: `d` si `d < p`, `d + 2` si `d ≥ p` |
| `parity-first` / `parity-last` | `0` / `n − 2` | `1` / `n − 1` | `d + 2` / `d` |
| `ddf-zero-restart` (RLQ=01) | como right-asymmetric | como right-asymmetric | como right-asymmetric |
| `ddf-N-restart` (RLQ=02) | `(n − 1) − ((s + 1) mod n)` | como left-asymmetric | como left-asymmetric (primera fila D…D P Q) |
| `ddf-N-continue` (RLQ=03) | `(n − 1) − (s mod n)` | `(p − 1) mod n` | `(p + 1 + d) mod n` |
| `*-6` (transición desde RAID-5) | disposición RAID-5 sobre `n − 1` miembros | `n − 1` | disposición RAID-5 sobre `n − 1` |

**Orden de los coeficientes de Q** (determina el contenido de Q y, por tanto, la reconstrucción):

- SPEC (md, `raid6_d0` y `raid6_idx_to_slot`): en las disposiciones md el coeficiente de cada dato es
  `g^d` con `d` = índice lógico del dato en la fila (se recorre desde el miembro siguiente a Q); en las
  disposiciones DDF el coeficiente es `g^m` con `m` = posición física del miembro (P y Q ocupan posición).
- SPEC (DDF §4.2.19.1): `Q = Σ K_u · D_u` con `K_u = gfilog(u)`.
- INF: hay al menos dos convenciones de Q incompatibles; `mr-raid` debe parametrizarlas (`q_orden`).

### 3.4 Aritmética de RAID-6 (SPEC: Anvin §1 y §2; DDF §4.2.19.2)

- Campo GF(2^8) con polinomio `x^8 + x^4 + x^3 + x^2 + 1` (0x11D, igual en Anvin §1 y DDF §4.2.19.2.1);
  suma = XOR; generador `g = {02}`.
- `P = D_0 ⊕ … ⊕ D_{k−1}`; `Q = g^0·D_0 ⊕ … ⊕ g^{k−1}·D_{k−1}` (por byte).
- Recuperación (Anvin §2): un dato perdido con P (XOR); un dato perdido con Q (`D_x = (Q ⊕ Q_x) · g^{−x}`,
  donde `Q_x` es Q calculado con `D_x = 0`); dato y P perdidos (por Q y después P); dos datos perdidos
  `x, y` (sistema lineal de 2×2 con coeficientes precalculables por par).
- Anvin §4: con todos los miembros legibles, el par de síndromes permite **localizar** un único miembro
  corrupto (no solo reparar uno conocido). INF: útil como diagnóstico y como evidencia de detección (§7.4).
- Máximo de 255 discos de datos con este campo (Anvin §1).

### 3.5 Metadatos en disco que revelan parámetros

| Formato | Posición | Firma | Campos útiles | Fuente |
|---|---|---|---|---|
| md 0.90 | Bloque de 4 KiB alineado a 64 KiB, a ≥ 64 KiB y < 128 KiB del final (tamaño redondeado a 64 KiB menos 64 KiB) | (definida en `md_p.h`) | nivel, disposición, chunk, miembros | SPEC: wiki md, *superblock formats* |
| md 1.0 / 1.1 / 1.2 | Final / byte 0 / byte 4096 del miembro | `0xa92b4efc` LE en 0x00 | `level` 0x48, `layout` 0x4C, `size` 0x50, `chunksize` 0x58 (sectores de 512 B), `raid_disks` 0x5C, `data_offset` 0x80, `data_size` 0x88, `super_offset` 0x90, `dev_number` 0xA0, `dev_roles` desde 0x100 | SPEC: wiki md |
| DDF | Cabecera ancla en el **último bloque** direccionable de cada miembro | `0xDE11DE11` | registros de configuración: PRL, RLQ, tamaño de strip, miembros, extensiones; CRC-32 | SPEC: DDF §2.2.1, §5.3, §5.5, §5.9 |
| Contenedor GPT/MBR | Tabla del miembro | Tipo MBR 0xFD (autodetección md), 0x42 (LDM); GUID GPT de «Linux RAID» | inicio del contenedor = desplazamiento | SPEC: UEFI §5.3 (formato); tipos por convención pública |

- SPEC (`mdadm(8)`, `--data-offset`): con metadatos 1.x el desplazamiento de datos es variable y lo calcula
  `mdadm` (puede diferir entre versiones), así que debe leerse del superbloque, no suponerse.
- INF (M13): el «container» de M13 corresponde a estas estructuras; DESC: si DMDE lee metadatos de md, DDF o
  controladoras (el manual no lo dice; M16).

### 3.6 Paridad retrasada («delay») en la práctica pública

- SPEC-práctica (foros HDDGuru citados): ciertas controladoras HP/Compaq Smart Array usan «paridad
  retrasada»: la paridad permanece en el **mismo miembro durante varias filas consecutivas** antes de
  rotar; los usuarios citan valores como 4 o 16, y modelos de la misma marca que no la usan. Son
  testimonios, no especificación.
- DESC: no hay especificación pública del fabricante; no hay oráculo libre que cree arrays con delay.

## 4. Inferencias sobre DMDE

| # | Inferencia | Base |
|---|---|---|
| I1 | El modelo interno de DMDE es probablemente una **tabla de rotación** periódica (filas × discos) de la que las disposiciones estándar son casos particulares: la rotación personalizada expone exactamente esa tabla. | INF: M8, M2 |
| I2 | «Delay» = número de filas consecutivas que comparten la misma posición de paridad (y de datos) antes de avanzar la rotación; con delay `D` el periodo de la tabla pasa de `n` a `n·D` filas. | INF: M10, §3.6; DESC la definición exacta |
| I3 | RAID-6 se lee con P (XOR) y Q se trata como bloque sin uso; por eso solo admite un NULL. El ejemplo de M8 lo confirma (Q = −2). | INF: M7, M8 |
| I4 | El cálculo automático es una búsqueda sobre un espacio discreto (tamaño de bloque, rotación, orden, desplazamiento a pasos) con una puntuación estadística; necesita `n` exacto porque `k` y la tabla dependen de `n`. | INF: M12, M13 |
| I5 | Los miembros ausentes (NULL) son borrados conocidos: en RAID-0 y JBOD sus bloques se pierden; con paridad se reconstruyen. | INF: M3, M5, M9 |
| I6 | «Read ahead» es una caché de lectura de filas; desaconsejada con sectores defectuosos porque amplifica las lecturas en zonas dañadas. | INF: M10 |

## 5. Desconocido

- DESC: la definición exacta de delay en DMDE (unidad, si afecta también a la colocación de datos, si se
  aplica a RAID-6 igual que a RAID-5) y su valor por defecto.
- DESC: la métrica del cálculo automático, sus lecturas, sus límites y cómo presenta la confianza.
- DESC: el tratamiento de sectores ilegibles en un miembro presente (reconstrucción o relleno) y si DMDE
  distingue en el visor un sector reconstruido de uno leído.
- DESC: el comportamiento con miembros de tamaño distinto y con la fila parcial final.
- DESC: si se admiten coeficientes de Q y si se leen metadatos de md, DDF, LDM o controladoras.

Ninguna incógnita bloquea MemRescue: §6 a §9 definen un comportamiento propio, completo y comprobable.

## 6. Modelo de mapeo propio

### 6.1 Tabla de rotación compilada

Todas las disposiciones se compilan a una **tabla periódica** `T` de `R` filas × `n` columnas cuyas celdas son
`Dato(j)`, `P`, `Q` o `SinUso`, con `K` celdas de datos numeradas `0..K−1` exactamente una vez (formato
equivalente al de M8, ampliado con `Q` = −3 en el fichero de configuración).

- Mapeo lógico → físico: `L = ⌊x / c⌋`, `u = x mod c`, periodo `π = ⌊L / K⌋`, `j = L mod K`; la celda
  `(i, m)` con `Dato(j)` da el miembro `m` y el bloque físico `π·R + i`; el byte del miembro es
  `o_m + (π·R + i)·c + u`, con `o_m` el desplazamiento del miembro. Se precalcula el inverso
  `j → (i, m)` (vector de `K` entradas).
- Disposiciones estándar (§3.2, §3.3): `R = n·D`, y la fila `i` de la tabla usa la fórmula de su disposición
  con la fila de rotación `t = ⌊i / D⌋` en lugar de `s` (I2). `D = 1` reproduce exactamente md y DDF.
- RAID-0: `R = 1`, `K = n`, todas `Dato`. RAID-4: `parity-last` (o `parity-first`) con `R = 1`.
- Validación al compilar: cada fila con como mucho una `P` y una `Q`; `Q` solo si la fila tiene `P` (RAID-6
  de MemRescue); numeración de datos completa; `R·n ≤ 2^20` celdas (cota de memoria; con `n = 64` admite
  `D ≤ 256`).
- Variantes de delay alternativas (si el corpus real o el propietario lo requieren): se expresan como tabla
  personalizada sin tocar el motor.

### 6.2 Tamaño lógico y miembros

- Cada miembro es una `RangeView` (REPO: `mr-io::view`) sobre su origen con `offset` y `tamaño` en sectores
  (M10): la vista garantiza que nunca se lee fuera del contenedor.
- Todos los miembros presentes deben tener el mismo tamaño de sector lógico; `c` múltiplo de ese tamaño,
  entre un sector y 64 MiB.
- Niveles con bloques: filas útiles `F = ⌊min_m(tamaño_m) / c⌋`; tamaño lógico = (celdas de datos de las
  filas `0..F−1` de la tabla repetida) × `c`. Así un periodo incompleto al final aporta solo sus filas
  completas. Los bytes de cada miembro por debajo de un bloque completo no se exponen (DESC lo que hace
  DMDE).
- RAID-1: tamaño = mínimo de los miembros. JBOD: suma, en el orden dado; cada miembro con su `RangeView`.
- Miembro ausente: `Miembro::Ausente { tamaño }`. **No** se modela con `NullDevice`: este declara sus
  sectores `Sparse` (ceros fiables, REPO `mr-io::null`), lo que haría pasar por datos legítimos los bloques
  de un disco perdido. Un ausente es siempre un borrado.

### 6.3 Composición

`RaidDevice` implementa `BlockDevice`, así que se puede anidar (RAID 10 = RAID-0 de RAID-1, 50, 60) y abrir
como cualquier origen: particiones (incluida la búsqueda exprés de INV-05b, como hace DMDE según M17), NTFS,
visor. Mejora sobre lo documentado de DMDE (M16).

## 7. Reconstrucción en lectura y mapa de validez

### 7.1 Regla de fiabilidad

Para cada sector lógico se consideran los **sectores homólogos** de la fila (mismo desplazamiento dentro del
bloque en cada miembro de la fila). Estados de entrada (REPO: ADR-019): `Read`, `Recovered`, `Sparse` son
fiables; `Unreadable`, `Skipped` y cualquier sector de un miembro ausente son **borrados**.

1. Si el sector del dato es fiable, se entrega tal cual con su estado (un `Sparse` sigue siendo `Sparse`).
2. Si es un borrado y la fila tiene redundancia suficiente para el número de borrados **entre las celdas que
   intervienen en la ecuación** (datos, P y, en su caso, Q; las `SinUso` no cuentan), se reconstruye:
   XOR con P (un borrado), Anvin §2 con P y Q (hasta dos), RAID-1 desde otra copia fiable.
3. Un sector reconstruido es fiable **solo si todas sus entradas lo eran**; si no, el resultado es ilegible.
   Nunca se reconstruye con rellenos ni se mezclan rellenos en un XOR.
4. RAID-0 y JBOD: un borrado es ilegible (M3, M9).

### 7.2 Cómo se expresa con el contrato actual de `BlockDevice`

- `read_at` de `RaidDevice` devuelve los datos (leídos o reconstruidos) o `IoError::Read` con el rango lógico
  si algún sector pedido es irrecuperable. Es el mismo mecanismo de ADR-020: el lector tolerante
  (`mr_io::resilient`) relee entonces sector a sector y marca `Unreadable` con el relleno solo los
  irrecuperables. Las lecturas estrictas y los hashes fallan, como deben.
- `allocation` declara `Sparse` solo donde el miembro del dato está presente y declara `Sparse`; nunca para
  bloques de un ausente.
- Un sector reconstruido llega al consumidor como `Read`. Para no perder la información, `RaidDevice` lleva
  contadores (sectores reconstruidos, irrecuperables, filas con paridad incoherente) y la descripción del
  origen los muestra. Pregunta abierta 1: añadir un estado `Reconstruido` al mapa de validez (cambio de
  ADR-019 y de la ABI).
- Advertencia que se documenta en la interfaz (INF): la reconstrucción es correcta solo si la paridad estaba
  al día; un apagado con escrituras a medias (*write hole*) produce datos reconstruidos erróneos sin forma de
  detectarlo en RAID-5.

### 7.3 Camino de lectura acotado

1. La petición (≤ `MAX_READ_REQUEST`, REPO `mr-io`) se parte en segmentos que no cruzan un bloque; cada
   segmento se limita además a 1 MiB.
2. Camino rápido: miembro del dato presente y lectura correcta.
3. Si el miembro está ausente o la lectura falla: se lee el mismo segmento de los demás miembros de la fila
   que intervienen (con relectura sector a sector solo en los que fallen, sin reintentos propios: los
   reintentos los decide la política de E/S de M1 sobre la lectura lógica) y se aplica §7.1 sector a sector.
4. Memoria de trabajo: `n × segmento`, con el segmento reducido para que no pase de 16 MiB; tablas de GF de
   512 bytes estáticas; tabla de rotación ≤ 2^20 celdas.
5. *Read-ahead* (M10): caché LRU de segmentos por miembro, desactivada por defecto y desactivada siempre que
   haya habido un error de lectura en la sesión (I6).
6. Escritura: ninguna en M5 (todos los niveles de solo lectura). JBOD con escritura queda para M12 (RD-07).

### 7.4 Verificación de paridad (diagnóstico)

Opción para recorrer un rango comprobando `P` (y `Q`) cuando todo es legible: informa filas incoherentes;
en RAID-6 localiza el miembro discrepante (Anvin §4). Nunca corrige datos en silencio; se ofrece como
diagnóstico y como evidencia en §8.

## 8. Detección automática de parámetros (diseño propio)

### 8.1 Objetivos e invariantes

1. **Lecturas acotadas** por presupuesto explícito (bytes y operaciones), independientes del tamaño de los
   miembros; resultado parcial válido al agotarlo o cancelar.
2. **Dos fases separadas**: recogida de muestras con E/S, y evaluación de hipótesis **en memoria** sin E/S;
   solo las mejores hipótesis se verifican después con unas pocas lecturas dirigidas.
3. **Un sector no fiable nunca es evidencia** (ADR-019), ni a favor ni en contra.
4. **Determinista**: mismas entradas, mismas lecturas y mismo resultado.
5. **Nunca se aplica en silencio**: se presentan las hipótesis ordenadas con su confianza y su evidencia; el
   usuario elige (M15 muestra el coste de equivocarse).

### 8.2 Entradas y restricciones

`n` exacto (M12), lista de miembros presentes (los ausentes cuentan en `n`), y restricciones opcionales:
niveles, disposiciones, rango de `c` (por defecto potencias de dos de 4 KiB a 1 MiB), delays (por defecto
`{1}`; ampliable a `{1, 2, 4, 8, 16, 32}`), orden fijo total o parcial (M14), desplazamiento conocido por
miembro o paso de búsqueda (M13), convención de Q. Opciones de presupuesto: `max_bytes` (256 MiB por
defecto), `max_lecturas` (8192), `ventana` (64 KiB), `max_hipotesis` evaluadas (10^6).

### 8.3 Fase 0: metadatos

Se leen las posiciones de §3.5 en cada miembro (inicio, 4 KiB, final, último bloque y ventana final de
128 KiB): superbloques md, ancla DDF con CRC y tablas MBR/GPT del miembro. Unos metadatos coherentes entre
miembros (mismo identificador de conjunto, roles distintos, CRC correcta) proponen una hipótesis completa
con origen «metadatos»; no se aceptan sin la verificación de §8.7, pero fijan restricciones fuertes
(`dev_roles` da el orden; `data_offset` el desplazamiento).

### 8.4 Fase 1: muestras

- Clasificación barata por sector de 512 B: `cero`, `baja` (pocas clases de byte o texto), `alta` (entropía
  de bytes ≥ 7,0 bits; con 512 muestras el máximo práctico ronda 7,6), `ilegible`. Huella de 64 bits por sector para comparar sin guardar los datos.
- Ventanas de muestreo **alineadas por filas**: como la fila `s` está en el mismo desplazamiento
  `o_m + s·c` de todos los miembros, una ventana en el mismo desplazamiento relativo de cada miembro cubre
  filas completas sin conocer orden ni rotación. Se toman: el inicio (2 MiB), el final, 32 ventanas
  estratificadas por el tamaño y las ventanas dirigidas de §8.6.
- Anclas: en las ventanas se buscan estructuras que **declaran su posición lógica** o su continuidad:

| Ancla | Qué declara | Fuente |
|---|---|---|
| MBR en el sector 0 lógico; cabecera GPT con `MyLBA` = 1 y CRC; array de entradas con CRC-32 | Su posición absoluta; el array de 16 KiB cruza bloques si `c ≤ 8 KiB` y su CRC solo cuadra con el orden correcto | SPEC: UEFI §5.3.2 |
| Boot sector NTFS | Tamaño de clúster, LCN de `$MFT` y `$MFTMirr`, total de sectores (copia al final) | SPEC: `layout.h` (INV-05b §4) |
| Registro `FILE` de la MFT | Su número de registro (NTFS 3.1, cabecera 0x2C): posición relativa al inicio de la MFT | SPEC: `layout.h` |
| Registro `INDX` | VCN dentro de su índice | SPEC: `layout.h` |
| Copia de superbloque ext | `s_block_group_nr`: posición absoluta dentro del FS | SPEC: documentación ext4 |
| Superbloque md 1.x | `data_offset`, `dev_roles` | SPEC: wiki md |

### 8.5 Fase 2: hipótesis por pasos (cada paso acota el siguiente)

1. **Nivel y número de paridades** (prueba XOR). Por fila muestreada con al menos dos sectores no nulos y
   todos fiables: si el XOR de los `n` homólogos es cero en ≥ 99 % de las filas informativas, hay una paridad
   XOR (RAID-4/5); si es cero excluyendo exactamente un miembro por fila, es RAID-6 y ese miembro es Q (su
   posición por fila revela la rotación de Q); si nunca, RAID-0 o JBOD; si las huellas coinciden entre
   miembros, RAID-1. Con un miembro ausente en RAID-5 la prueba no informa (el ausente queda determinado por
   los demás): el nivel sale de la capacidad (siguiente punto) o de la restricción del usuario.
2. **Datos por fila `k`** (INF propia): un FS hallado declara su tamaño (NTFS total de sectores, ext número de
   bloques); `tamaño_FS / tamaño_útil_por_miembro ≈ k` (cota inferior), lo que distingue RAID-0 de RAID-5 con
   el mismo `n` incluso con un ausente.
3. **Tamaño de bloque `c`** (sin orden): en cada miembro, las transiciones de clase entre sectores
   consecutivos se concentran en múltiplos de `c` (cambia el fichero o el miembro de origen). Para cada `c`
   candidato se calcula el enriquecimiento `E(c)` = fracción de transiciones en múltiplos de `c` dividida por
   `512/c` (lo esperado al azar). Se elige el mayor `c` con `E(c)` significativo (prueba binomial,
   p < 10^−6) y `E(c) ≥ 0,8·E(c/2)`; los múltiplos de `c` heredan el enriquecimiento de `c` pero lo diluyen.
   Las anclas que cruzan bloques (array GPT, registros consecutivos) refinan o confirman.
4. **Desplazamientos** (M13): candidatos por miembro desde metadatos, contenedores, el sector del MBR o de la
   GPT lógica (solo puede estar en el miembro del dato 0 de la fila 0) y, si no hay nada, la rejilla del paso
   indicado dentro de una ventana inicial (por defecto paso 2048 hasta 16 MiB). Los desplazamientos iguales en
   todos los miembros se prueban primero.
5. **Orden, rotación y delay** (en memoria): cada hipótesis `H = (nivel, c, tabla, orden, desplazamientos,
   q_orden)` asigna a cada muestra una posición lógica; se evalúa con la puntuación de §8.6. Poda: las anclas
   de posición (registros `FILE` en miembros distintos, superbloques ext) fijan pares (miembro, fila) ↔
   bloque lógico y reducen el orden a pocas permutaciones antes de enumerar; con `n ≤ 8` y sin anclas se
   enumera todo (`8!` × 6 disposiciones × delays) dentro de `max_hipotesis`; por encima, solo con anclas o
   con orden fijado.

### 8.6 Puntuación

Puntuación = suma de log-verosimilitudes de evidencias independientes, con cada evidencia en tres estados
(`a favor`, `en contra`, `no verificable`):

| Evidencia | Prueba bajo `H` | Peso a favor / en contra |
|---|---|---|
| Estructura con CRC que cruza bloques (array GPT) | CRC correcta leyendo por `H` | +8 / −8 |
| Anclas de posición (`FILE`, `INDX`, ext) | Posición lógica predicha = declarada (por pares para cancelar el inicio desconocido de la MFT) | +2 / −4 por **frontera de bloque** distinta (no por registro) |
| Q de RAID-6 | `Q` calculada con la tabla, el orden y `q_orden` de `H` coincide | +3 / −6 por fila |
| Continuidad de clase en fronteras | Último sector del bloque lógico `L` y primero de `L + 1` de la misma clase, frente a la tasa de la línea base dentro de bloques | razón de verosimilitud por frontera, acotada a ±0,5 |
| Coherencia de FS | Boot NTFS y su copia al final, `$MFT` y `$MFTMirr` coherentes, GPT principal y de respaldo | +6 / −6 por estructura |

- Cobertura: una hipótesis solo puede ser `Alta` si las evidencias a favor cubren **todos los miembros y
  todas las fases de la rotación** (cada posición de paridad de la tabla vista al menos una vez; con delay,
  filas de al menos dos periodos).
- Niveles de confianza: `Alta` (cobertura completa, cero evidencias fuertes en contra, margen ≥ 10 sobre la
  segunda y verificación de §8.7 superada); `Media` (margen ≥ 4 sin cobertura completa); `Ambigua` (margen
  < 4: se muestran todas las empatadas, que suelen diferir en algo que los datos no distinguen, como el
  orden de miembros con bloques idénticos); `Insuficiente` (sin evidencias fuertes).
- La puntuación numérica ordena; la confianza se decide por estas reglas explicables (como en INV-05b §8.7).

### 8.7 Fase 3: verificación dirigida

Para las 5 mejores hipótesis se abre el `RaidDevice` y se leen estructuras concretas: tablas de particiones,
boot de cada volumen y su copia, primeros registros de la MFT y de `$MFTMirr`, superbloque ext y su copia del
grupo 1 (lecturas dirigidas por fila, dentro del presupuesto). Si la verificación contradice, la hipótesis
baja de nivel. Después se puede lanzar la búsqueda exprés de INV-05b sobre la mejor (M17).

### 8.8 Ilegibles, progreso y cancelación

- Los sectores no fiables se excluyen de clases, XOR, anclas y CRC (la evidencia queda `no verificable`).
- Tarea de `mr-api` cancelable; progreso por fase y por presupuesto (no por tamaño); resultado con estado
  `Completa`, `PresupuestoAgotado` o `Cancelada` y las hipótesis evaluadas.

### 8.9 Diferencias deliberadas respecto a lo observado

- RAID-6 con P y Q completos (hasta dos borrados), con las convenciones de Q de md y DDF parametrizadas.
- Estado de reconstrucción visible (contadores y, si se aprueba, estado propio en el mapa de validez).
- Lectura de metadatos md y DDF como pista; RAID anidados por composición.
- Confianza explicada con evidencias y cobertura, no solo una lista ordenada.

## 9. Diseño de `mr-raid` y correspondencia con PARIDAD

### 9.1 Estructura

- Crate `core/mr-raid`, dependiente solo de `mr-io`. Módulos: `config` (tipos y TOML), `table` (compilación y
  validación de tablas), `gf` (GF(2^8), tablas log/exp, recuperación de Anvin §2), `device` (`RaidDevice`),
  `verify` (§7.4), `detect` (§8), `meta` (lectores de superbloques md y DDF, solo lectura).
- Tipos principales: `RaidConfig { nivel, miembros: Vec<Miembro>, bloque_bytes, disposicion, delay,
  q_orden }`; `Miembro::Presente { origen: Arc<dyn BlockDevice>, offset_sectores, tamaño_sectores }` (envuelto
  en `RangeView`) o `Miembro::Ausente`; `Disposicion` con las de §3.2 y §3.3 más `Personalizada(Tabla)`.
- `RaidDevice::stats()` con reconstruidos, irrecuperables e incoherencias; `describe()` para la descripción
  del origen.
- Límites: `n ≤ 64` en niveles con bloques, `n ≤ 256` en JBOD; aritmética comprobada en todos los mapeos.
- API, FFI y CLI: `mr raid build --config raid.toml`, `mr raid detect …`, `mr raid verify …`; el fichero de
  configuración versionado (RD-10) admite la tabla personalizada con −1, −2 y −3.

### 9.2 Correspondencia

| ID | Cobertura en este diseño |
|---|---|
| RD-01 RAID 0 | §6.1 (`R = 1`), §7.1 punto 4 |
| RD-02 RAID 1 | §6.2, §7.1 (copia fiable), diagnóstico de copias divergentes en §7.4 |
| RD-03 RAID 4 | §3.2 `parity-last`/`parity-first`, §7 |
| RD-04 RAID 5 + delay | §3.2, §6.1 (`R = n·D`), I2 |
| RD-05 RAID 6 | §3.3, §3.4, §7 (dos borrados, mejora sobre M7) |
| RD-06 Rotación personalizada | §6.1 (tabla con −1, −2 y −3) |
| RD-07 JBOD | §6.2 (lectura en M5; escritura en M12) |
| RD-08 Offset, tamaño, NULL, read-ahead | §6.2 (`RangeView`, `Miembro::Ausente`), §7.3 punto 5 |
| RD-09 Cálculo automático | §8 |
| RD-10 Guardar/cargar | §9.1 (TOML versionado) |

## 10. Plan de pruebas

### 10.1 Generador sintético propio

Herramienta determinista (Python, en `tools/`, sin código común con `mr-raid`): recibe una imagen lógica
(contenido con semilla SHAKE-256 o imágenes de los corpus existentes, p. ej. las NTFS de ADR-024) y una
configuración, y escribe los miembros con su paridad (XOR y Q con ambas convenciones), con un manifiesto
(parámetros, SHA-256 de la imagen lógica y de cada miembro). Implementa las fórmulas de §3 de forma
independiente, más tablas personalizadas y delay. Escenarios:

| Escenario | Contenido | Esperado |
|---|---|---|
| Matriz de disposiciones | RAID-0/1/4/5/6, todas las disposiciones de §3.2 y §3.3, `n` 2 a 8, `c` 4 KiB a 1 MiB, delay 1, 4 y 16 | SHA-256 lógico idéntico al de la imagen de entrada |
| Personalizada | El ejemplo de 4 × 4 de M8 y tablas irregulares | Idem; tablas inválidas rechazadas al compilar |
| Ausentes | Cada miembro sustituido por `Ausente` (uno; dos en RAID-6) | SHA-256 idéntico; contadores de reconstruidos exactos |
| Ilegibles | `FaultyDevice` (REPO) en un miembro, y en dos miembros en la misma fila y en filas distintas | Reconstrucción donde hay redundancia; `Unreadable` exactamente en los sectores sin ella; nunca un relleno en un XOR |
| Ausente frente a cero | Miembro ausente y miembro `NullDevice` real | El ausente nunca produce `Sparse` |
| Desplazamientos y tamaños | Offsets distintos por miembro, miembros de tamaños distintos, fila parcial | Tamaño lógico según §6.2; ninguna lectura fuera de la `RangeView` |
| Paridad incoherente | Una fila con P alterada | §7.4 la informa; la lectura normal no cambia |
| Anidados | RAID-0 de RAID-1, RAID-0 de RAID-5 | SHA-256 idéntico |
| Detección | Parámetros ocultos sobre imágenes con NTFS, GPT, ext y contenido sin FS; con un ausente; con desplazamiento | Hipótesis correcta primera con `Alta` cuando hay FS; `Ambigua` o `Insuficiente` declarada (nunca `Alta` errónea) sin FS |
| Presupuesto, cancelación, determinismo | Límites pequeños; cancelar tras k lecturas; dos ejecuciones | Lecturas ≤ límite (envoltorio que cuenta lecturas), estados correctos, mismas lecturas |

Además: pruebas de propiedad (lógico → físico → lógico es la identidad; cada byte físico de datos tiene un
único lógico), pruebas de GF contra las tablas de DDF §4.2.19.2 y los ejemplos de Anvin, y fuzzing acotado de
la configuración y de los lectores de metadatos.

### 10.2 Oráculo independiente: md de Linux en un ejecutor desechable (propuesta de ADR)

Como el corpus NTFS (ADR-024), se propone un workflow `raid-corpus.yml` en un ejecutor Ubuntu desechable de
GitHub Actions:

1. Crea ficheros de miembros, los asocia a dispositivos de bucle y crea arrays con `mdadm --create` (niveles
   0, 1, 4, 5, 6; todas las disposiciones de `--layout`, incluidas las `ddf-*` y las `-6`; varios `--chunk`;
   metadatos 1.0, 1.2 y DDF con `--metadata=ddf`).
2. Escribe la imagen lógica completa a través de `/dev/mdX` (así toda la paridad es coherente), sincroniza y
   detiene el array.
3. Registra en el manifiesto lo que dice md (`mdadm --examine`, `--detail`: disposición, chunk,
   `data_offset`, roles) y el SHA-256 de la imagen lógica leída de vuelta por md.
4. Publica los miembros como artefacto con retención explícita, acciones fijadas por SHA de commit y
   `permissions: contents: read`; un script local los descarga y verifica.

La verdad la da md (otro implementador), no el generador ni MemRescue. La reproducibilidad es semántica (el
superbloque lleva identificadores y marcas de tiempo; se comparan imagen lógica, parámetros y bytes fuera de
los metadatos). Limitaciones: md no crea arrays con delay ni rotaciones de controladoras; el RAID-5 de
discos dinámicos de Windows necesitaría una VM Windows desechable (pregunta abierta 4).

## 11. Preguntas abiertas para el propietario

1. ¿Se añade un estado `Reconstruido` al mapa de validez (cambia ADR-019 y la ABI) o bastan los contadores y
   la descripción del origen en M5?
2. Delay: ¿se acepta la interpretación I2 (filas que comparten rotación) como única variante integrada, con
   las demás como tablas personalizadas, mientras no haya un array real de referencia?
3. RAID-6: ¿se implementan ya los dos borrados (mejora sobre DMDE) o solo P en la primera entrega?
4. ¿Se aprueba el workflow `raid-corpus.yml` con md como oráculo (nueva ADR) y una VM Windows desechable más
   adelante para el RAID-5 de discos dinámicos?
5. ¿Presupuesto por defecto de la detección (256 MiB, 8192 lecturas) y conjunto por defecto de delays
   probados (solo 1, o también 2 a 32)?
6. ¿Se exponen las filas parciales finales y el periodo incompleto, o solo filas completas como propone §6.2?
7. ¿Se leen metadatos de controladoras propietarias (cuando haya documentación pública) o solo md y DDF?
8. ¿Escritura en JBOD (RD-07) se mantiene para M12, como el resto de escrituras?
