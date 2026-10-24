# INV-03 · Escaneo completo: candidatos de volumen, puntuación y reconstrucción

Fecha: 2026-10-24 · Issue #29 (hito M6 · Escaneo completo y reconstrucción) · Alimenta `mr-scan`
(SC-01 a SC-05, SC-11) y la parte de M6 de `mr-vfs` (SC-07 a SC-09).

> Sala limpia (ADR-001): en esta fase **no** se ha ejecutado DMDE ni se ha analizado su binario (ni
> decompilación, ni desensamblado, ni cadenas, ni herramientas de ingeniería inversa). El punto de
> partida que cita `ROADMAP.md` para INV-03 (funciones del binario) **no** se ha consultado. Las fuentes
> son el manual público de DMDE, especificaciones públicas de los sistemas de archivos, la documentación
> pública de herramientas libres (TestDisk, The Sleuth Kit) para la práctica general y los documentos y
> el código de este repositorio. Del manual solo se recogen hechos breves con su cita; el diseño de §8 es
> propio de MemRescue.

Leyenda:

- **OBS** = observado en la documentación pública de DMDE (se indica URL y sección).
- **REPO** = consta en un documento o en el código de este repositorio (se indica dónde).
- **SPEC** = especificación o documentación pública de terceros (se indica URL y sección).
- **INF** = inferencia propia, razonada a partir de lo anterior.
- **DESC** = desconocido: no se puede afirmar con las fuentes permitidas.

## 1. Fuentes consultadas

| Fuente | URL | Uso |
|---|---|---|
| Manual de DMDE: *Full Scan* | <https://dmde.com/manual/fullscan.html> | Objetivo, parámetros, barrido a saltos, columnas, guardar/cargar, versiones |
| Manual de DMDE: *Virtual Reconstruction* | <https://dmde.com/manual/reconstruction.html> | Modos, más/menos resultados, grupos de fragmentos, opciones NTFS y FAT |
| Manual de DMDE: *Find and Open Volume* | <https://dmde.com/manual/openvolume.html> | Abrir una variante encontrada; RAID y cifrado antes del escaneo |
| Manual de DMDE: *File Panel* | <https://dmde.com/manual/filepanel.html> | Carpetas virtuales, directorios encontrados, ficheros sin nombre ni padre |
| Manual de DMDE: *Partition Management* | <https://dmde.com/manual/partitions.html> | Relación con la búsqueda exprés (ya resumida en INV-05b §2) |
| TestDisk, *Step By Step* | <https://www.cgsecurity.org/wiki/TestDisk_Step_By_Step> | Práctica general: búsqueda rápida y profunda, copias de arranque |
| TestDisk, *Advanced NTFS Boot and MFT Repair* | <https://www.cgsecurity.org/wiki/Advanced_NTFS_Boot_and_MFT_Repair> | Práctica general: reconstruir el arranque NTFS desde la MFT |
| The Sleuth Kit, `sigfind` | <https://www.sleuthkit.org/sleuthkit/man/sigfind.html> | Práctica general: firmas en un desplazamiento fijo de cada bloque |
| libfsntfs, documentación del formato NTFS | <https://github.com/libyal/libfsntfs/blob/main/documentation/New%20Technologies%20File%20System%20(NTFS).asciidoc> | §3.1 (ficheros 0 a 15), §5.1.1 (cabecera del registro), §5.2 (indicadores) |
| Microsoft, *exFAT file system specification* | <https://learn.microsoft.com/en-us/windows/win32/fileio/exfat-specification> | §3 (región de arranque), §6.3.3 (`SetChecksum`), §7.1, §7.2, §7.6 |
| Microsoft, *FAT32 File System Specification* 1.03 (fatgen103) | <https://download.microsoft.com/download/1/6/1/161ba512-40e2-4cc9-843a-923143f3456c/fatgen103.doc> | *Boot Sector and BPB*, *FAT Directory Structure* (entradas `.` y `..`) |
| Documentación de ext4 del kernel de Linux | <https://www.kernel.org/doc/html/latest/filesystems/ext4/checksums.html>, <https://www.kernel.org/doc/html/latest/filesystems/ext4/group_descr.html>, <https://www.kernel.org/doc/html/latest/filesystems/ext4/ifork.html> | §2.5 *Checksums*, §3.2 *Block Group Descriptors*, §4.2.3 *Extent Tree* |
| XFS, *Self Describing Metadata* (kernel) | <https://www.kernel.org/doc/html/latest/filesystems/xfs/xfs-self-describing-metadata.html> | §3.2, §3.4 (`magic`, `crc`, `uuid`, `owner`, `blkno`, `lsn`) |
| XFS Algorithms & Data Structures | <https://mirrors.edge.kernel.org/pub/linux/utils/fs/xfs/docs/xfs_filesystem_structure.pdf> | Superbloque y cabeceras de grupo de asignación (no releído en esta sesión) |
| Apple TN1150 (HFS Plus) | <https://developer.apple.com/library/archive/technotes/tn/tn1150.html> | *Volume Header*, *B-Trees* (`BTNodeDescriptor`, `BTHeaderRec`) |
| Apple File System Reference | <https://developer.apple.com/support/downloads/Apple-File-System-Reference.pdf> | `obj_phys_t`, `nx_superblock_t` (el PDF no se pudo convertir a texto en esta sesión: §11, pregunta 10) |
| Btrfs, formato en disco | <https://btrfs.readthedocs.io/en/latest/dev/On-disk-format.html> | Cabecera de nodo (`csum`, `fsid`, `bytenr`, `generation`, `level`), superbloque y espejos |
| Documentos del repositorio | INV-05b, INV-01, `formatos/busqueda-particiones.md`, `formatos/ntfs.md`, `formatos/mapa-de-validez.md`, `PARIDAD_DMDE_PRO.md` (§5), `DECISIONES.md` (ADR-001, ADR-008, ADR-016, ADR-019, ADR-023, ADR-025 a ADR-027), `core/mr-store` | Contexto, restricciones y piezas existentes |

## 2. Lo que dice el manual de DMDE (comportamiento visible)

