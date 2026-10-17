# INV-05b · Generación de candidatos de la búsqueda exprés de particiones perdidas

Fecha: 2026-10-18 · Issue #16 (hito M2 · Particiones) · Continúa
[INV-05](INV-05-mbr-y-busqueda-de-particiones.md) §3 («bucle externo no verificado»).

> Sala limpia (ADR-001): en esta fase **no** se ha ejecutado DMDE ni se ha analizado su binario (ni
> decompilación, ni desensamblado, ni herramientas de ingeniería inversa). Las fuentes son el manual
> público de DMDE, especificaciones públicas de los formatos, convenciones públicas de alineación y los
> documentos ya existentes en este repositorio. El algoritmo de §8 es un diseño propio de MemRescue.

Leyenda:

- **OBS** = observado en la documentación pública de DMDE (se indica URL y sección).
- **REPO** = consta en un documento anterior de este repositorio (se indica documento y apartado); no se
  ha vuelto a verificar en esta fase.
- **SPEC** = especificación o convención pública (se indica URL y sección).
- **INF** = inferencia propia, razonada a partir de lo anterior.
- **DESC** = desconocido: no se puede afirmar con las fuentes permitidas.

## 1. Fuentes consultadas

| Fuente | URL | Uso |
|---|---|---|
| Manual de DMDE: *Partition Management* | <https://dmde.com/manual/partitions.html> | Comportamiento de la búsqueda exprés, indicadores |
| Manual de DMDE: *Device Selection* | <https://dmde.com/manual/devices.html> | Opción «Show partitions» al abrir |
| Manual de DMDE: *Find and Open Volume* | <https://dmde.com/manual/openvolume.html> | Relación lista de particiones / Full Scan |
| Manual de DMDE: *Full Scan* | <https://dmde.com/manual/fullscan.html> | Contraste con el escaneo completo |
| Manual de DMDE: *Device I/O Parameters* | <https://dmde.com/manual/devioparams.html> | Ausencia de parámetros de tamaño de sector |
| UEFI 2.10, cap. 5 (MBR y GPT) | <https://uefi.org/specs/UEFI/2.10/05_GUID_Partition_Table_Format.html> | §5.2.1, §5.2.3, §5.3.1, §5.3.2 |
| Microsoft, *exFAT file system specification* | <https://learn.microsoft.com/en-us/windows/win32/fileio/exfat-specification> | §3, §3.1.4, §3.1.5, §3.1.14, §3.1.20, §3.4 |
| Microsoft, *FAT32 File System Specification* 1.03 (fatgen103) | <https://download.microsoft.com/download/1/6/1/161ba512-40e2-4cc9-843a-923143f3456c/fatgen103.doc> | Sección «Boot Sector and BPB» |
| Comentarios públicos del controlador NTFS de Linux (`fs/ntfs/layout.h`) | <https://code.woboq.org/linux/linux/fs/ntfs/layout.h.html> | Posición de la copia del boot sector NTFS |
| ntfs-3g, corrección de `ntfsfix` sobre la copia del boot sector | <https://ognproject.evlt.uma.es/gitea/opengnsys/ntfs-3g/commit/8ae20018dfce1e3d37371a6e2c00805759bb54d2> | Variantes de posición de la copia NTFS |
| Documentación de ext4 del kernel de Linux | <https://www.kernel.org/doc/html/latest/filesystems/ext4/globals.html> y <https://www.kernel.org/doc/html/latest/filesystems/ext4/overview.html> | Superbloque y copias |
| Apple TN1150 (HFS Plus) | <https://developer.apple.com/library/archive/technotes/tn/tn1150.html> | *Volume Header* y copia alternativa |
| Apple File System Reference | <https://developer.apple.com/support/downloads/Apple-File-System-Reference.pdf> | `nx_superblock_t` |
| XFS Algorithms & Data Structures | <https://mirrors.edge.kernel.org/pub/linux/utils/fs/xfs/docs/xfs_filesystem_structure.pdf> | Superbloque y grupos de asignación |
| Btrfs, formato en disco | <https://btrfs.readthedocs.io/en/latest/dev/On-disk-format.html> | Superbloque y espejos |
| Microsoft, *Disk Partition Alignment Best Practices for SQL Server* | <https://learn.microsoft.com/en-us/previous-versions/sql/sql-server-2008/dd758814(v=sql.100)> | Alineación de 1 MiB por defecto desde Windows Vista / Server 2008 |
| Geometría CHS | <https://en.wikipedia.org/wiki/Cylinder-head-sector> | Convención de 63 sectores por pista y 255 cabezas |
| Documentos del repositorio | INV-05, `PARIDAD_DMDE_PRO.md` (PT-05, PT-06), `DECISIONES.md` (ADR-001, ADR-003, ADR-019) | Contexto y restricciones |

En esta sesión se recuperaron y leyeron las páginas del manual de DMDE y la especificación exFAT. El
resto son referencias públicas conocidas cuyos datos aquí citados (posiciones y campos) son estables y de
uso general; antes de implementar cada reconocedor se debe releer la sección citada (§10, pregunta 9).

## 2. Lo que dice el manual de DMDE (comportamiento visible)

| # | Afirmación | Etiqueta y fuente |
|---|---|---|
| M1 | La búsqueda exprés de particiones perdidas se ejecuta por defecto al abrir un dispositivo y al construir un RAID; se evita desmarcando la opción de mostrar particiones correspondiente. | OBS: partitions.html, *Partitions Window*; devices.html, opción *Show partitions* |
| M2 | Marcar la casilla «found» muestra las particiones perdidas y borradas y realiza la búsqueda exprés. | OBS: partitions.html, *Partitions Window* |
| M3 | Para una búsqueda más compleja y profunda, el manual remite a Full Scan. | OBS: partitions.html, *Partitions Window* |
| M4 | Indicadores: T tabla, E entrada, B boot sector del volumen, C copia del boot sector o de la GPT, F estructuras básicas del FS (p. ej. el primer registro de la MFT), f MFTMirr, x ausente o dañado; errores en rojo. | OBS: partitions.html, *Partitions Window* |
| M5 | Con particionado incorrecto o no estándar no se garantiza una visualización correcta. | OBS: partitions.html, *Partitions Window* |
| M6 | Al insertar (undelete) particiones encontradas se recomienda hacerlo del principio al final del disco; las particiones no válidas pueden impedir la inserción. | OBS: partitions.html, *Insert the Partition (Undelete)* |
| M7 | Un volumen abierto desde los resultados de Full Scan se añade a la lista de encontradas. | OBS: partitions.html |
| M8 | El manual no indica qué posiciones, alineaciones ni estructuras examina la búsqueda exprés, ni cómo trata el tamaño de sector o los desplazamientos. | OBS (por ausencia): partitions.html, devices.html, devioparams.html, openvolume.html |
| M9 | Full Scan, en cambio, recorre un área seleccionada y puntúa los volúmenes («%», «Check») por coherencia entre estructuras; tiene un modo de barrido «a saltos». | OBS: fullscan.html |

## 3. Lo que ya consta en el repositorio

- REPO (INV-05 §2.3): reglas de aceptación de un sector como tabla MBR/EBR, con un modo estricto para la
  búsqueda de tablas perdidas (firma obligatoria, al menos un tipo conocido, ningún tipo desconocido,
  ningún indicador de arranque anómalo).
- REPO (INV-05 §2.4): el sondeo de **una** posición candidata comprueba la estructura principal de varios
  FS y sus copias en posiciones fijas relativas al candidato (sector 6, sector 12, −1024 bytes, último
  sector del volumen y copias a distancias fijas de decenas o cientos de MiB), con opciones que activan
  cada familia de comprobaciones, mensajes de progreso por LBA y cancelación.
- REPO (INV-05 §3): el bucle que **genera** los candidatos quedó sin verificar. Esta fase no lo verifica
  tampoco (por diseño, sin acceso al binario); lo sustituye por un diseño propio (§8).

## 4. Especificaciones: dónde están las estructuras y sus copias

Posiciones relativas al inicio del volumen o del disco. `S` = bytes por sector declarados por la propia
estructura; `N` = número de sectores lógicos del dispositivo.