| # | Afirmación | Etiqueta y fuente |
|---|---|---|
| E1 | El escaneo completo busca y reconstruye virtualmente la estructura de directorios de un FS dañado; si no hay FS utilizable, puede buscar ficheros por firma (RAW). | OBS: fullscan.html, *Full Scan* |
| E2 | Si el FS no está muy dañado, se puede abrir un volumen **antes de que termine** el escaneo; unos indicadores de calidad ayudan a elegir. | OBS: fullscan.html, *Full Scan* |
| E3 | Para particiones normales se recomienda abrir el dispositivo físico y escanear toda el área que la partición puede ocupar, no el disco lógico dañado. | OBS: fullscan.html, *Full Scan* |
| E4 | RAID, almacenamiento cifrado e imágenes de máquinas virtuales deben abrirse antes según *Open Volume*; si no, los resultados de FS pueden ser incorrectos. | OBS: fullscan.html; openvolume.html |
| E5 | Un mismo volumen puede tener **varias versiones**, relevantes tras un movimiento, división, fusión o redimensionado interrumpidos o un cambio de tamaño de clúster; si faltan ficheros, se prueba otra versión. | OBS: fullscan.html, *Full Scan* |
| E6 | Parámetros: área de escaneo, FS y parámetros para reunir datos y **calcular volúmenes**. | OBS: fullscan.html, *Parameters* |
| E7 | El barrido «a saltos» recorre todo el disco de forma selectiva y gradual y encuentra primero los volúmenes con muchos ficheros, para ver los resultados principales sin esperar al final. | OBS: fullscan.html, *Parameters* |
| E8 | Columnas «%» y «Check»: estiman la calidad de recuperación como porcentaje y número de **enlaces correctos** entre estructuras del FS y ficheros hallados por firma (NTFS, exFAT, HFS); en el resto de FS solo se comprueban estructuras del FS. | OBS: fullscan.html, *Columns* |
| E9 | «Min. Size»: estimación del tamaño mínimo del área que ocupan los ficheros del volumen, contando solo los mejores fragmentos del FS. | OBS: fullscan.html, *Columns* |
| E10 | Indicadores `+` presente y `-` ausente o dañado: B/C arranque y copia; en FAT, F tabla FAT y copia y R directorio raíz. | OBS: fullscan.html, *Columns* |
| E11 | Start/Stop: si los parámetros no cambian, el escaneo **continúa**. Save/Load guardan y cargan el registro de búsqueda (para continuar o reutilizar resultados); *Log-file* guarda automáticamente estado y resultados. | OBS: fullscan.html |
| E12 | El menú permite cambiar el orden de la lista de volúmenes. | OBS: fullscan.html |
| E13 | Las firmas RAW también sirven para validar resultados de NTFS, exFAT y HFS y marcar ficheros probablemente recuperables; la carpeta de RAW filtrada excluye el espacio ya reclamado por resultados de FS. | OBS: fullscan.html, *Raw File Search* |
| E14 | La reconstrucción virtual usa los resultados del escaneo completo si existe; sin él hay un escaneo rápido del volumen (solo el FS original) y un reescaneo del volumen en su formato. | OBS: reconstruction.html |
| E15 | Botones de más/menos resultados con un indicador de calidad y cantidad: más resultados adicionales suele significar menos calidad. | OBS: reconstruction.html |
| E16 | Los fragmentos del escaneo se agrupan por probabilidad de pertenecer al volumen abierto: mejores, probables, relacionados (otras versiones), desconocidos, ajenos (otro volumen), pequeños (sin estadística) e incompatibles (otros parámetros del FS). | OBS: reconstruction.html, *Volume FS Fragments* |
| E17 | Opciones NTFS: filtro por número de registro MFT (reconstrucción parcial con poca memoria), fusión de INDX mejorada con marcas de tiempo, proceso automático o completo de INDX, `$LogFile`, registros MFT **desplazados** (requiere escaneo completo), extras y registros técnicos. | OBS: reconstruction.html, opciones avanzadas |
| E18 | FAT: usar FAT1, FAT2 o ninguna (se prueban y se elige automáticamente) y no usar sectores malos; en exFAT la única FAT no se puede comprobar bien. | OBS: reconstruction.html |
| E19 | ReFS v3: los nodos de directorio llevan un identificador de volumen; los de otro identificador se listan aparte. | OBS: reconstruction.html |
| E20 | Panel de ficheros: carpetas virtuales (raíz, metadatos, ficheros sin nombre ni padre, RAW) y directorios «encontrados» con el número de bloque del FS entre corchetes; marca para directorios de contenido incompleto. | OBS: filepanel.html |
| E21 | Abrir una variante encontrada es el tercer método, para los casos más complicados; el manual no da criterios para elegir entre variantes. | OBS: openvolume.html |
| E22 | El manual no describe el formato del registro de escaneo, cómo se agrupan los registros en volúmenes, la fórmula de «%» ni el orden del barrido a saltos. | OBS (por ausencia): fullscan.html, reconstruction.html |

## 3. Práctica general de otras herramientas

| # | Hecho | Fuente |
|---|---|---|
| G1 | La búsqueda profunda de TestDisk examina cada cilindro y busca estructuras de respaldo (copia del arranque FAT32 y NTFS, superbloque de respaldo de ext2/3) y avisa cuando un volumen se encontró por su copia. | SPEC: TestDisk *Step By Step*, *Deeper Search* |
| G2 | Con ambos arranques NTFS dañados, TestDisk busca la MFT y `$MFTMirr`, lee el tamaño de registro, **calcula el tamaño de clúster** y genera un arranque nuevo, dejando listar los ficheros antes de escribir. | SPEC: TestDisk *Advanced NTFS Boot and MFT Repair*, *Rebuilding An NTFS Boot Sector* |
| G3 | `sigfind` busca una firma en un desplazamiento fijo dentro de cada bloque de tamaño configurable (512 por defecto) para encontrar arranques, superbloques y tablas perdidos. | SPEC: TSK `sigfind` |

INF: la práctica pública coincide en tres ideas: comprobar firmas en desplazamientos fijos de cada
sector, usar las estructuras de respaldo como evidencia independiente y deducir la geometría (inicio,
tamaño de clúster) de la propia metadata cuando el arranque falta. El diseño de §8 las generaliza.

## 4. Lo que ya existe en el repositorio

- REPO (`core/mr-store`, ADR-008, ADR-016): base SQLite de trabajo con esquema versionado
  (`PRAGMA user_version`, migraciones hacia delante), WAL, `synchronous=FULL`, `quick_check` al abrir,
  propietario único (`<base>.lock`), trabajos con máquina de estados (`running` huérfano pasa a
  `interrupted` al abrir), `job_checkpoints (job_id, seq, position, state_json)` y `job_events`. Regla:
  solo metadatos, progreso y estado de reanudación; nunca contenido recuperado.
- REPO (`formatos/mapa-de-validez.md`, ADR-019): lector tolerante con estado por sector (`Read`,
  `Recovered`, `Unreadable`, `Skipped`, `Sparse`); `Sparse` son ceros legítimos que `allocation()` declara
  sin datos y no se leen; memoria acotada (búfer ≤ 64 MiB, `ValidityMap` ≤ `MAX_MAP_RUNS` tramos).
- REPO (`formatos/busqueda-particiones.md`, ADR-023): búsqueda exprés acotada con reconocedores
  (`Recognizer`) de NTFS, FAT y exFAT, indicadores `E P C S U A` con `+ x ? o -`, confianza por reglas y
  puntuación que solo ordena. Sus limitaciones remiten al escaneo completo de M6.
- REPO (`formatos/ntfs.md`, ADR-025 a ADR-027): validación estricta del arranque, registros `FILE` con
  fixups sin tolerancia y estado por registro, `decode_runs` con aritmética comprobada, `$MFTMirr`
  comparado, borrados con padre `live`/`deleted`/huérfano. Dos limitaciones explícitas para M6: «no se
  busca la MFT si el registro 0 falla en las dos copias» y «si el registro 0 falla en las dos copias, el
  volumen no se abre (lo cubrirá el escaneo de M6)».
- REPO (INV-01 §7.2): `mr-vfs` con categorías `vivo`, `borrado`, `huérfano`, `metadatos` y reservadas
  `encontrado` (M6) y `raw` (M7); carpetas sintéticas con nombres propios; arena en memoria con límite y
  la posibilidad de moverla a SQLite en M6.

## 5. Especificaciones: estructuras clasificables y cómo se ubican

Para cada FS interesa qué bloques se reconocen solos (firma), cómo se comprueba su integridad sin
contexto, qué campo permite **deducir dónde empieza el volumen** desde la posición `p` del bloque y qué
identifica al volumen (para separar volúmenes y versiones).

| FS | Bloque reconocible | Integridad propia | Ecuación de anclaje (INF salvo indicación) | Identidad | Fuente |
|---|---|---|---|---|---|
| NTFS | Arranque («NTFS    », 0x55AA) | Reglas del BPB (REPO ntfs.md) | `inicio = p` (principal) o `p − total × S` y variante `+1` (copia) | Número de serie del BPB | REPO; INV-05b §4 |
| NTFS | Registro `FILE`/`BAAD` | Secuencia de actualización (fixups), cabecera coherente | Con número propio `n` (offset 0x2C, NTFS 3.1) y tamaño `rs`: `a = p − n × rs` es constante en un tramo contiguo de la MFT | Registros 0 a 15 de metadatos; `$Volume` (3) | SPEC: libfsntfs §5.1.1, §3.1 |
| NTFS | Registro de índice `INDX` | Fixups | Con su VCN (cabecera) y los runs de `$INDEX_ALLOCATION` del directorio dueño: `p = inicio + LCN(VCN) × c` | Directorio dueño (registro) | SPEC no normativa: `layout.h` (cabecera `INDEX_BLOCK`); INF |
| FAT12/16/32 | BPB; sector de la FAT (byte de medio en `FAT[0]`); cluster de directorio con `.` y `..` | Ninguna suma; coherencia del BPB y de las entradas | `.` declara el clúster `k` del propio directorio: `p = D + (k − 2) × c` con `D` inicio de la región de datos | Número de serie del BPB (`BS_VolID`) | SPEC: fatgen103, *Boot Sector and BPB*, *FAT Directory Structure* |
| exFAT | Región de arranque (12 sectores, suma en el 11); conjuntos de entradas de directorio | `SetChecksum` del conjunto; suma de la región de arranque | `FirstCluster` de las entradas (mapa de bits 0x81, mayúsculas 0x82, extensión de flujo 0xC0) contra la posición de los datos: `p = inicio + ClusterHeapOffset × S + (k − 2) × c` | `VolumeSerialNumber` | SPEC: exFAT §3, §6.3.3, §7.1, §7.2, §7.6 |
| ext2/3/4 | Superbloque (0xEF53) y copias; bloque de extents (0xF30A); inodos | Con `metadata_csum`: crc32c de superbloque, descriptores, mapas, inodos (incluye UUID, **número de inodo** y generación), bloques de extents y de directorio | Copia del superbloque con `s_block_group_nr = g`: inicio a `g × bloques_por_grupo` bloques (más `s_first_data_block`) antes; un inodo cuya suma solo cuadra con el número `i` fija la posición de su tabla | `s_uuid` | SPEC: ext4 §2.5, §3.2, §4.2.3 |
| XFS v5 | Superbloque («XFSB») por grupo; bloques de metadatos autodescritos | crc32c | `blkno` es la propia posición del bloque: `inicio = p − blkno × 512` | `uuid` (comprobado contra el superbloque) | SPEC: XFS *Self Describing Metadata* §3.2, §3.4 |
| HFS+/HFSX | Cabecera de volumen («H+»/«HX») y alternativa; nodos de B-tree | Sin sumas en los nodos; coherencia de `kind`, `height`, `numRecords`, `fLink`/`bLink` | Nodo `k` de un árbol de nodos de tamaño `ns`: `p = base_del_fichero + k × ns`; los enlaces `fLink`/`bLink` deben apuntar a nodos encontrados en `p ± ns × Δ` | Fechas y `finderInfo` de la cabecera (INF) | SPEC: TN1150, *Volume Header*, *B-Trees* |
| APFS | Superbloque del contenedor («NXSB»), objetos con `obj_phys_t` | Fletcher-64 de cada objeto | Para objetos físicos el identificador es la dirección de bloque: `inicio = p − oid × tamaño_bloque` | UUID del contenedor | SPEC (pendiente de releer el PDF, §11 pregunta 10) |
| Btrfs | Superbloque a 64 KiB (espejos a 64 MiB y 256 GiB); nodos de árbol | `csum` de la cabecera | `bytenr` es **lógico** (pasa por el árbol de chunks): solo ancla tras reconstruir el mapa de chunks; el superbloque sí está en posición física | `fsid` | SPEC: Btrfs *On-disk format* |
| ReFS | DESC: no hay especificación pública suficiente | DESC | DESC | Identificador de volumen en nodos de directorio (OBS E19) | OBS: reconstruction.html |

Consecuencias (INF):

1. **Los mejores anclajes son autodescriptivos**: XFS v5 (`blkno`), APFS (oid físico) y los registros
   MFT de NTFS 3.1 (número propio) dicen dónde deberían estar. Un único bloque fija una hipótesis de
   inicio; muchos bloques coincidentes la confirman.
2. **Cuando el bloque no dice su posición** (FAT, HFS+, INDX), la posición se deduce de referencias
   cruzadas (`.` de un directorio, VCN de un INDX, `fLink` de un nodo) y hacen falta **dos** referencias
   independientes para resolver las dos incógnitas habituales (inicio y tamaño de clúster).
3. **Las sumas de control eliminan casi todos los falsos positivos** (exFAT, ext4, XFS v5, APFS, Btrfs);
   donde no las hay (FAT, HFS+, NTFS sin fixups distintos de cero) la coherencia entre bloques sustituye
   a la suma y la puntuación debe reflejarlo.
4. **Ningún sector no fiable puede clasificarse** (ADR-019): un relleno `BAD!`/`SKIP` nunca es firma.

## 6. Inferencias sobre el escaneo completo de DMDE

| # | Inferencia | Base |
|---|---|---|
| I1 | El escaneo recorre un área (no posiciones típicas, a diferencia de la búsqueda exprés) y acumula estructuras de FS sueltas; los volúmenes se **calculan** a partir de ellas, no se leen de una tabla. | INF: E1, E6, E3 |
| I2 | Los candidatos se recalculan durante el escaneo (se pueden abrir antes de terminar), así que el cálculo de volúmenes es incremental o repetible sobre lo acumulado. | INF: E2, E7 |
| I3 | «Versión» es una variante de geometría o de contenido del mismo volumen lógico; la identidad se conserva y cambian inicio, tamaño o tamaño de clúster. | INF: E5, E16 («relacionados», «incompatibles») |
| I4 | «%» y «Check» miden enlaces comprobados entre estructuras (y con ficheros RAW en algunos FS), no solo presencia de estructuras; los indicadores B/C/F/R miden presencia. | INF: E8, E10 |
| I5 | El estado guardado incluye lo encontrado y la posición del barrido, porque al cargarlo se puede continuar. | INF: E11 |
| I6 | El barrido a saltos favorece los volúmenes con muchos ficheros porque sus estructuras ocupan mucho espacio y un muestreo disperso las toca antes. | INF: E7 |
| I7 | Los «registros desplazados» de NTFS son registros `FILE` encontrados fuera de la posición que les corresponde según la MFT reconstruida (copias antiguas, desfragmentación, otra versión). | INF: E17, E5 |
| I8 | La reconstrucción con poca memoria se resuelve parcialmente (filtro por número de registro), lo que sugiere que el árbol completo se mantiene en memoria. | INF: E17 |