| Estructura | Principal | Copia o respaldo | Campos que revelan inicio o tamaño | Fuente |
|---|---|---|---|---|
| MBR | LBA 0, tabla en 0x1BE, firma 0x55AA en 0x1FE | Ninguna | `StartingLBA`, `SizeInLBA` por entrada | SPEC: UEFI §5.2.1 |
| MBR protector | LBA 0, entrada 0xEE desde LBA 1 | Ninguna | Tamaño de la entrada (hasta el final o 0xFFFFFFFF) | SPEC: UEFI §5.2.3 |
| EBR | Inicio de cada lógica menos el hueco; enlace relativo a la extendida | Ninguna | Entrada de datos relativa al EBR | REPO: INV-05 §1 |
| Cabecera GPT | LBA 1 | Último LBA del disco (`AlternateLBA`) | `MyLBA`, `AlternateLBA`, `FirstUsableLBA`, `LastUsableLBA`, CRC-32 | SPEC: UEFI §5.3.2 |
| Entradas GPT | Normalmente LBA 2 | Justo antes de la cabecera de respaldo | `PartitionEntryLBA`, CRC-32 del array | SPEC: UEFI §5.3.2, §5.3.3 |
| FAT12/16/32 | Sector 0 del volumen (BPB, 0x55AA) | FAT32: `BPB_BkBootSec` (offset 50), recomendado 6; FSInfo en `BPB_FSInfo` (normalmente 1) con su copia a continuación de la copia del boot | `BPB_BytsPerSec` (512 a 4096), `BPB_HiddSec` (offset 28, sectores que preceden a la partición), `BPB_TotSec16/32` | SPEC: fatgen103, *Boot Sector and BPB* |
| exFAT | Región de arranque principal, sectores 0 a 11 (suma de control en el 11) | Región de respaldo, sectores 12 a 23 | `PartitionOffset` (offset 64; 0 = ignorar), `VolumeLength` (offset 72), `BytesPerSectorShift` (offset 108, 9 a 12) | SPEC: exFAT §3, §3.1.4, §3.1.5, §3.1.14, §3.4 |
| NTFS | Sector 0 del volumen («NTFS    » en el offset 3) | Último sector de la partición desde NT 4 (en NT 3.51 y anteriores, a mitad del volumen); el recuento de sectores del boot excluye esa copia | Bytes por sector, sectores ocultos (offset 0x1C), total de sectores, LCN de `$MFT` y `$MFTMirr` | SPEC no normativa: comentarios de `layout.h`; ntfs-3g |
| ext2/3/4 | Superbloque a 1024 bytes del inicio, firma 0xEF53 | Copias en el grupo 1 y en potencias de 3, 5 y 7 (`sparse_super`) o en todos los grupos | `s_blocks_count`, `s_log_block_size`, `s_block_group_nr` | SPEC: documentación ext4 del kernel |
| XFS | Superbloque en el sector 0 («XFSB») | Superbloque secundario al inicio de cada grupo de asignación | Tamaño de bloque, bloques por AG, número de AG | SPEC: XFS Algorithms & Data Structures |
| HFS+/HFSX | Cabecera de volumen a 1024 bytes («H+» o «HX») | Cabecera alternativa a 1024 bytes del final del volumen | `blockSize`, `totalBlocks` | SPEC: TN1150, *Volume Header* |
| APFS | Superbloque del contenedor en el bloque 0 («NXSB») | Copias en el área de *checkpoints* | `nx_block_size`, `nx_block_count` | SPEC: Apple File System Reference |
| Btrfs | Superbloque a 64 KiB | Espejos a 64 MiB, 256 GiB y 1 PiB | Firma «_BHRfS_M», `total_bytes` | SPEC: Btrfs On-disk format |

Consecuencias útiles para generar candidatos (INF):

- **Copia al final**: un boot sector NTFS encontrado en la posición `p` y no en el inicio de un volumen
  sugiere el inicio `p − TotalSectors × S` (o `p − (TotalSectors + 1) × S` según cómo se haya
  colocado; ver ntfs-3g). Es la única estructura de respaldo que permite «volver hacia atrás» desde el
  final de un hueco.
- **Copia cercana**: las copias de FAT32 (sector 6) y exFAT (sector 12) están en los primeros 8 KiB del
  volumen con sectores de 512 bytes: una única lectura de 8 KiB en el candidato cubre principal y copia.
- **Autoubicación**: `BPB_HiddSec` (FAT y NTFS), `PartitionOffset` (exFAT) y `MyLBA` (GPT) declaran dónde
  creía estar la estructura al escribirse. La diferencia con la posición real mide un desplazamiento de
  la imagen o un error de quien la formateó. `BPB_HiddSec` es débil: en lógicas puede ser relativo al EBR
  y algunas herramientas escriben 0 (INF).
- **Extensión**: casi todos los FS declaran su tamaño, lo que permite encadenar el siguiente candidato
  al final del volumen confirmado.

## 5. Convenciones públicas de alineación

| Convención | Inicios típicos (sectores de 512 B salvo indicación) | Fuente |
|---|---|---|
| DOS / CHS | Primera partición en LBA 63; las demás en límite de cilindro (`k × 16065` con 255 cabezas y 63 sectores); cada lógica a 63 sectores de su EBR | SPEC-convención: geometría CHS |
| Windows Vista / Server 2008 y posteriores | Múltiplos de 1 MiB (primera en LBA 2048); lógicas a 1 MiB de su EBR | SPEC-convención: guía de alineación de Microsoft |
| Disco 4Kn (4096 B lógicos) | 1 MiB = 256 sectores; GPT en LBA 1 = byte 4096 | SPEC: UEFI §5.3.1 con `BlockSize` 4096 |
| Superfloppy / medio sin tabla | Volumen en LBA 0 | INF (práctica habitual en memorias USB y tarjetas) |
| Partición EFI / reservadas de Microsoft | Inicio en 1 MiB; MSR de 16 o 128 MiB delante de la de datos | INF (práctica habitual) |

## 6. Inferencias sobre la búsqueda exprés de DMDE

| # | Inferencia | Base |
|---|---|---|
| I1 | La búsqueda exprés es acotada: se ejecuta al abrir cualquier dispositivo sin pedir confirmación y el manual reserva la búsqueda profunda a Full Scan. Un barrido completo de un disco de varios TB sería incompatible con ese uso. | INF: M1, M3 |
| I2 | Las posiciones candidatas son pocas y «típicas»: inicios y finales declarados por tablas, posiciones de alineación habituales y posiciones derivadas de estructuras ya encontradas. | INF: M3, M8, REPO INV-05 §2.4 (sondeo con inicio y final esperado) |
| I3 | Cada candidato se confirma con la estructura principal y su copia; los indicadores B, C y F lo reflejan para el usuario. | INF: M4 y REPO INV-05 §2.4 |
| I4 | Las particiones mal alineadas o en posiciones no estándar pueden no aparecer o aparecer incorrectamente (de ahí el aviso de M5 y la remisión a Full Scan). | INF: M3, M5 |
| I5 | El orden de inserción recomendado (del principio al final) sugiere que los resultados se presentan ordenados por posición y que pueden solaparse. | INF: M6 |
| I6 | La copia NTFS al final es probablemente el medio para encontrar volúmenes cuyo inicio está dañado. | INF: M4 («C»), §4 |

## 7. Desconocido

- DESC: el orden real, la granularidad y la extensión de las posiciones que DMDE genera (si hay rejilla,
  de qué paso y hasta dónde; si explora los extremos del disco; si encadena desde el final de cada volumen).
- DESC: los límites de trabajo (número de lecturas, tiempo) y si dependen del tamaño del disco.
- DESC: cómo se resuelven solapamientos y cómo se decide «encontrada» frente a «coincide con la tabla».
- DESC: el tratamiento de desplazamientos de imagen y de discos 4Kn abiertos como 512 (el manual no lo
  menciona; REPO INV-05 §2.3 punto 9 solo muestra que las posiciones pueden ser negativas).
- DESC: qué hace con los sectores ilegibles durante la búsqueda.

Ninguna de estas incógnitas bloquea MemRescue: §8 define un comportamiento propio, completo y comprobable.

## 8. Propuesta de algoritmo propio para MemRescue (#15)

### 8.1 Objetivos e invariantes

1. **Nunca un barrido completo.** El trabajo está acotado por un presupuesto explícito de lecturas que no
   crece con el tamaño del disco (salvo un término logarítmico opcional, §8.5).
2. **Un sector ilegible nunca es evidencia**, ni a favor ni en contra (ADR-019): ni los rellenos ni los
   sectores `Unreadable`/`Skipped` pasan por un reconocedor.
3. **Determinista**: mismo origen y mismas opciones producen las mismas lecturas, en el mismo orden, y el
   mismo resultado.
4. **Solo lectura e informativo**: la búsqueda añade particiones «encontradas»; no modifica nada ni
   aplica en silencio desplazamientos o tamaños de sector alternativos.
5. **Aritmética comprobada** (`checked_*`) y posiciones en bytes con signo (`i128` interno) para poder
   representar volúmenes que empiezan antes del inicio de la imagen.
6. **Cancelable y con progreso** en todo momento; un resultado parcial es válido y lo dice.

### 8.2 Entradas y opciones

Entradas: `Source` de `mr-api` (lectura con mapa de validez), tamaño lógico de sector del dispositivo
`S_dev`, número de sectores `N`, y el resultado de los analizadores de #12 (MBR/EBR) y #13 (GPT),
incluidas las tablas con confianza `Suspicious` o `Invalid` (aportan pistas, no verdades).

| Opción | Valor por defecto | Significado |
|---|---|---|
| `max_lecturas` | 4096 | Operaciones de lectura totales (cada una de como mucho `ventana_sondeo`) |
| `max_bytes` | 32 MiB | Bytes leídos totales |
| `max_tiempo` | ninguno | Límite de tiempo opcional (resultado parcial al agotarse) |
| `max_volumenes` | 256 | Volúmenes confirmados tras los que se deja de encadenar |
| `ventana_sondeo` | 8 KiB | Bytes leídos en cada candidato (cubre sectores 0 a 15 de 512 B) |
| `rejilla_inicio` | 64 MiB | Ventana inicial del disco explorada con rejillas de alineación |
| `rejilla_final` | 16 MiB | Ventana final del disco explorada buscando copias al final |
| `cilindros` | 64 | Límites de cilindro CHS explorados (`k × 16065` y `+63`) |
| `sondeos_por_hueco` | 8 | Candidatos por hueco no explicado (§8.4, nivel P3) |
| `familias` | todas | FS activados: NTFS, FAT, exFAT, ext, XFS, HFS+, APFS, Btrfs, tablas MBR/GPT |
| `tamanos_sector` | {`S_dev`, 512, 4096} | Interpretaciones de sector probadas para tablas y copias al final |
| `copias_lejanas` | no | Lecturas de copias a distancias grandes (ext grupo 1, Btrfs 64 MiB, XFS AG 1) |