## 7. Desconocido

- DESC: la fórmula exacta de «%», «Check» y «Min. Size», y cómo se ponderan los enlaces.
- DESC: el orden del barrido a saltos, su granularidad y cuándo se considera cubierta un área.
- DESC: el formato y el contenido del registro de escaneo (no se busca compatibilidad: ADR-008).
- DESC: cómo se agrupan registros en candidatos y cómo se separan las versiones.
- DESC: el tratamiento de sectores ilegibles durante el escaneo.
- DESC: el comportamiento con ReFS más allá de E19.

Ninguna incógnita bloquea M6: §8 define un comportamiento propio, completo y comprobable.

## 8. Diseño propio para MemRescue (M6)

### 8.1 Invariantes

1. **Solo lectura**; nada del escaneo modifica el origen. Un arranque o una MFT «reconstruidos» existen
   solo en memoria y en la base, nunca se escriben (la escritura es de M12 con ADR-006).
2. **Un sector no fiable nunca es evidencia** (ADR-019): ni a favor ni en contra. Los clasificadores
   reciben solo bytes en estado `Read`, `Recovered` o `Sparse`.
3. **Memoria acotada e independiente del tamaño del origen** (§8.10). Lo que crece con el disco va a la
   base, no a la RAM.
4. **Reanudable sin repetir trabajo confirmado** (§8.11): todo lo confirmado en la base corresponde a
   área cubierta; lo no confirmado se vuelve a leer.
5. **Determinista**: mismo origen, área y opciones producen los mismos hallazgos, candidatos y
   puntuaciones, con independencia de interrupciones, del modo (lineal o a saltos) y del número de hilos.
6. **Explicable**: cada candidato lleva sus evidencias y sus enlaces comprobados; la confianza se decide
   por reglas, la puntuación solo ordena (como ADR-023).
7. **Aritmética comprobada** y posiciones en bytes con signo (`i128` interno), como la búsqueda exprés.

### 8.2 Arquitectura

Crate nuevo `core/mr-scan` (capa de análisis, sobre `mr-io`, `mr-part`, `mr-store` y los lectores de FS).
Cada FS aporta su escáner en su crate (`mr-fs-ntfs::scan`, más adelante `mr-fs-fat::scan`, etc.)
implementando rasgos de `mr-scan`; `mr-scan` no conoce los formatos.

```
planificador de tramos ──► lector tolerante (mapa de validez) ──► clasificadores por FS
        ▲                                                              │ hallazgos
        │ cobertura                                                    ▼
   base de escaneo (SQLite) ◄──── agregador de corridas (memoria acotada)
        │
        ├──► agrupador de candidatos ──► puntuación ──► lista de volúmenes y versiones
        └──► reconstrucción (abrir candidato) ──► `mr-vfs` (árbol con huérfanos y encontrados)
```

Rasgos propuestos (nombres orientativos):

| Rasgo | Responsabilidad |
|---|---|
| `RecordClassifier` | Dado un tramo de bytes fiables y su posición, emite hallazgos (`Hit`) con tipo, posición, campos clave y resultado de integridad. Sin E/S: puro y probado con fuzzing. |
| `RunAggregator` | Funde hallazgos consecutivos con la misma clave de anclaje en **corridas** (§8.5). |
| `CandidateBuilder` | Propone hipótesis de volumen a partir de corridas y estructuras únicas (arranques, superbloques) y resuelve su geometría (§8.7). |
| `CandidateVerifier` | Comprueba enlaces de un candidato leyendo de forma acotada (§8.8). |
| `VolumeReconstructor` | Abre un candidato como volumen virtual para `mr-vfs` (§8.9). |

### 8.3 Planificador: área, tramos y orden

- **Área**: rango de bytes del origen (por defecto todo el dispositivo; E3) y, opcionalmente, los huecos
  sin volumen válido de la tabla. Se divide en **tramos** de tamaño fijo `T` (defecto 1 MiB, alineado al
  sector del dispositivo). La unidad de cobertura es el tramo.
- **Lineal**: tramos en orden ascendente; mejor para discos mecánicos.
- **A saltos** (SC-02): orden de **inversión de bits** del índice del tramo (secuencia de van der Corput
  sobre `⌈log2(n)⌉` bits, descartando índices ≥ `n`). Propiedades: determinista, sin estado más allá del
  índice de la secuencia, cada prefijo cubre el área de forma uniforme con densidad creciente y al final
  cubre todos los tramos exactamente una vez. Un volumen cuya metadata ocupa una fracción `f` del área se
  toca, en esperanza, tras unos `1/f` tramos (I6).
- **Expansión local**: cuando un tramo produce una corrida que llega a su borde, se encolan los tramos
  vecinos con prioridad (una MFT de 1 GiB se lee entera en cuanto se toca, sin esperar a la secuencia).
  La cola de expansión está acotada (p. ej. 4096 tramos) y es determinista (orden por posición).
- **Lecturas**: bloques de `L` tramos (defecto 4 MiB) con la lectura tolerante; si el contenedor declara
  `Sparse` un tramo entero, se marca cubierto sin leer (las imágenes dispersas de varios TB cuestan solo
  sus datos reales).
- **Cobertura**: mapa de bits de tramos (1 bit por MiB: 10 TB son 1,25 MB) con un estado resumido por
  tramo en la base (`cubierto`, `cubierto_con_ilegibles`, `disperso`). Los sectores no fiables concretos
  quedan en un mapa de tramos `(lba, n, estado)` como el de `ValidityMap`.

### 8.4 Clasificadores por FS

Cada clasificador declara su granularidad (desplazamientos que examina) y su ventana:

| FS | Se examina | Firma rápida | Validación completa (solo tras la firma) |
|---|---|---|---|
| NTFS | Cada sector | «NTFS    » en 3; «FILE»/«BAAD»/«INDX» en 0 | Arranque: reglas de ntfs.md. `FILE`: fixups por tramo (`apply_fixups` de ADR-025, sin tolerancia), cabecera, atributos hasta el fin; se extraen número propio, secuencia, indicadores, referencia al base, `$FILE_NAME` (padre, nombre) y runs de `$DATA`/`$INDEX_ALLOCATION` resumidos. `INDX`: fixups y cabecera de índice; VCN propio |
| FAT | Cada sector | 0x55AA con BPB; `FAT[0]` con byte de medio; entrada `.` + `..` en el byte 0 de un sector | BPB de fatgen103; directorio: nombres válidos, atributos, clúster `.` ≥ 2, `..` coherente |
| exFAT | Cada sector | «EXFAT   » en 3; tipos de entrada 0x81/0x82/0x83/0x85 | Suma de la región de arranque; `SetChecksum` de cada conjunto 0x85 |
| ext | Bloques de 1 KiB a 64 KiB | 0xEF53 en 1024+56 del bloque; 0xF30A; inodos en tablas | crc32c cuando hay `metadata_csum` (el UUID sale del superbloque ya visto; hasta entonces el hallazgo queda «pendiente de UUID») |
| XFS | Cada sector | «XFSB», magias de AGF/AGI y nodos | crc32c; `blkno` y `uuid` |
| HFS+ | Cada sector | «H+»/«HX» en 1024; descriptor de nodo plausible | Reglas de TN1150 (tipo, altura, recuentos, desplazamientos de registros decrecientes al final del nodo) |
| APFS | Bloques de 4 KiB (y el tamaño de `nx_block_size` visto) | Fletcher-64 válido | Tipo y subtipo conocidos |
| Btrfs | Bloques de 4 KiB | `csum` válido (crc32c u otro según el superbloque) | `fsid` coherente con un superbloque visto |