Todos los valores son configurables desde la API, la CLI y la UI; los defectos se documentan.

### 8.3 Unidades y tamaño de sector

- Los candidatos son **desplazamientos en bytes**. La LBA solo se usa para mostrar (progreso, resultado)
  con `S_dev`.
- Cada reconocedor obtiene `S_fs` de la propia estructura (FAT `BPB_BytsPerSec`, exFAT
  `BytesPerSectorShift`, NTFS bytes por sector, GPT según la posición de la cabecera) y calcula sus copias
  y su extensión en esas unidades.
- Si `S_fs ≠ S_dev`, el volumen se informa con la anomalía «tamaño de sector distinto del dispositivo»
  (típico de una imagen de un disco 4Kn leída como 512 o de un adaptador USB que traduce sectores).
- Las tablas se interpretan con cada `S` de `tamanos_sector`: una cabecera GPT en el byte 4096 de un
  dispositivo de 512 B propone `S = 4096` y regenera los candidatos de tabla con esa unidad.

### 8.4 Generación de candidatos por niveles

Cada candidato lleva: posición en bytes, nivel, origen (qué lo propuso), hipótesis opcional (FS
esperado, final esperado, `S` esperado) y un identificador estable para deduplicar.

| Nivel | Fuente | Candidatos |
|---|---|---|
| **P0** · declarados | Tablas de #12 y #13 (todas, también las sospechosas, las de respaldo GPT y las de EBR huérfanos) | Byte 0 (volumen sin tabla); inicio declarado de cada entrada; final declarado menos un sector (copia NTFS); inicio y final de las cabeceras GPT en `N − 1` con cada `S` |
| **P1** · derivados | Evidencia ya confirmada durante la búsqueda | Inicio inferido desde una copia (NTFS al final: `p − TotalSectors × S` y variante `+1`; FAT32: `p − BkBootSec × S`; exFAT: `p − 12 × S`); final del volumen confirmado alineado hacia arriba a 1 MiB, a cilindro y `+63`, y sin alinear; tablas MBR/EBR encontradas en modo estricto (sus entradas pasan a P1); posiciones corregidas por un desplazamiento aceptado (§8.8) |
| **P2** · rejilla inicial | Convenciones de §5 | 63; `k × 16065` y `k × 16065 + 63` para `k ≤ cilindros`; múltiplos de 1 MiB dentro de `rejilla_inicio`; los mismos con `S = 4096` coinciden en bytes y se deduplican |
| **P3** · huecos | Espacio no explicado entre volúmenes confirmados y hasta el final del disco | Por hueco `[a, b)`: `a` alineado a 1 MiB, a cilindro y `+63`; `b − S` (copia NTFS de un volumen que termina en `b`); `b − S` en los dos límites de 1 MiB anteriores a `b`; hasta `sondeos_por_hueco` |
| **P4** · rejilla final | Final del disco | Último sector con cada `S`; último sector de cada MiB dentro de `rejilla_final` (copias NTFS y cabeceras GPT de respaldo de discos recortados o ampliados) |

Reglas comunes:

- Un candidato de P2 a P4 que cae **dentro** de un volumen ya confirmado como `Valid` se descarta al
  sacarlo de la cola (evita falsos positivos con imágenes de disco guardadas como ficheros y ahorra
  lecturas). Los de P0 y P1 nunca se descartan por esta regla.
- Un candidato en posición negativa o más allá del final no se lee; si procede de evidencia (P1), se
  conserva en el resultado como «volumen parcial fuera de la imagen».
- Los huecos (P3) se recalculan cada vez que se confirma un volumen; así se encadenan particiones
  consecutivas aunque estén mucho más allá de `rejilla_inicio`, sin rejilla global.

### 8.5 Orden, deduplicación y presupuesto

- Cola de prioridad con clave `(nivel, posición, id)`: primero todo lo declarado, luego lo derivado (que se
  inserta dinámicamente y adelanta a las rejillas), luego rejillas y huecos. Dentro de un nivel, por
  posición ascendente, que favorece la localidad en discos mecánicos.
- Deduplicación por `(posición, S)`; las hipótesis distintas en la misma posición se fusionan en un solo
  sondeo.
- Caché LRU pequeña de ventanas leídas (p. ej. 64 × 8 KiB): sondeo y confirmación comparten sectores.
- El presupuesto cuenta lecturas físicas reales (no aciertos de caché). Reservas internas para no gastar
  todo en rejillas: al menos el 25 % de `max_lecturas` queda para P0 y P1, y ningún nivel P2 a P4 puede
  consumir más del 40 %.