Reglas comunes:

- La firma rápida es una comparación de pocos bytes; el coste dominante es la E/S. La validación completa
  solo se ejecuta tras la firma.
- Un hallazgo cuya ventana incluye un sector no fiable no se emite; se cuenta como «posible no
  verificable» en la cobertura del tramo.
- Un hallazgo guarda **posiciones y campos**, nunca los bytes (ADR-008): el contenido se vuelve a leer
  del origen al reconstruir.
- M6 entrega NTFS (SC-05) y los arranques/superbloques de todos los FS que ya reconoce la búsqueda
  exprés (NTFS, FAT, exFAT); el resto llega con su lector (SC-06, M8, INV-04 por FS) sin cambiar `mr-scan`.

### 8.5 Corridas: compactación de hallazgos

Una MFT contigua de un millón de registros no debe ser un millón de filas. El agregador funde hallazgos
consecutivos en **corridas**:

| Tipo | Clave de anclaje | Una corrida es |
|---|---|---|
| NTFS `FILE` | `(rs, a = p − n × rs)` | Registros con números consecutivos a distancia `rs` (permitiendo huecos de registros vacíos o dañados, que se cuentan) |
| NTFS `INDX` | Tamaño de registro de índice | Registros de índice contiguos del mismo directorio dueño (si consta) |
| FAT directorio | `(c, D)` cuando ya se conoce; si no, cada directorio es único | Directorio con sus clústeres contiguos |
| ext/XFS/APFS/Btrfs | `(uuid, inicio deducido)` | Bloques consecutivos con el mismo anclaje |

Cada corrida guarda: tipo, clave, posición inicial y final, primer y último número, recuentos (válidos,
dañados por fixups o suma, vacíos, no verificables) y un resumen de campos útil para agrupar (p. ej. en
NTFS: número de serie del arranque si se conoce, rango de LSN, número de registros en uso). Los hallazgos
**singulares** con mucha información (arranques, superbloques, registros 0 a 15 de NTFS, raíces de
árboles, cabeceras de grupo) se guardan uno a uno con sus campos clave.

Estimación (INF): un disco con unos pocos volúmenes produce del orden de miles de corridas y decenas de
miles de singulares; el peor caso (un disco muy reformateado) se acota con `max_corridas` y
`max_singulares` (defectos propuestos 10 millones y 1 millón) con estado `LimiteAlcanzado` explícito.

### 8.6 Base de escaneo (migración de `mr-store`)

El escaneo es un **trabajo** de `mr-store` (`kind = 'full_scan'`, parámetros en `params_json`, progreso
en `progress_done/total` en tramos). Una migración (versión 2 del esquema) añade tablas propias, todas
con `job_id` y `ON DELETE CASCADE`:

| Tabla | Contenido |
|---|---|
| `scan_area` | Área, tamaño de tramo, modo, sector del dispositivo, familias activas, huella del origen (tamaño, sector, SHA-256 si existe en `sources`) |
| `scan_coverage` | Tramos cubiertos como rangos `(desde, hasta, estado)` fusionados; el mapa de bits en memoria se reconstruye de aquí al reanudar |
| `scan_unreliable` | Rangos de sectores `Unreadable`/`Skipped` vistos (como `ValidityMap`, acotado) |
| `scan_runs` | Corridas (§8.5): `fs`, `kind`, `key_a`, `key_b`, `pos_first`, `pos_last`, `num_first`, `num_last`, recuentos, `summary_json` pequeño |
| `scan_singletons` | Estructuras únicas: `fs`, `kind`, `pos`, `integrity`, `identity`, `fields_json` |
| `scan_candidates` | Candidatos calculados: `fs`, identidad, inicio, tamaño, sector, unidad de asignación, versión, confianza, puntuación, indicadores, `evidence_json` |
| `scan_links` | Recuentos de enlaces por candidato y tipo (§8.8), no enlaces individuales |

Reglas:

- **Transacción por lote**: los hallazgos de un bloque de lectura y su cobertura se confirman en la
  **misma** transacción, seguida de un `job_checkpoint` con la posición en la secuencia del planificador
  (`position` = índice de la secuencia, `state_json` = cola de expansión pendiente). Así la base nunca
  dice «cubierto» sin sus hallazgos ni al revés (invariante 4).
- Índices por `(job_id, fs, key_a, key_b)` en corridas e `(job_id, fs, identity)` en singulares: el
  agrupador trabaja con consultas `GROUP BY`, no con todo en memoria.
- **Guardar/cargar** (SC-03): la base de trabajo **es** el registro de escaneo. «Guardar como» copia la
  base con la API de copia en caliente de SQLite; «cargar» abre otra base (propietario único, ADR-016) y
  comprueba la huella del origen antes de continuar (si no coincide, solo lectura de resultados, con
  aviso). Exportación opcional a JSON para informes y pruebas.
- Los candidatos son **derivados**: se pueden recalcular desde corridas y singulares en cualquier
  momento (tras cambiar opciones de puntuación, sin volver a escanear).

### 8.7 Agrupación de candidatos y geometría

Un candidato es `(fs, identidad, inicio, tamaño, S, unidad de asignación)` más sus evidencias. Se
construye en tres pasos, repetibles durante el escaneo (I2) cada `k` lotes o a petición.

**Paso 1: hipótesis por anclaje.** Cada singular y cada corrida aporta hipótesis de inicio con sus
ecuaciones de §5. Ejemplos NTFS:

- Arranque principal en `p`: inicio `p`, `c` y `rs` del BPB, LCN de `$MFT` y `$MFTMirr`, total.
- Arranque copia en `p`: inicio `p − total × S` (y `+1`), con los mismos campos.
- Corrida `FILE` con clave `a`: si contiene el registro 0, su `$DATA` da los runs de la MFT
  `(VCN_i, LCN_i)`. Para el fragmento `i` de la MFT, `a_i = inicio + (LCN_i − VCN_i) × c`. Con dos
  fragmentos encontrados: `c = (a_i − a_j) / ((LCN_i − VCN_i) − (LCN_j − VCN_j))`, válido solo si es una
  potencia de dos entre 512 B y 2 MiB; después `inicio = a_0 − LCN_0 × c`. Con un solo fragmento
  (MFT contigua) se necesita otra referencia: `$MFTMirr` (registro 1, LCN del espejo) encontrado como
  corrida de 4 registros en `q = inicio + LCN_mirr × c`, el `$Boot` (registro 7, `$DATA` en el LCN 0) o
  un `INDX` del directorio raíz (registro 5) en `inicio + LCN(VCN) × c`. Es la generalización propia de
  G2 (TestDisk deduce `c` de la MFT) a varias referencias que votan.
- Sin registro 0 pero con corridas de `FILE` y algún `INDX` situado: se prueban los `c` posibles; cada
  `c` que hace coincidir al menos dos referencias independientes produce una hipótesis.
- Tamaño del volumen (INF): total del arranque; si falta, el máximo de: final de la última corrida
  atribuida, `$Bitmap` (registro 6: `tamaño × 8 × c`) y el flujo `$Bad` de `$BadClus` (registro 8), cuyo
  tamaño declarado es el del volumen según la práctica pública (§11, pregunta 7).

FAT: con dos directorios cuyos clústeres `.` son `k1`, `k2` en `p1`, `p2`: `c = (p2 − p1)/(k2 − k1)` y
`D = p1 − (k1 − 2) × c`; un arranque en `p` con `D' = p + (reservados + nFAT × tamañoFAT) × S` igual a
`D` confirma la geometría; las FAT encontradas en `p + reservados × S` y siguiente confirman F/F2.
exFAT, ext, XFS, APFS: ecuaciones de §5 con su identidad.

**Paso 2: votación.** Las hipótesis se agrupan por `(fs, identidad, inicio, S, c)` exactos (no hay
tolerancia: una geometría distinta es otro candidato). Cada grupo suma votos ponderados por la calidad
del aportante (singular con suma de control o fixups > corrida grande > corrida pequeña > hallazgo sin
integridad). Los grupos con un único voto de baja calidad no se presentan (quedan en la base).

**Paso 3: volúmenes y versiones.** Los candidatos con la misma identidad (número de serie, UUID) y
geometría distinta (inicio, tamaño o `c`) son **versiones** del mismo volumen (E5, I3): se presentan
juntos, ordenados por puntuación, con la diferencia explícita («inicio desplazado 1 MiB», «clúster de
4 KiB frente a 8 KiB», «tamaño mayor: posible ampliación»). Sin identidad (FAT sin BPB), la versión se
decide por solape de las corridas atribuidas.

### 8.8 Puntuación, indicadores y confianza

**Atribución.** Cada corrida se atribuye a un candidato si su anclaje es coherente con su geometría
(p. ej. `a = inicio + (LCN − VCN) × c` para algún run del registro 0, o posición dentro del volumen con
el número propio esperado). Una corrida puede ser coherente con varios candidatos (versiones); se
atribuye al de mayor puntuación y se cuenta como «relacionada» en los demás.

**Enlaces comprobados** (I4; equivalente propio de «Check»). Se comprueban de forma acotada, por
muestreo determinista si hay muchos (defecto: hasta 65 536 por candidato, semilla fija):

| FS | Enlace correcto |
|---|---|
| NTFS | El `$FILE_NAME` de un registro cita un padre que es un directorio en uso con la misma secuencia; un `INDX` está donde dicen los runs de su directorio; una entrada de índice cita un registro encontrado con la misma secuencia; el registro en la posición `n` dice ser el `n` |
| FAT | `..` de un directorio apunta a un directorio cuyo `.` es ese clúster; la cadena de un directorio en la FAT es coherente con su tamaño |
| exFAT | `FirstCluster` de un conjunto apunta a un clúster asignado en el mapa de bits; los conjuntos de un directorio están en su cadena |
| ext/XFS/APFS/Btrfs | Bloque con suma válida cuyo número de inodo, `blkno` u oid coincide con la posición según la geometría |

Columnas propuestas (nombres propios, ADR-001):

| Columna | Definición |
|---|---|
| `Enlaces` | Enlaces correctos / comprobados (p. ej. `48 210 / 48 902`) |
| `Coherencia` | Porcentaje de lo anterior, con un decimal |
| `Ocupado mín.` | Suma de los tramos de corridas atribuidas como «mejores» (cota inferior del área usada; equivalente propio de E9) |
| `Cobertura` | Fracción del rango del candidato ya escaneada (un candidato con cobertura parcial puede mejorar) |
| Indicadores | `P` principal, `C` copia, `M` estructura de metadatos principal (MFT/FAT1/tabla de inodos/árbol raíz), `m` su copia (`$MFTMirr`/FAT2/superbloque de respaldo), `R` raíz, `G` geometría deducida de la metadata sin arranque, con `+ x ? o -` como la búsqueda exprés |

**Confianza** (reglas, como ADR-023):

- `Valid`: principal y copia coherentes y `Coherencia ≥ 95 %` con al menos 64 enlaces; o geometría
  confirmada por al menos dos referencias independientes de distinta clase (p. ej. arranque y registro 0,
  o registro 0 y `$MFTMirr`) con `Coherencia ≥ 95 %`.
- `Suspicious`: el resto de candidatos presentados, con sus motivos: geometría deducida sin arranque
  (`G+`), una sola referencia, coherencia baja, cobertura incompleta, solape con un candidato válido,
  sectores no fiables en la metadata principal, versión de otro candidato.
- No presentado: un solo voto de baja calidad o `Coherencia < 50 %` (queda en la base y en el registro
  de diagnóstico).
- La puntuación ordena dentro de cada confianza: `enlaces correctos` (logarítmico), `Ocupado mín.`,
  indicadores (pesos de ADR-023) y, a igualdad, posición.

**Grupos de fragmentos** (SC-11, E16, nombres propios): respecto del candidato abierto, cada corrida es
`propia` (atribuida a él), `probable` (coherente pero de menor calidad), `de otra versión` (atribuida a
una versión del mismo volumen), `ajena` (otra identidad), `sin volumen` (ningún candidato coherente),
`pequeña` (menos de `min_registros`, sin estadística) o `incompatible` (otro `rs`, `c` o tamaño de
índice). «Más/menos resultados» (SC-08) recorre estos grupos en ese orden: incluir un grupo nunca quita
resultados de los anteriores (monotonía comprobable: más ⊇ menos).

### 8.9 Reconstrucción de un volumen desde lo encontrado

**Abrir un candidato** (SC-07, relación con `ntfs.md`). `VolumeReconstructor` crea un volumen virtual:

- NTFS con arranque válido: se abre con el lector existente, con el inicio y el tamaño del candidato.
- NTFS sin arranque utilizable (la limitación de ADR-025): se sintetiza en memoria un **descriptor de
  geometría** (inicio, `S`, `c`, `rs`, tamaño de índice, LCN de la MFT), nunca un sector de arranque
  escrito; el lector NTFS se amplía con una entrada `NtfsVolume::open_with_geometry` que acepta ese
  descriptor y marca el volumen `Suspicious` con la anomalía `geometry_from_scan`.
- MFT sin registro 0 utilizable en las dos copias: el lector recibe un **mapa de MFT** construido con las
  corridas atribuidas (números y posiciones), en lugar de los runs del registro 0. Los números sin corrida
  quedan `unmapped` (estado ya existente). Los registros encontrados fuera de su posición esperada
  (desplazados, I7) solo se incluyen con la opción `incluir_desplazados` y llevan anomalía.
- Cada registro sigue el camino normal (fixups, atributos, estado): el escaneo nunca relaja ADR-025.

**Árbol virtual** (`mr-vfs`, INV-01 §7.2):