- Cota del trabajo con los defectos: P2 ≈ 64 + 2 × 64 + 1 ≈ 193 sondeos; P4 ≈ 3 + 16; P0 y P1 lineales
  en el número de entradas y de volúmenes (≤ `max_volumenes` × (3 confirmaciones + `sondeos_por_hueco`)).
  El total queda por debajo de 4096 lecturas en casos normales y nunca las supera. Con 10 ms por lectura
  aleatoria en un disco mecánico el peor caso es de unos 40 s; en SSD o imagen, décimas de segundo.
- Al agotar el presupuesto, el estado final es `PresupuestoAgotado` con el número de candidatos sin
  sondear por nivel. Nunca se continúa en silencio.

### 8.6 Sondeo de un candidato

1. Leer `ventana_sondeo` (8 KiB) en la posición con lectura tolerante. Con sectores de 512 B esto cubre:
   boot sector (0), FSInfo (1), copia FAT32 (6) y FSInfo de respaldo (7), región principal exFAT completa
   (0 a 11) e inicio de la de respaldo (12 a 15), superbloque ext y cabecera HFS+ (1024), XFS y APFS (0).
   Con 4096 B cubre los sectores 0 y 1 (boot y cabecera GPT si el candidato es el byte 0).
2. Aplicar el mapa de validez: cada reconocedor declara qué rangos de bytes necesita; si alguno no está
   en estado `Read`, `Recovered` o `Sparse`, el reconocedor devuelve `NoVerificable` sin mirar los bytes.
   (`Sparse` es dato legítimo: ceros que simplemente no coinciden con ninguna firma.)
3. Ejecutar los reconocedores activos, empezando por el FS que sugiere la hipótesis (tipo de la entrada de
   tabla, tipo de la copia encontrada). Cada uno devuelve una hipótesis de volumen `{fs, inicio, S_fs,
   extensión, posiciones de copia, campos clave}` o nada. Los reconocedores validan rangos y coherencia
   interna (potencias de dos, extensión dentro de límites razonables, LCN de `$MFT` dentro del volumen,
   suma de control exFAT, etc.).
4. Si ningún FS coincide en el byte 0 del candidato, probar la tabla MBR/EBR en **modo estricto**
   (INV-05 §2.3) y, en el byte 0 del disco y en `N − 1`, la cabecera GPT.
5. Si el candidato es P0 y la principal falla o no es verificable, intentar las copias cercanas con
   `S = 4096` (bytes 24576 y 49152), que no caben en la ventana: una lectura más cada una.

### 8.7 Confirmación y puntuación de evidencia

Para cada hipótesis se leen, si no están ya en caché, sus copias declaradas (NTFS al final según
`TotalSectors` y, si hay entrada de tabla, también el último sector de la entrada; HFS+ a −1024 del
final; ext grupo 1 y XFS AG 1 solo con `copias_lejanas`) y una estructura básica:

| FS | Estructura básica (indicador F/f) |
|---|---|
| NTFS | Registro 0 de `$MFT` con firma «FILE» (F) y registro 0 de `$MFTMirr` coincidente (f) |
| FAT12/16/32 | Primer elemento de la FAT con el byte de medio del BPB; copia de la FAT si cabe en el presupuesto |
| exFAT | Suma de control de la región de arranque (sector 11) y entrada del mapa de bits en el directorio raíz |
| ext | Descriptor del grupo 0 coherente con el superbloque |
| HFS+ / APFS / XFS | Coherencia de tamaño de bloque y número de bloques con el espacio disponible |

Elementos de evidencia, siempre con resultado triestado (`Sí`, `No`, `NoVerificable`):

| Clave | Evidencia | Peso para ordenar |
|---|---|---|
| `B` | Estructura principal válida | 4 |
| `C` | Copia válida y **coherente** con la principal (mismos campos clave: geometría, extensión, número de serie o identificador de volumen; en exFAT, mismas sumas salvo `VolumeFlags` y `PercentInUse`) | 3 |
| `F`, `f` | Estructuras básicas válidas | 2 |
| `T`, `E` | Hay tabla y una entrada coincide en inicio y tamaño (tolerancia: final declarado igual o mayor que la extensión del FS) | 2 |
| `U` | Autoubicación coherente (`BPB_HiddSec`, `PartitionOffset` o `MyLBA` igual a la posición real) | 1 |
| `A` | Inicio alineado a una convención de §5 | 1 |

Reglas de confianza (ADR-003, `Confidence`):

- `Valid`: `B ∧ C`, o `B ∧ F ∧ E`.
- `Suspicious`: solo `B`; solo `C` con la principal legible e inválida (anomalía «principal dañada,
  candidata a restaurar desde la copia», enlaza con PT-08); `B` y `C` legibles pero incoherentes; o
  cualquier `Valid` con una anomalía de §8.3 o §8.8.