1. Árbol de índices vivos (#22) si la raíz es utilizable.
2. Registros en uso y no en uso atribuidos, colocados por la cadena de padres de `$FILE_NAME` (como #23).
3. Directorios **encontrados** (registros de directorio sin padre resoluble, o `INDX` sin registro dueño)
   en la carpeta sintética `[Encontrados]`, con el número de registro o la posición del bloque en el
   nombre; sus hijos cuelgan de ellos (E20).
4. Huérfanos (padre fuera de rango, ilegible, reutilizado o no directorio) en `[Huerfanos]`, agrupados por
   número de padre citado (`padre_N`), para que los hermanos de un directorio perdido aparezcan juntos.
5. Ficheros sin nombre (registro sin `$FILE_NAME` pero con `$DATA`) en `[Sin nombre]`.
6. Entradas de `INDX` residuales que citan registros no encontrados: entrada con `fiab_datos =
   no_disponible` (nunca se inventa contenido).

Categoría `encontrado` (reservada en INV-01) para todo lo que viene de una corrida del escaneo y no de la
MFT del volumen abierto. Opciones de SC-09 se mapean a este diseño: filtro por número de registro (rango
para reconstrucción parcial), procesar `INDX` (automático: solo directorios sin registro o con registro
dañado; completo: todos), incluir desplazados, incluir extras (corridas `probable` y `sin volumen`),
incluir registros técnicos (0 a 15, `[Metadatos]`). `$LogFile` (FS-06) queda fuera de M6 salvo decisión.

**Escala del árbol.** Para volúmenes con más de `max_nodos_memoria` registros (defecto 4 millones), la
arena de `mr-vfs` se respalda en tablas de la base (`vfs_nodes`) con paginación; el filtro por número de
registro sigue disponible como en E17.

### 8.10 Memoria acotada

| Componente | Cota |
|---|---|
| Búfer de lectura | `L` (defecto 4 MiB; ≤ `MAX_READ_REQUEST`) |
| Mapa de bits de cobertura | `⌈tamaño / T⌉ / 8` bytes (10 TB con `T` = 1 MiB: 1,25 MB); para orígenes mayores, `T` crece automáticamente para no superar 16 MB |
| Corridas abiertas | Una por (FS, tipo, clave) activa, como mucho `max_abiertas` (defecto 4096); al superarla se cierra la más antigua |
| Lote pendiente de confirmar | Hallazgos de un bloque de lectura (≤ `L / 512` hallazgos) |
| Cola de expansión | ≤ 4096 tramos |
| Agrupación | Consultas a la base por grupos; en memoria solo el grupo en curso |
| Verificación de enlaces | Muestra acotada (≤ 65 536 por candidato) y caché de registros (p. ej. 64 MiB) |

Nada en esta lista depende del tamaño del origen salvo el mapa de bits, acotado por diseño. Prueba
obligatoria: memoria máxima igual (con margen) en un origen disperso de 1 GiB y de 16 TiB.

### 8.11 Reanudación, progreso y cancelación

- **Pausa** = parar en el siguiente límite de lote, confirmar y pasar el trabajo a `paused`.
  **Cancelación** = lo mismo con estado `cancelled`; los resultados parciales siguen siendo válidos y
  presentables. El token de cancelación se comprueba antes de cada lectura.
- **Cierre abrupto**: al abrir la base, `running` pasa a `interrupted` (ADR-016); continuar lee el último
  punto de control, reconstruye el mapa de bits desde `scan_coverage` y sigue la secuencia del
  planificador desde ese índice. Lo leído tras el último punto de control se vuelve a leer y, como la
  confirmación es atómica (§8.6), no se duplica ningún hallazgo.
- **Continuar con otros parámetros** (E11): si cambian área o familias, se reutilizan los tramos ya
  cubiertos para las familias que ya se buscaron y solo se reescanea lo nuevo; si cambia el tamaño de
  tramo o el modo, la cobertura se convierte a rangos de bytes y se conserva.
- **Progreso**: tramos cubiertos / tramos del área (también en bytes), velocidad, candidatos por
  confianza, mejor candidato actual; frecuencia limitada (cada 250 ms o cada lote). En modo a saltos el
  porcentaje es exacto (cada tramo se cuenta una vez).
- **Candidatos durante el escaneo** (E2): el agrupador se ejecuta cada `k` lotes (defecto: cada 1 GiB
  cubierto o cada 30 s) y la lista se actualiza; abrir un candidato antes de terminar está permitido y se
  avisa de su `Cobertura`.

### 8.12 Relación con la búsqueda exprés y con el lector NTFS

- **Búsqueda exprés** (ADR-023): acotada, posiciones típicas, sin base. El escaneo completo **reutiliza
  sus reconocedores** de arranque (los mismos `Recognizer` como clasificadores de singulares) y sus
  indicadores; sus volúmenes válidos se cargan como singulares de partida (no se vuelven a buscar) y un
  candidato del escaneo que coincide con un volumen de la exprés se fusiona con él. Un candidato abierto
  desde el escaneo se añade a la lista de particiones encontradas (como OBS M7 de INV-05b).
- **Lector NTFS** (ntfs.md): el escaneo aporta lo que el lector no hace: buscar la MFT cuando el registro
  0 falla en las dos copias y abrir sin arranque. Se reutilizan `apply_fixups`, el parser de registros y
  `decode_runs` sin cambiar sus reglas; la extensión es `open_with_geometry` y el mapa de MFT externo
  (§8.9).
- **RAW** (M7, RW-01, RW-05): fuera de INV-03; el planificador y la cobertura están pensados para que el
  escáner RAW sea otra familia de clasificadores en la misma pasada.

### 8.13 Diferencias deliberadas respecto a lo observado

- Indicador «?» y estado `cobertura parcial` separados de «x»: lo no verificable no es ausencia.
- Métricas con recuentos explícitos (`Enlaces` correctos / comprobados) y confianza por reglas
  documentadas, en lugar de un solo porcentaje.
- Registro de escaneo en SQLite propio (ADR-008); sin compatibilidad con el de la referencia.
- Geometría sin arranque solo como candidato `Suspicious` con anomalía, nunca escrita.
- Orden a saltos determinista y documentado (inversión de bits) con expansión local.

## 9. Correspondencia con la matriz de paridad

| Fila | Cubierto por | Notas |
|---|---|---|
| SC-01 Full Scan multi-FS | §8.2 a §8.4 | NTFS completo en M6; arranques y superbloques de FAT/exFAT desde la exprés; el resto con su lector (M8) |
| SC-02 A saltos | §8.3 | Inversión de bits + expansión local; prueba de tiempo al primer resultado |
| SC-03 Pausa/reanudación y logs | §8.6, §8.11 | La base de trabajo es el registro; guardar como, cargar, autoguardado por lote |
| SC-04 Candidatos y versiones con calidad | §8.7, §8.8 | Métrica propia (`Enlaces`, `Coherencia`, `Ocupado mín.`, `Cobertura`, indicadores); la de la referencia es DESC |
| SC-05 Escáner NTFS (FILE/INDX, agrupación) | §8.4, §8.5, §8.7 | Corridas por clave `p − n × rs`; deducción de `c` por varios anclajes |
| SC-06 Escáneres de otros FS | §5, §8.4 | Ecuaciones y clasificadores definidos; implementación con INV-04 |
| SC-07 Reconstrucción virtual | §8.9 | `open_with_geometry`, mapa de MFT externo, `mr-vfs` con `[Encontrados]` y `[Huerfanos]` |
| SC-08 Más/menos | §8.8 (grupos) | Monotonía comprobable |
| SC-09 Opciones NTFS avanzadas | §8.9 | `$LogFile` pendiente de decisión (§11) |
| SC-11 Grupos de fragmentos | §8.8 | Siete grupos con nombres propios |
| SC-10, SC-12, SC-13 | Fuera de INV-03 | Dependen de los lectores FAT y de `mr-vfs` |

## 10. Plan de pruebas

Todas las pruebas usan un envoltorio de `Source` que **cuenta lecturas y bytes** y `FaultyDevice` para
sectores ilegibles; las imágenes grandes son dispersas (VHDX/VMDK dinámicos o ficheros dispersos con
`Sparse` en el mapa) para no ocupar disco.

**Unitarias y fuzzing** (`mr-scan` y `mr-fs-ntfs::scan`): cada clasificador con estructuras válidas,
cada campo fuera de rango, fixups/sumas incorrectos y relleno `BAD!`/`SKIP` que imita firmas (nunca
emite); agregador con huecos, dañados y claves alternas; ecuaciones de geometría con modelos sintéticos
(propiedad: para cualquier `(inicio, c, rs, runs)` generado, la deducción devuelve exactamente esa
geometría o «indeterminado», nunca otra); orden a saltos (permutación exacta de `0..n` para `n`
arbitrario, determinismo); fuzzing de 100 000 tramos aleatorios (sin pánicos, salida acotada).

**Corpus NTFS** (ADR-024, ampliación del generador con escenarios nuevos):

| Escenario | Contenido | Resultado esperado |
|---|---|---|
| `ntfs_basic` sin daños | Volumen en el byte 0 | 1 candidato `Valid` igual a `ntfs-info`; `Coherencia` 100 %; árbol = manifiesto |
| `ntfs_damaged_boot` (existente) | Principal sobrescrito | `Valid` por copia + MFT; `P x C +` |
| `scan_ntfs_both_boots` | Principal y copia sobrescritos | `Suspicious` con `G +`; `c` y `rs` deducidos = los de `mkntfs`; árbol = `ntfs_basic` |
| `scan_ntfs_record0_lost` | Registro 0 de `$MFT` y de `$MFTMirr` sobrescritos (la limitación de ADR-025) | Abierto por mapa de MFT de corridas; los registros dañados exactos; resto del árbol = manifiesto |
| `scan_ntfs_fragmented_mft` | MFT en varios fragmentos (volumen lleno antes de crear ficheros) y ambos arranques dañados | `c` deducido de dos fragmentos; geometría exacta |
| `ntfs_damaged_mft` (existente) | 4 registros dañados | Los 4 en `damaged`; ningún hallazgo de esos registros |
| `scan_ntfs_reformatted` | Volumen formateado otra vez con otro tamaño de clúster sobre el anterior | Dos candidatos (identidades distintas) o dos versiones; los registros antiguos en `ajena`/`de otra versión` |
| `scan_ntfs_resized` | Volumen ampliado (`ntfsresize`) dejando la copia antigua del arranque | Dos versiones con tamaños distintos; la actual mejor puntuada |
| `scan_ntfs_orphans` | Directorio con hijos cuyo registro se sobrescribe | Hijos en `[Huerfanos]/padre_N`; sus `INDX` en `[Encontrados]` si sobreviven |
| `scan_ntfs_unreadable` | Sectores ilegibles en la MFT y en el arranque | Estados `unreadable`; `?` en indicadores; cobertura con ilegibles; nunca relleno como firma |

**Corpus de particiones** (#14): `lost_*`, `ntfs_mbr`, `ntfs_gpt` y `image_shifted_*` escaneados por
completo: mismos volúmenes que la búsqueda exprés (más los que esta no encuentra por estar fuera de
rejilla: escenario nuevo `scan_offgrid_volume` con un NTFS en una posición arbitraria no alineada a MiB).

**Escala y robustez**:

| Escenario | Resultado esperado |
|---|---|
| Disco disperso de 16 TiB con un NTFS de 32 MiB a 9 TiB | Lee solo los datos reales; encuentra el volumen; memoria máxima igual que con 1 GiB |
| Igual, modo a saltos con dos volúmenes (pequeño y grande) | El grande aparece antes (número de tramos hasta el primer candidato `Valid` medido) |
| Interrupción en cada lote (proceso terminado) y reanudación | Resultado final idéntico (mismas filas, mismos candidatos) al de una ejecución sin cortes; ningún tramo leído dos veces salvo el lote en curso |
| Cancelación y pausa | Estado correcto; resultados parciales presentables; ninguna lectura tras la cancelación |
| Guardar como / cargar con otro origen | Huella distinta: solo lectura de resultados con aviso |
| Determinismo | Dos ejecuciones (y lineal frente a saltos) con los mismos candidatos y puntuaciones |
| Negativos | Disco a cero, aleatorio, todo ilegible: 0 candidatos |

Oráculo: TSK (`fsstat`, `fls`) sobre la imagen sin dañar del mismo generador, como en ADR-024.

## 11. Preguntas abiertas para el propietario

1. **Alcance de M6**: ¿solo NTFS completo más arranques de FAT/exFAT (propuesta), o también los
   escáneres FAT/exFAT completos antes de M8?
2. **Modo por defecto**: ¿a saltos (resultados antes) o lineal (mejor en discos mecánicos)? Se propone a
   saltos en imágenes y SSD y lineal en dispositivos rotacionales, con elección visible.
3. **Tamaño de tramo** (1 MiB) y de lectura (4 MiB): ¿se aceptan como defectos hasta medir en hardware
   real (INV-06)?
4. **Umbrales de confianza** (`Coherencia ≥ 95 %` con ≥ 64 enlaces para `Valid`; < 50 % no se presenta):
   ¿se aceptan o se prefiere calibrarlos antes con el corpus?
5. **Guardar/cargar**: ¿basta con que la base de trabajo sea el registro (copia en caliente para
   «guardar como»), o se quiere un formato de intercambio aparte (JSON) además?
6. **Árbol en la base**: ¿se acepta mover la arena de `mr-vfs` a SQLite por encima de 4 millones de
   nodos, o se prefiere solo el filtro por número de registro (como la referencia)?
7. **Tamaño del volumen sin arranque** desde `$BadClus:$Bad`: es práctica pública conocida pero no la
   documentan las fuentes leídas en esta sesión; ¿se acepta como evidencia secundaria o solo `$Bitmap`?
8. **`$LogFile` (FS-06)** en la reconstrucción: ¿dentro de M6 o en un hito posterior?
9. **Nombres** de las columnas (`Enlaces`, `Coherencia`, `Ocupado mín.`, `Cobertura`), de los indicadores
   (`P C M m R G`) y de las carpetas sintéticas nuevas (`[Encontrados]`, `padre_N`): ¿se aceptan? (enlaza
   con INV-01, pregunta 2).
10. **Releer** al implementar cada clasificador: la Apple File System Reference (el PDF no se pudo
    convertir a texto en esta sesión: la igualdad entre oid y dirección de bloque de los objetos físicos
    se apoya en conocimiento público general y debe confirmarse), el documento de XFS y la cabecera
    `INDEX_BLOCK` de NTFS.
11. **ReFS**: con E19 como única evidencia y sin especificación pública suficiente, ¿se excluye ReFS del
    escaneo hasta INV-10?
12. **Decisión de arquitectura**: ¿se registra este diseño como ADR (base de escaneo en `mr-store` v2,
    corridas, geometría sin arranque solo `Suspicious`, orden a saltos por inversión de bits) antes de
    abrir las issues de implementación de M6?