- `Invalid`: no se informa como encontrada; queda en el registro de diagnóstico.
- Si un elemento es `NoVerificable`, el indicador correspondiente se muestra como **«?» (no verificable)**,
  distinto de **«x» (ausente o dañado)**: el usuario sabe que falta evidencia por lectura, no porque la
  estructura esté mal. Un volumen con `B = NoVerificable` y `C = Sí` es `Suspicious`, nunca `Valid`.

La puntuación numérica solo ordena y desempata; la confianza se decide por las reglas anteriores, que son
explicables y comprobables.

### 8.8 Imágenes desplazadas

- Fuentes de evidencia de un desplazamiento `δ` (en bytes): cabecera GPT en `p` con `MyLBA = m`
  (`δ = p − m × S`); FS con `PartitionOffset` o `BPB_HiddSec` no nulos (`δ = p − valor × S`); volumen
  confirmado cuyo tamaño coincide con una entrada de tabla y cuyo inicio difiere en `δ`.
- Se acepta la **hipótesis** «imagen desplazada `δ`» cuando al menos dos evidencias independientes
  coinciden en `δ ≠ 0` y ninguna entrada es coherente con `δ = 0`. `BPB_HiddSec` de una lógica cuenta
  solo como evidencia débil (puede ser relativo al EBR).
- Con la hipótesis aceptada se añaden a P1 los inicios declarados más `δ`. Las particiones resultantes
  llevan la anomalía «desplazamiento detectado `δ`» y se presentan junto a las de `δ = 0`; nunca se
  reescribe la tabla ni se aplica `δ` en silencio (coherente con el plan de #13 para PT-03).
- `δ < 0` (imagen recortada por el principio): los volúmenes cuyo inicio queda fuera se informan como
  parciales a partir de sus copias al final; no se leen posiciones negativas.

### 8.9 Sectores ilegibles

- Los reconocedores reciben solo rangos con estado fiable; un patrón de relleno que imite una firma
  («NTFS    », 0x55AA, «EFI PART») no puede producir un volumen.
- Una principal ilegible convierte la copia en la única evidencia (`Suspicious`); una copia ilegible deja
  `C = ?`. En ambos casos se añade la anomalía `SectorIlegible` con la LBA.
- Las lecturas fallidas cuentan para el presupuesto; los reintentos los decide la política de E/S de
  M1, no la búsqueda.

### 8.10 Resultado y resolución de conflictos

- Cada volumen encontrado se compara con las entradas de las tablas: si coincide, se marcan `T`/`E` sobre
  la entrada existente y **no** se duplica; si no, se añade como `Encontrada` con su origen (nivel y
  evidencia que lo propuso).
- Solapamientos: se conservan todos los volúmenes `Valid`; entre un `Valid` y un `Suspicious` solapados,
  el segundo se marca «solapada con …» pero no se oculta (el usuario decide, como en M6).
- Orden de presentación: por posición ascendente y, a igualdad, por puntuación.
- Estado final: `Completa`, `PresupuestoAgotado` o `Cancelada`, con recuentos de lecturas, bytes, sectores
  no verificables y candidatos pendientes por nivel.

### 8.11 Progreso y cancelación

- Tarea de `mr-api` con token de cancelación comprobado antes de cada lectura; la cancelación devuelve el
  resultado parcial con estado `Cancelada`.
- Progreso con: lecturas hechas / `max_lecturas`, nivel actual, LBA del candidato (con `S_dev`) y
  volúmenes confirmados. Frecuencia limitada (p. ej. cada 100 ms o cada 32 lecturas). El porcentaje se basa
  en el presupuesto, no en el tamaño del disco, porque la búsqueda no recorre el disco.

### 8.12 Diferencias deliberadas respecto a lo observado

- Indicador «?» separado de «x» para lo no verificable por lectura (DMDE solo documenta «x»).
- Límites y presupuestos explícitos y visibles para el usuario.
- Desplazamientos y tamaños de sector alternativos detectados y comunicados como hipótesis.
- Tipo 0x85 aceptado como extendida (propuesta ya recogida en INV-05 §4).

## 9. Plan de pruebas (corpus de #14)

Los nombres de escenario son propuestos; se alinean con los definitivos cuando #14 se fusione. Cada
escenario se genera con el generador determinista (manifiesto con SHA-256) y los boot sectors son
sintéticos según la especificación (limitación ya documentada en INV-05 §6). Todas las pruebas usan un
envoltorio de `Source` que **cuenta lecturas** para comprobar el presupuesto.

| Escenario | Contenido | Resultado esperado |
|---|---|---|
| `lost_ntfs_mbr_zeroed` | MBR a cero; NTFS en LBA 2048 con copia en el último sector | 1 volumen NTFS `Valid` (B, C, F), origen P2 (rejilla 1 MiB) |
| `lost_fat32_chs` | Tabla borrada (entradas a cero, firma intacta); FAT32 en LBA 63 con copia en el sector 6 | FAT32 `Valid` (B, C), alineación CHS |
| `lost_exfat_1mib` | Tabla borrada; exFAT en 1 MiB con regiones principal y de respaldo | exFAT `Valid` (B, C, suma de control) |
| `lost_chain_three` | NTFS, FAT32 y exFAT consecutivos, el tercero fuera de `rejilla_inicio` (con la opción reducida para caber en ≤ 64 MiB) | Los tres, el último por encadenamiento P1/P3, no por rejilla |
| `ntfs_primary_damaged` | Boot NTFS principal a cero, copia al final intacta | `Suspicious`, B = x, C = sí, anomalía «restaurable desde copia», encontrado desde P4/P3 |
| `fat32_primary_damaged` / `exfat_primary_damaged` | Principal corrupta, copia en el sector 6 / región en el 12 intacta | `Suspicious`, C = sí |
| `primary_unreadable` | Como el anterior pero con la principal marcada ilegible (`FaultyDevice`) y un relleno que imita «NTFS    » y 0x55AA | B = «?», nunca se reconoce el relleno; `Suspicious` por la copia |
| `backup_unreadable` | Copia ilegible | C = «?», `Suspicious` |
| `image_shifted_pos` / `image_shifted_neg` | Imagen con k sectores añadidos / recortados al inicio; MBR o GPT y volúmenes con `BPB_HiddSec`/`PartitionOffset` | Hipótesis `δ` aceptada con ≥ 2 evidencias, comunicada; volúmenes parciales en el caso negativo |
| `gpt_4kn_lost` | Variante de `gpt_4kn` sin GPT principal y con exFAT/NTFS de 4096 B | Volumen encontrado con `S = 4096`; al abrir como 512, anomalía de tamaño de sector |
| `superfloppy` | FS en LBA 0 sin tabla | 1 volumen en el byte 0 |
| `table_intact` | `mbr_basic` y `gpt_basic` sin daños | Sin encontradas duplicadas; T/E/B/C sobre las entradas |
| Negativos | Disco a cero, aleatorio, todo relleno de ilegible, sector con aspecto de MBR dentro de datos | 0 encontradas; la tabla falsa no supera el modo estricto |
| Presupuesto | `max_lecturas` pequeño | Estado `PresupuestoAgotado`, lecturas ≤ límite, resultado parcial determinista |
| Cancelación | Cancelar tras la k-ésima lectura | Estado `Cancelada`, resultado parcial, sin lecturas posteriores |
| Escala | Mismo contenido en imágenes dispersas de 1 GiB y de 2 TiB | Mismo resultado y número de lecturas casi igual (independiente del tamaño) |
| Determinismo | Dos ejecuciones | Misma secuencia de lecturas y mismo resultado |

Además: pruebas unitarias por reconocedor (válido, cada campo fuera de rango, sumas de control) y fuzzing
acotado de los reconocedores (sin pánicos, sin bucles).

## 10. Preguntas abiertas

1. ¿Valores por defecto de `rejilla_inicio`, `rejilla_final` y `max_lecturas`? Se proponen 64 MiB,
   16 MiB y 4096; conviene medirlos en hardware real lento (M9, INV-06).
2. NTFS: ¿probar siempre ambas variantes de la copia al final (`TotalSectors` y `TotalSectors + 1`) y la
   de NT 3.51 (mitad del volumen)? Se propone las dos primeras por defecto y la tercera solo con opción.
3. ¿Activar `copias_lejanas` por defecto para ext y XFS? Cuestan una lectura lejana por volumen; se
   propone desactivado en exprés y activado al abrir un volumen concreto.
4. ¿Cómo se presenta en la UI un volumen parcial fuera de la imagen (δ negativo)?
5. ¿Entra APFS/Btrfs en M2 o se pospone su reconocedor a M8 con el resto de FS no NTFS?
6. Particiones dinámicas (LDM, 0x42) e híbridos: solo se informan como encontradas; ¿basta con eso en M2?
7. ¿Hace falta un oráculo independiente para el corpus de esta búsqueda (formateadores reales en WSL o VM)
   antes de marcar PT-05 como probado?
8. ¿Debe la búsqueda exprés ejecutarse automáticamente al abrir (como DMDE, M1) o solo a petición? Se
   propone automática con presupuesto reducido y botón para repetirla con el presupuesto completo.
9. Releer las secciones citadas de fatgen103, TN1150, la referencia de APFS y la documentación de ext4 y
   XFS al implementar cada reconocedor (en esta fase solo se recuperaron el manual de DMDE y la
   especificación exFAT).
