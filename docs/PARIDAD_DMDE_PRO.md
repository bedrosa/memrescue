# Matriz de paridad con DMDE Professional

Referencia: DMDE 4.4.4.842 Professional. Fecha de creación: 2026-10-17.

## Leyenda

**Verificación de la referencia** (qué sabemos de DMDE):
- **M**: documentado en el manual o la web oficial (`investigacion/dmde-manual-oficial.md`).
- **B**: observado en el binario (cadenas, mensajes de UI, IOCTL, constantes).
- **D**: lógica verificada en código decompilado (`investigacion/dmde-fase1/05`).
- **Pro**: exclusiva de la edición Professional según la web oficial.
- **?**: necesita investigación (ver `ROADMAP.md`, tareas INV).

**Estado MemRescue**: `No iniciado` → `Diseñado` → `Implementado` → `Probado` → **`Paridad`**.
`Paridad` exige prueba reproducible (ADR-002). Tras M0 solo hay infraestructura: ninguna función de
recuperación está implementada (ver resumen al final).

Referencias abreviadas a documentos de investigación: `F1-05` = `investigacion/dmde-fase1/05-algoritmos-verificados.md`, etc.; `MAN` = `investigacion/dmde-manual-oficial.md`.

---

## 1. Acceso a dispositivos y E/S

| ID | Funcionalidad | Descripción | Ref. DMDE | Evidencias | Verif. | Diseño MemRescue | Estado | Pruebas necesarias | Diferencias pendientes |
|---|---|---|---|---|---|---|---|---|---|
| IO-01 | Enumerar discos físicos | Lista de discos con modelo, tamaño, bus | Selector de dispositivos | F1-06 (SetupAPI, IOCTL_STORAGE_QUERY_PROPERTY), MAN | M B D | `mr-io-win` enumeración | No iniciado | Mock de enumeración; prueba manual en VM | Columnas exactas de DMDE no documentadas (INV-12) |
| IO-02 | Enumerar volúmenes lógicos | Volúmenes y letras | Selector, "Show Volume Letters" | F1-06 | M B | `mr-io-win` | No iniciado | VM con VHD montado | - |
| IO-03 | Abrir disco en solo lectura | `\\.\PhysicalDriveN` | Base | F1-06 | B D | `BlockDevice` Win | No iniciado | Prueba en VM con disco virtual | - |
| IO-04 | Geometría y tamaño | Sector lógico/físico, tamaño total | Base | F1-06 (GET_LENGTH_INFO, GEOMETRY) | D | `mr-io-win` | No iniciado | VHD de 512 y 4Kn | Detección de capacidad "no accesible" (HPA/DCO) ? |
| IO-05 | Interfaz IO FILE / SCSI / ATA | Selección de vía de lectura | Parámetros E/S | MAN, F1-06 (SCSI/ATA pass-through) | M D | Estrategias intercambiables | No iniciado | Simulador + VM | CDBs/comandos exactos no verificados (INV-06) |
| IO-06 | Reintentos CRC y seek | Reintentos configurables, sector a sector tras error | Parámetros E/S | MAN, F1-06 | M B | `mr-io::resilient` | **Implementado parcial** (M1: relectura sector a sector tras fallo de bloque + N reintentos por sector; los sectores leídos tras reintento quedan como `Recovered` en el mapa de validez) | Hecho: `FaultyDevice` (sectores permanentes e intermitentes), pruebas de `resilient` con comprobación sector a sector. Falta: hardware real (M9) | Sin distinción CRC/seek |
| IO-07 | Salto sobre sectores malos + lectura hacia atrás | Reduce carga en zonas dañadas | Parámetros E/S | MAN, F1-06 | M B | `ResilientReader` | **Implementado parcial** (M1: tras un ilegible se saltan N sectores, marcados `Skipped` sin leerlos; lectura hacia atrás opcional hasta el siguiente error; memoria acotada; CLI `--jump`/`--read-back`, GUI y ABI 3) | Hecho: simulador con recuento de lecturas por sector (los saltados y dispersos no se leen), zonas dañadas contiguas y dispersas, salto hasta el final, disperso dentro de un salto, cancelación. Falta: hardware real (M9) | Algoritmo exacto de retroceso de la referencia ? (INV-06); sin salto adaptativo |
| IO-08 | Rellenos de sectores malos/saltados | Patrón de 4 bytes LE | Parámetros E/S | MAN | M B | `mr-io::resilient` | **Implementado** (M1: rellenos distintos para ilegibles y saltados, por defecto `BAD!`/`SKIP`, configurables por lectura en el motor (`mr_source_read_mapped_ex`, ADR-021), en la CLI (`sectors --bad-fill/--skip-fill`) y en la interfaz (Herramientas → Patrones de relleno, preferencias persistentes por usuario); regla de admisión común: ni patrones con los 4 bytes iguales, `00 00 00 00` incluido, ni el mismo para ambos; mapa de validez con 5 estados por sector; el relleno nunca se presenta como dato ni entra en un hash) | Hecho: comprobación sector a sector contenido↔estado con patrones propios, hash estricto que rechaza el relleno con cualquier patrón, validación en motor/FFI/C#/CLI, preferencias dañadas → valores por defecto, UI Automation (validación, guardado, persistencia entre ejecuciones), visor y CLI que marcan «NO FIABLE, patrón de relleno» | El patrón se define como secuencia de 4 bytes en el orden escrito (no como entero LE); se aplicará también al clonado/imagen cuando exista (M9) |
| IO-09 | Omitir errores por código / "ignorar todo" | Lista de códigos | Parámetros E/S | MAN | M B | Política de errores | **Implementado parcial** (M1: `ReadPolicy` con lista de códigos del sistema sin reintentos y modo «ignorar todo»; los sectores afectados quedan `Unreadable` con relleno, nunca datos; con `Fail` se sigue abortando; se combina con salto y lectura hacia atrás; informe con errores omitidos y recuento por código; CLI `--skip-codes`/`--ignore-errors`, GUI y `mr_source_verify_start_ex` sin cambiar la ABI 3) | Hecho: simulador con códigos distintos por sector y recuento de lecturas por sector (sin reintentos en los omitidos), estados, salto, bordes del rango, FFI, C# y CLI. Falta: hardware real (M9) | Sin decisión interactiva por error (se configura antes de empezar) |
| IO-10 | Timeouts, reset de handle, reanudar al reconectar | Robustez ante desconexiones | Parámetros E/S | MAN, F1-06 | M B | `mr-io-win` + política | No iniciado | Simulador de desconexión | - |
| IO-11 | Script de manejo de E/S **(Pro)** | Lenguaje para decidir ante errores | Parámetros E/S | MAN, F1-09 | M B Pro | Lenguaje propio o Rhai/Lua embebido | No iniciado | Pruebas del intérprete | Semántica completa ? (INV-07) |
| IO-12 | Informe SMART (ATA) y salud NVMe | Atributos y umbrales | Parámetros E/S | MAN, F1-06 (SMART IOCTL) | M B D | `mr-io-win` | No iniciado | Datos SMART grabados (fixtures) | - |
| IO-13 | Bloquear/desmontar volúmenes | Antes de escribir/copiar | Disk → Lock Volumes | F1-06 (FSCTL_LOCK/DISMOUNT) | M D | `mr-io-win` | No iniciado | VM | - |
| IO-14 | Acceso DASD extendido | Leer todo el volumen | Base | F1-06 (FSCTL_ALLOW_EXTENDED_DASD_IO) | D | `mr-io-win` | No iniciado | VM | - |

## 2. Imágenes y dispositivos virtuales

| ID | Funcionalidad | Descripción | Ref. DMDE | Evidencias | Verif. | Diseño MemRescue | Estado | Pruebas necesarias | Diferencias pendientes |
|---|---|---|---|---|---|---|---|---|---|
| IM-01 | Imagen sector a sector | IMG/BIN/RAW | Open Image | MAN, F1-02 | M B | `mr-io::ImageFile` | **Probado** (M1: apertura solo lectura con política de rutas, sectores de 512/1024/2048/4096, lectura de rangos estricta, verificación completa, visor) | Hecho: unitarias, CLI, C#, corpus (marcadores en LBA exactas, 512 y 4096 B), autodiagnóstico GUI, pruebas de UI Automation (`--open`), inmutabilidad del origen | Falta abrir junto con logs/escaneos y la búsqueda de particiones al abrir (M2) para cubrir todo el diálogo de referencia |
| IM-02 | Imagen dividida | Partes + descriptor | Copy Sectors (split) | MAN | M B | `mr-image::split` | **Implementado parcial** (M1: lectura de `x.001…` / `x.000…` del mismo directorio; el descubrimiento solo lista y la apertura exige confirmación; CLI `split-list`/`--split`, GUI, ABI 3) | Hecho: SHA-256 de la lectura = RAW en 5 reparticiones, lecturas que cruzan fronteras, huecos, nombres raros, directorios, límite de 10 000 segmentos, inmutabilidad; FFI, C# y CLI | Crear partes (CL-07); convenciones `.aa`/`.part1`; descriptor propio |
| IM-03 | VHD (fijo/dinámico/diferencial) | Lectura | Imágenes | MAN, F1-06 | M B | `mr-image` | **Implementado parcial** (M1: VHD **fijo y dinámico probados**; diferencial → referencia externa, nunca se abre el padre; BAT dañada: estricto por defecto y **modo tolerante** explícito, ADR-020) | Hecho: qemu-img 11.1.2 como referencia independiente (hash completo, 300 rangos aleatorios, mapa de dispersos = `qemu-img map`), corpus GPT/MBR convertido, unitarias con corrupciones, inmutabilidad; modo tolerante: entradas fuera del fichero, solapadas, duplicadas, sobre estructuras (sector a sector contenido ↔ estado, también contra qemu-img), FFI, C#, CLI e interfaz | Diferencial (cadenas de padres) pendiente |
| IM-04 | VHDX | Lectura | Imágenes | MAN, F1-06 | M B | `mr-image` | **Implementado parcial** (M1: VHDX **fijo y dinámico probados**, sector lógico 512/4096; registro pendiente → no soportado; diferencial → referencia externa; BAT dañada: estricto por defecto y **modo tolerante** explícito, ADR-020) | Hecho: qemu-img 11.1.2 como referencia independiente (14 variantes de bloque y tamaño), corpus convertido, unitarias con CRC-32C y corrupciones, inmutabilidad; modo tolerante con imágenes sintéticas (fuera del fichero, solapadas, duplicadas, estados 4/5/PARTIALLY_PRESENT; sector a sector contenido ↔ estado). Sector lógico 4096 solo con imágenes sintéticas (qemu no lo genera) | Reproducción del registro (log replay) y diferenciales pendientes |
| IM-05 | VMDK | Lectura | Imágenes | MAN | M B | `mr-image` | **Implementado parcial** (M1: VMDK **monolithicSparse probado**; streamOptimized/COWD → no soportado; descriptor separado, extensiones y padres → referencia externa sin seguirla; GD/GT dañados: estricto por defecto y **modo tolerante** explícito, ADR-020) | Hecho: qemu-img 11.1.2 como referencia independiente (4 imágenes, incl. granos a cero), rechazo de streamOptimized/monolithicFlat/hijo con padre, descriptores maliciosos (rutas `..\`, UNC), corpus convertido; modo tolerante: GT y granos fuera del fichero, solapados, duplicados, entrada 1 sin indicador (sintéticas y grano duplicado contra qemu-img) | streamOptimized, twoGbMaxExtent*, monolithicFlat y padres pendientes (requieren abrir otros ficheros con confirmación) |
| IM-06 | E01/EWF **(Pro)** | Lectura con compresión | Imágenes | MAN | M Pro | `mr-image` (spec libewf) | No iniciado (M1: **detección** EVF/LVF/EVF2) | E01 generados con herramienta libre | - |
| IM-07 | Capa de cambios diferidos | Deshacer/rehacer/aplicar/rollback | Disk menu | MAN, F1-04 | M B | `mr-changes` | No iniciado | Pruebas de propiedades | - |
| IM-08 | Modo global de solo lectura | Bloqueo de escritura | Disk menu | MAN | M B | ADR-006 | No iniciado | Prueba de que nada escribe | - |

## 3. Particiones

| ID | Funcionalidad | Descripción | Ref. DMDE | Evidencias | Verif. | Diseño MemRescue | Estado | Pruebas necesarias | Diferencias pendientes |
|---|---|---|---|---|---|---|---|---|---|
| PT-01 | Leer MBR/EBR | Primarias y lógicas | Partitions | MAN, F1-07 | M B D | `mr-part` | **Implementado (#12)**: primarias y lógicas con LBA, tamaño, tipo y activa; confianza y anomalías tipadas (ADR-022); bucles, enlaces fuera del disco o de la extendida, límite de cadena; 0x85 aceptado (mejora, INV-05 §4); MBR protector e híbrido reconocidos e integrados con la GPT (particiones GPT, entradas del híbrido y discrepancias entre ambas); API, ABI, CLI `partitions`/`--partition` y lista en la interfaz con apertura de cada partición. Pendiente de comparación manual con DMDE (ADR-002, opcional) | Corpus `mbr_basic`, `mbr_*` (sin firma, 2 activas, 0x85, EBR en bucle o fuera, solapes, tipo desconocido, fuera del disco), `fake_mbr_in_data`, `hybrid_mbr_gpt` (#14) | - |
| PT-02 | Leer GPT + copia | Cabecera, entradas, CRC | Partitions | F1-05 §5.2 | M D | `mr-part::gpt` | **Implementado** (#13: firma, revisión, tamaño y CRC-32 de cabecera, CRC-32 de entradas, `MyLBA`/`AlternateLBA`, rango utilizable, límites de entradas, solapes y GUID repetidos; primaria preferida y respaldo informado; desacuerdo entre copias; 512 y 4096 B con prueba del otro tamaño; sectores ilegibles nunca interpretados; `formatos/gpt.md`) | Hecho: 21 unitarias (incl. fuzzing determinista) y 4 de corpus contra los 13 manifiestos con estado GPT, con `FaultyDevice` | Integrado (#12, ADR-022 adenda): API, ABI (copia en uso, GUID, nombres), CLI `partitions` e interfaz muestran las particiones GPT y la copia de respaldo en uso; prueba de corpus de la tabla completa por Rust, CLI, C# e interfaz. No se reconstruye una copia a partir de la otra |
| PT-03 | Tolerancia GPT | Rebasar LBA si la imagen está desplazada | - | F1-05 §5.2 | D | `mr-part::gpt` | **Implementado** (#13: desplazamiento deducido de `MyLBA` de la copia encontrada frente a su posición, informado como anomalía y nunca aplicado en silencio; sectores finales ausentes; rango utilizable fuera de la imagen) | Hecho: corpus `gpt_shifted_start`, `gpt_shifted_start_1`, `gpt_truncated_end` (marcadores leídos en la LBA de imagen calculada) y unitarias con desplazamiento +63, +1 y -8 | Imagen desplazada y recortada a la vez sin primaria no se reconoce (queda para #15). Integrado (#12): API, ABI, CLI e interfaz informan del desplazamiento y abren una partición de una imagen desplazada solo si se pide explícitamente (`--apply-shift`, `MR_PARTITION_OPEN_APPLY_SHIFT`, confirmación), con el desplazamiento en la descripción |
| PT-04 | Apple Partition Map | Lectura | - | F1-07 | B | `mr-part` | No iniciado | Imagen APM | Prioridad baja |
| PT-05 | Búsqueda exprés de particiones perdidas | Al abrir dispositivo | Partitions "found" | MAN | M B D | `mr-part::search` | **Implementado (#15, ADR-023)**: algoritmo propio de INV-05b (candidatos P0-P4, presupuesto acotado que no crece con el disco, caché determinista, cancelación antes de cada lectura), NTFS/FAT12/16/32/exFAT con principal, copia coherente y estructuras básicas, tablas MBR/EBR estrictas y cabeceras GPT en posiciones candidatas, hipótesis de desplazamiento con dos evidencias; API, ABI, CLI `find-partitions`/`--found` y acción en la interfaz. **Diferencia deliberada**: solo a petición (decisión del propietario), no al abrir | Hecho: 18 volúmenes de los 38 manifiestos comparados exactamente (Rust con `FaultyDevice`, CLI y C#), señuelos, desplazadas, 4Kn (y leído como 512), negativos, tablas intactas sin duplicados, presupuesto, cancelación, determinismo, escala 1 GiB frente a 2 TiB, fuzzing acotado y UI Automation | El orden y la extensión exactos de DMDE no constan en su documentación pública; pendiente la comparación manual (ADR-002, opcional) |
| PT-06 | Indicadores T/E/B/C/F/f/x y activa | Estado de estructuras | Partitions | MAN | M | UI + `mr-part` | **Implementado (#15)**: indicadores propios equivalentes en propósito (E entrada de tabla, P principal, C copia, S estructuras básicas, U autoubicación, A alineación; estado tabla/encontrada/parcial) con un estado más, «?» no verificable, distinto de «x» (`formatos/busqueda-particiones.md`) | Hecho: indicadores comprobados en Rust, CLI, C# y UI Automation (`FoundVolumesList`) | Diferencia deliberada: «?» separado de «x»; sin indicador separado para `$MFTMirr` (f) |
| PT-07 | Insertar (undelete), quitar, crear RAW | Edición de tabla | Partitions (avanzado) | MAN | M B | `mr-part` + `mr-changes` | No iniciado | Sobre imágenes únicamente | Escritura física bloqueada (ADR-006) |
| PT-08 | Restaurar boot sector desde copia | NTFS/FAT/exFAT | Partitions | MAN | M B | `mr-part` | No iniciado | Boot dañado + copia sana | - |
| PT-09 | MBR/GPT On/Off, activa | Firmas y flag | Partitions | MAN | M B | `mr-part` | Parcial, solo lectura (#12): se informa la firma ausente (`mbr_signature_missing`) y el indicador activo/no válido de cada entrada; cambiarlos requiere la capa de cambios (M12) | Imágenes | - |
| PT-10 | Guardar/cargar particionado, deshacer | Copia de seguridad de tablas | Partitions | MAN | M B | `mr-part` | No iniciado | Ida y vuelta | Formato propio |
| PT-11 | Escribir boot sectors / código MBR | Disco arrancable DOS | Partitions | MAN | M B | - | No iniciado | - | **Fuera de alcance inicial** (requiere código de arranque propio) |

## 4. Sistemas de archivos (lectura)

| ID | Funcionalidad | Descripción | Ref. DMDE | Evidencias | Verif. | Diseño MemRescue | Estado | Pruebas necesarias | Diferencias pendientes |
|---|---|---|---|---|---|---|---|---|---|
| FS-01 | NTFS: boot, MFT, fixups, atributos, runs | Lectura completa | Base | F1-05 §5.1, §5.3 | M D | `mr-fs-ntfs` | **Hecho (#20, #21; ADR-025, ADR-026)**: arranque con validación estricta y copia del último sector, `$MFT` por los runs del registro 0 (o su copia) y sus extensiones, registros FILE con fixups comprobados por tramo, cabeceras de atributo acotadas, atributos tipados (`$STANDARD_INFORMATION`, `$FILE_NAME`, `$OBJECT_ID`, `$VOLUME_*`), `$ATTRIBUTE_LIST` con extensiones comprobadas (base, secuencia, ciclos), runs de cualquier atributo con aritmética comprobada y extensiones contiguas, estado por registro, `$MFTMirr` comparado; API `ntfs_open`/`ntfs_file` y CLI `ntfs-info`/`ntfs-stat` (`formatos/ntfs.md`) | Hecho: corpus NTFS de ntfs-3g con TSK como oráculo (10 manifiestos: campos del volumen, arranque desde la copia, exactamente los 4 registros dañados, `$MFTMirr` dañado, estado y secuencia de 523 entradas, runs de cada fichero, MBR/GPT por `mr-part`), unitarias y fuzzing; `$ATTRIBUTE_LIST` solo con volúmenes sintéticos. Pendiente: imágenes NTFS de Windows | Diferencia deliberada: un fixup incorrecto fuera del área usada también deja el registro dañado (ADR-025); una extensión de otro base o reutilizada no se usa (ADR-026) |
| FS-02 | NTFS: índices y directorios | INDX/$I30 | Base | F1-05 §5.3 | D | `mr-fs-ntfs` | No iniciado | Directorios grandes | - |
| FS-03 | NTFS: comprimidos (LZNT1), sparse, residentes | Datos | Base | F1-05 §5.3 | M D | `mr-fs-ntfs` | **Hecho (#21, ADR-026)**: residentes, no residentes, dispersos, más allá del tamaño inicializado (ceros, nunca leídos), unidades de compresión LZNT1 ([MS-XCA]) comprimidas, sin comprimir y dispersas; lectura por el mapa de validez con estado por tramo (patrón `BAD!` en lo no fiable); API `ntfs_read_stream`/`ntfs_copy_stream`, CLI `ntfs-cat` (código 3 si hay tramos no fiables) | Hecho: SHA-256 de 434 ficheros del corpus (vivos, borrados y sobrescritos, con dispersos e `init_size`), LZNT1 contra 26 vectores de `RtlCompressBuffer`/`RtlDecompressBuffer`, ficheros comprimidos sintéticos, `FaultyDevice`. Pendiente: ficheros comprimidos de un volumen de Windows (`compact`) | El corpus de ntfs-3g no tiene comprimidos: la compresión se valida con el oráculo de Windows y volúmenes sintéticos |
| FS-04 | NTFS: ADS **(Pro)** | Flujos alternativos | Recuperación | MAN, F1-04 | M B Pro | `mr-fs-ntfs` | **Lectura hecha (#21)**: enumeración de los `$DATA` con nombre (`NtfsFile::data_streams`), lectura residente y no residente, `ntfs-cat --stream`, `ntfs-stat` | Hecho: los 5 flujos alternativos del corpus (residentes y no residentes, en ficheros y en un directorio) con el SHA-256 del manifiesto | Recuperación de ADS junto al fichero: M4 |
| FS-05 | NTFS: EFS (raw, .efs/.efk/.efb) | Sin descifrar | Recuperación | MAN, F1-02 | M B | `mr-fs-ntfs` + `mr-recover` | No iniciado | Ficheros EFS en VM | Formato `.efb` es propio de DMDE: usar API de Windows `ReadEncryptedFileRaw` |
| FS-06 | NTFS: $LogFile (journal, beta) | Información adicional | Reconstrucción | MAN, F1-05 | M D | `mr-fs-ntfs` | No iniciado | Imágenes con journal | DMDE no hace replay completo |
| FS-07 | FAT12/16/32 | Boot, FAT1/FAT2, directorios, LFN | Base | MAN, F1-07 | M B | `mr-fs-fat` | No iniciado | mkfs.fat / format | Lógica DMDE no verificada (INV-04) |
| FS-08 | exFAT | Lectura | Base | MAN, F1-07 | M B | `mr-fs-exfat` | No iniciado | Imágenes exFAT | INV-04 |
| FS-09 | Ext2/3/4 | Inodos, extents, journal, 64-bit, codificaciones | Base | MAN, F1-05 §5.5 | M D (superbloque) | `mr-fs-ext` | No iniciado | mkfs.ext2/3/4 | Resto INV-04 |
| FS-10 | HFS+/HFSX | Catálogo, extents, resource fork | Base | MAN, F1-07 | M B | `mr-fs-hfsplus` | No iniciado | Imágenes HFS+ | INV-04 |
| FS-11 | APFS | Contenedor, omap, volúmenes | Base | MAN, F1-05 §5.6 | M D (superbloque) | `mr-fs-apfs` | No iniciado | Imágenes APFS (generación a investigar) | Cifrado APFS fuera de alcance ? |
| FS-12 | ReFS (hasta v3.14) | Lectura | Base | MAN, changelog | M B | `mr-fs-refs` | No iniciado | Imágenes ReFS (Windows Server/Pro for Workstations) | Especificación pública escasa: alto riesgo |
| FS-13 | Btrfs (subvolúmenes) | Lectura | Base | MAN, changelog | M B | `mr-fs-btrfs` | No iniciado | mkfs.btrfs | INV-04 |
| FS-14 | ISO9660 | Lectura | - | F1-07 (`CD001`) | B | `mr-fs-iso` | No iniciado | Imágenes ISO | No anunciado oficialmente |
| FS-15 | Nombres nacionales / páginas de código | Transliteración, OEM | Base | MAN, F1-09 | M B | `mr-common` | No iniciado | Nombres no ASCII | - |

## 5. Escaneo y reconstrucción

| ID | Funcionalidad | Descripción | Ref. DMDE | Evidencias | Verif. | Diseño MemRescue | Estado | Pruebas necesarias | Diferencias pendientes |
|---|---|---|---|---|---|---|---|---|---|
| SC-01 | Full Scan multi-FS | Escaneo del dispositivo/rango | Tools → Full Scan | MAN, F1-04 | M B | `mr-scan` | No iniciado | Imágenes con particiones borradas | - |
| SC-02 | Escaneo "a saltos" (grueso→fino) | Resultados grandes primero | Full Scan | MAN, F1-05 §5.4 | M D (NTFS) | `mr-scan` | No iniciado | Medir tiempo a primer resultado | - |
| SC-03 | Pausa/reanudación y logs | Guardar/cargar/autoguardado | Full Scan | MAN, F1-09 | M B | SQLite (ADR-008) | No iniciado | Interrumpir y reanudar | Formato propio |
| SC-04 | Candidatos/versiones de volumen con calidad | %, Check, Min. Size, indicadores | Full Scan | MAN | M B | `mr-scan` | No iniciado | Imágenes con volúmenes redimensionados | Métrica exacta de DMDE ? (INV-03) |
| SC-05 | Escáner NTFS (FILE/INDX, agrupación) | Fragmentos MFT/INDX | Full Scan | F1-05 §5.4 | D | `mr-fs-ntfs::scan` | No iniciado | MFT parcialmente sobrescrita | Puntuación ? (INV-03) |
| SC-06 | Escáneres FAT/exFAT/Ext/HFS/ReFS/APFS/Btrfs | Fragmentos por FS | Full Scan | MAN, F1-07 | M B | `mr-fs-*::scan` | No iniciado | Imágenes dañadas por FS | INV-04 |
| SC-07 | Reconstrucción FS virtual: modos rápido/defecto/completo/personalizado | Árbol reconstruido | All Found/Virtual FS | MAN | M B | `mr-vfs` | No iniciado | Comparar árboles esperados | - |
| SC-08 | Más/Menos resultados | Ajuste de extras | Reconstrucción | MAN | M B | `mr-vfs` | No iniciado | Monotonía (más ⊇ menos) | - |
| SC-09 | Opciones NTFS avanzadas | Filtro MFT, INDX auto/completo, journal, desplazados, extras, técnicos | Reconstrucción | MAN | M B | `mr-vfs` + `mr-fs-ntfs` | No iniciado | Una prueba por opción | - |
| SC-10 | Opciones FAT (FAT1/FAT2/ninguna, no usar sectores malos) | - | Reconstrucción | MAN | M B | `mr-fs-fat` | No iniciado | FAT1 dañada | - |
| SC-11 | Grupos de fragmentos (mejores/relacionados/ajenos…) | Clasificación | Reconstrucción | MAN | M B | `mr-scan` | No iniciado | Dos volúmenes superpuestos | Criterios ? |
| SC-12 | Mapa de clusters + informe | Qué fichero ocupa cada cluster | Tools | MAN | M B | `mr-vfs` | No iniciado | Verificar asignaciones | - |
| SC-13 | Búsqueda por nombre en carpetas | Búsqueda en árbol | Tools | MAN | M B | `mr-vfs` | No iniciado | Máscaras | - |

## 6. RAW por firmas

| ID | Funcionalidad | Descripción | Ref. DMDE | Evidencias | Verif. | Diseño MemRescue | Estado | Pruebas necesarias | Diferencias pendientes |
|---|---|---|---|---|---|---|---|---|---|
| RW-01 | Búsqueda RAW integrada en Full Scan | Resultados en carpeta RAW | Full Scan | MAN, F1-08 | M B | `mr-raw` | No iniciado | Imagen con ficheros conocidos sin FS | Lógica DMDE no verificada (INV-02) |
| RW-02 | Firmas con comodín, máscara de bits, offset variable ≤ 4096 | Definición | Full Scan | MAN | M B | ADR-009 | No iniciado | Pruebas unitarias del matcher | - |
| RW-03 | Firma EOF, límites de tamaño, contenedores, "fiable" | Fin de fichero | Full Scan | MAN, F1-08 | M B | `mr-raw` | No iniciado | Ficheros truncados/concatenados | - |
| RW-04 | Calculadores de tamaño por formato | JPEG, ZIP, OLE, MP4, PDF… | Implícito | F1-08 (tabla de tipos) | B | `mr-raw::formats` | No iniciado | Corpus por formato | Qué formatos calcula DMDE ? (INV-02) |
| RW-05 | Filtrado RAW con mapa de clusters | Ocultar lo ya hallado por FS | Full Scan | MAN | M B | `mr-raw` + `mr-vfs` | No iniciado | Imagen mixta | - |
| RW-06 | Generar firma desde muestras + probar | Asistente | Full Scan | MAN, F1-04 | M B | `mr-raw` | No iniciado | Muestras sintéticas | - |
| RW-07 | Marcadores de estado RAW (+EOF, sin EOF, tamaño estimado) | Panel | File panel | MAN | M | UI | No iniciado | - | - |
| RW-08 | Recuperación de fragmentados | Más allá del primer fragmento | - | MAN (DMDE solo recupera el 1.er fragmento en RAW) | M | Investigación propia | No iniciado | Ficheros fragmentados | **Mejora posible** sobre DMDE, no requisito de paridad |

## 7. Recuperación y exportación

| ID | Funcionalidad | Descripción | Ref. DMDE | Evidencias | Verif. | Diseño MemRescue | Estado | Pruebas necesarias | Diferencias pendientes |
|---|---|---|---|---|---|---|---|---|---|
| RC-01 | Recuperar ficheros/directorios masivamente | Sin límite de nº | Recover (bulk) | MAN, F1-08 | M B | `mr-recover` | No iniciado | Árbol de 100k ficheros | Lógica DMDE no verificada (INV-01) |
| RC-02 | Categorías (normal/borrado/encontrado/RAW…) | Selección | Recover | MAN, F1-04 | M B | `mr-recover` | No iniciado | Una prueba por categoría | - |
| RC-03 | Filtros (máscaras, exclusión, tamaño, fecha, ID, solo dirs con ficheros) | - | Recover | MAN | M B | `mr-recover` | No iniciado | Tabla de casos | - |
| RC-04 | Destino en otro disco físico | Protección | Recover | MAN | M B | `mr-recover` | No iniciado | Mismo disco → rechazo | - |
| RC-05 | Conflictos de nombre (renombrar, auto, omitir, fusionar, umbral) | - | Recover | MAN | M B | `mr-recover` | No iniciado | Casos | - |
| RC-06 | Errores (omitir, ignorar, ocultar, mover a carpeta de malos) + manejadores | - | Recover | MAN | M B | `mr-recover` | No iniciado | Simulador con errores | - |
| RC-07 | Rutas largas, Unicode, transliteración | - | Recover | MAN | M B | `mr-recover` | No iniciado | Rutas > 260 | Nombres sustitutos DOS fuera de alcance |
| RC-08 | Dividir ficheros grandes | Destinos FAT | Recover | MAN | M B | `mr-recover` | No iniciado | 5 GiB → FAT32 | - |
| RC-09 | Restaurar fechas, permisos, propietario | - | Recover | F1-04 | B | `mr-recover` | No iniciado | Comparar metadatos | - |
| RC-10 | Informe de recuperación con CRC-32/MD5/SHA-1/SHA-256 **(Pro)** | Forense | Recover | MAN, F1-02 (MD5/CRC D) | M B D Pro | `mr-report` (hashes en `mr-common`) | No iniciado (M0: primitivas CRC-32/MD5/SHA-1/SHA-256 implementadas y probadas con vectores conocidos) | Hashes conocidos | Falta el informe en sí (M4) |
| RC-11 | Lista de ficheros TXT/CSV | - | Recover | MAN | M B | `mr-report` | No iniciado | Formato | - |
| RC-12 | Lista HTML **(Pro)** | - | Recover | MAN | M B Pro | `mr-report` (plantilla propia) | No iniciado | Validar HTML | - |
| RC-13 | Lista de sectores / clusters | Para clonado parcial | Recover | MAN | M B | `mr-report` | No iniciado | Rangos correctos | - |
| RC-14 | Vista previa de ficheros | Imágenes/texto | File panel | MAN, F1-02 (GDI+) | M B | UI | No iniciado | - | - |

## 8. RAID virtual

| ID | Funcionalidad | Descripción | Ref. DMDE | Evidencias | Verif. | Diseño MemRescue | Estado | Pruebas necesarias | Diferencias pendientes |
|---|---|---|---|---|---|---|---|---|---|
| RD-01 | RAID 0 | Stripe | Construct RAID | MAN, F1-08 | M B | `mr-raid` | No iniciado | Arrays sintéticos (mdadm o generador propio) | Lógica DMDE no verificada (INV-03b) |
| RD-02 | RAID 1 (solo lectura) | Espejo | Construct RAID | MAN | M B | `mr-raid` | No iniciado | Idem | - |
| RD-03 | RAID 4 | Paridad dedicada | Construct RAID | MAN | M B | `mr-raid` | No iniciado | Reconstruir con un disco NULL | - |
| RD-04 | RAID 5 (4 rotaciones + paridad retrasada) | - | Construct RAID | MAN | M B | `mr-raid` | No iniciado | Cada rotación con mdadm/generador | Definición exacta de "delay" ? |
| RD-05 | RAID 6 (solo lectura, un NULL) | Doble paridad | Construct RAID | MAN | M B | `mr-raid` (P+Q, GF(2^8)) | No iniciado | Idem | - |
| RD-06 | Rotación personalizada (orden, −1 XOR, −2 sin uso) | - | Striping | MAN | M | `mr-raid` | No iniciado | Tablas de rotación | - |
| RD-07 | JBOD/spanned (lectura/escritura) | Concatenación | Construct RAID | MAN | M B | `mr-raid` | No iniciado | - | Escritura bloqueada al principio |
| RD-08 | Offset y tamaño por disco, disco NULL, read-ahead | - | Construct RAID | MAN | M B | `mr-raid` | No iniciado | - | - |
| RD-09 | Cálculo automático de parámetros | Estadístico con restricciones | RAID Auto Calculate | MAN, F1-08, `FUN_00671ef0` (no usar) | M B | `mr-raid::detect` (diseño propio) | No iniciado | Arrays con parámetros ocultos | Métrica ? (INV-03b) |
| RD-10 | Guardar/cargar configuración | - | RAID | MAN | M B | TOML | No iniciado | Ida y vuelta | - |
| RD-11 | VHD diferenciales encadenados | "DVHD" | RAID | F1-08 | B | `mr-image` | No iniciado | Cadena de VHD | ? |

## 9. Clonado e imagen

| ID | Funcionalidad | Descripción | Ref. DMDE | Evidencias | Verif. | Diseño MemRescue | Estado | Pruebas necesarias | Diferencias pendientes |
|---|---|---|---|---|---|---|---|---|---|
| CL-01 | Copiar sectores (dispositivo/partición/fichero/selección) | - | Copy Sectors | MAN, F1-08 | M B | `mr-clone` | No iniciado | Imagen → imagen bit a bit | - |
| CL-02 | Copia inversa | - | Copy Sectors | MAN | M B | `mr-clone` | No iniciado | - | - |
| CL-03 | Doble hilo | Lectura/escritura en paralelo | Copy Sectors | MAN | M B | `mr-clone` | No iniciado | - | - |
| CL-04 | Log + reanudar **(Pro)** | Multipasada | Copy Sectors | MAN | M B Pro | `mr-clone` + mapa | No iniciado | Interrupción y reanudación | - |
| CL-05 | Reintentar sectores malos desde log **(Pro)** | - | Copy Sectors | MAN | M B Pro | `mr-clone` | No iniciado | Simulador | - |
| CL-06 | Mapfile ddrescue (exportar; importar como mejora) | Interoperabilidad | Copy Sectors | MAN, F1-08 | M B | `mr-clone` | No iniciado | Validar con ddrescuelog | - |
| CL-07 | Dividir imagen en partes | - | Copy Sectors | MAN | M B | `mr-image` | No iniciado | - | - |
| CL-08 | Bloquear origen durante la copia | - | Copy Sectors | MAN | M B | `mr-io-win` | No iniciado | VM | - |
| CL-09 | Exportar rangos malos/saltados | Para mapear ficheros dañados | Copy Sectors | MAN | M B | `mr-clone` + `mr-report` | No iniciado | - | - |
| CL-10 | Rellenar sectores | Patrón | Tools → Fill Sectors | MAN, F1-04 | M B | `mr-hex` | No iniciado | Solo imágenes | - |
| CL-11 | Clonado a disco físico | Escritura | Copy Sectors | MAN | M | - | No iniciado | - | **Bloqueado** hasta hito de escritura segura |

## 10. Editor hexadecimal y herramientas

| ID | Funcionalidad | Descripción | Ref. DMDE | Evidencias | Verif. | Diseño MemRescue | Estado | Pruebas necesarias | Diferencias pendientes |
|---|---|---|---|---|---|---|---|---|---|
| HX-01 | Vista hex/texto con codificaciones | - | Editor/Mode | MAN, F1-04 | M B | `HexDump` (C#) + visor WPF | **Implementado parcial** (M1: hex + ASCII con desplazamientos absolutos, 1–8 sectores por página, estado de validez en la cabecera de cada sector y aviso de sectores no fiables, barra de posición para imágenes grandes; colores por estado dentro del volcado con marcas de texto, barra lateral en los no fiables, fila de estados por sector y leyenda, contraste ≥ 7:1 y colores del sistema con alto contraste, ADR-021) | Hecho: `HexDumpTests`, `FillPatternTests` (paleta y contraste), autodiagnóstico, pruebas de UI Automation (VHD, barra de posición, estados por sector con ilegibles y dispersos, leyenda) | Sin codificaciones alternativas |
| HX-02 | Vistas de estructura integradas (MBR, GPT, boot, FAT dir, MFT, INDX…) | - | Mode | MAN, F1-04 | M B | Plantillas propias | No iniciado | Cada estructura | Lista completa de vistas (INV-09) |
| HX-03 | Plantillas personalizadas | Lenguaje de plantillas | Mode | MAN, F1-09 | M B | Lenguaje propio documentado | No iniciado | Intérprete | No compatible con `template.txt` de DMDE |
| HX-04 | Navegación (ir a sector/cluster/registro/offset, objetos FS) | - | Editor | MAN, F1-04 | M B | Comandos WPF + `mr_source_read` | **Implementado parcial** (M1: ir a sector, ir a desplazamiento, anterior/siguiente, primero/último, barra de posición, con validación de rango) | Hecho: análisis de números (`HexDumpTests`), lectura en LBA exactas (corpus), 11 pruebas de UI Automation | Cluster, registro y objetos FS (M3+) |
| HX-05 | Modo edición + colores (modificado, no usado, error E/S, error lógico) | - | Edit | MAN | M | `mr-hex` + `mr-changes` | No iniciado | - | - |
| HX-06 | Buscar cadena / sector especial | - | Tools → Search | MAN, F1-04 | M B | `mr-hex` | No iniciado | - | - |
| HX-07 | Copiar/pegar bloques, exportar a texto | - | Edit | F1-04 | B | `mr-hex` | **Implementado parcial** (M1: copiar el volcado visible; selección de texto) | - | Pegar (requiere escritura, M12), exportar a fichero |
| NT-01 | Herramientas NTFS sin driver: copiar dentro, crear dir, borrar, reparar INDX | Escritura FS | Tools → NTFS Tools | MAN, F1-04 | M B | `mr-fs-ntfs::write` | No iniciado | Solo imágenes + chkdsk en VM | Alto riesgo: último hito |
| NT-02 | Crear lista de atributos NTFS | Avanzado | Undocumented | F1-04 | B | - | No iniciado | - | Baja prioridad |

## 11. Aplicación y experiencia de usuario

| ID | Funcionalidad | Descripción | Ref. DMDE | Evidencias | Verif. | Diseño MemRescue | Estado | Pruebas necesarias | Diferencias pendientes |
|---|---|---|---|---|---|---|---|---|---|
| UX-01 | Flujo selector → particiones → volumen → árbol → recuperar | Flujo principal | Manual "Work with Program" | MAN | M | ARQUITECTURA §5 | No iniciado | Pruebas de UI guiadas | Disposición exacta de DMDE no documentada (INV-12) |
| UX-02 | Menús Disco/Herramientas/Ventanas/Editor/Modo/Edición | Organización | Menu | MAN, F1-04 | M B | Misma organización lógica, textos propios | **Implementado parcial** (M1: los 6 menús; cada entrada funciona o está deshabilitada con su hito) | Revisión de cobertura; captura `--screenshot` | Submenús Windows/Editor/Mode/Edit de la referencia sin documentar (INV-12) |
| UX-03 | Panel de ficheros con marcadores de estado | Borrado/encontrado/RAW/fragmento | File panel | MAN | M | UI con iconos propios | No iniciado | - | - |
| UX-04 | Disposiciones de ventana (árbol, panel, editor) | - | Windows | F1-04 | B | UI | No iniciado | - | - |
| UX-05 | Atajos de teclado familiares | Ctrl+E, Ctrl+W, F2, Insert/Espacio… | Varios | MAN, F1-02 (lista de atajos) | M B | Mapa de atajos configurable | **Implementado parcial** (M1: Ctrl+O, Ctrl+F4, Ctrl+G, Ctrl+Mayús+G, Ctrl+Mayús+R, RePág/AvPág, Ctrl+Inicio/Fin, Ctrl+Mayús+C) | - | Lista completa de la referencia ?; atajos no configurables aún |
| UX-06 | Idiomas | Localización | Language | MAN, F1-09 | M B | Recursos `.resx` propios (WPF) | No iniciado | - | ES/EN primero |
| UX-07 | Tema oscuro, alto DPI | - | "Dark Mode (Unofficial)" | F1-04 | B | WPF: PerMonitorV2 (hecho en M0) + temas claro/oscuro propios | No iniciado (DPI: configurado) | Captura con `--screenshot` | Tema oscuro pendiente |
| UX-08 | Pistas para usuarios noveles | Ayudas contextuales | - | F1-04 | B | UI | No iniciado | - | - |
| UX-09 | Portable sin instalación | - | Base | MAN | M | Publicación WPF autocontenida (ADR-011) | **Implementado** (M0) | Autodiagnóstico del exe publicado sin .NET instalado: superado 2026-10-17 | Falta empaquetado (zip/firma) y configuración junto al exe |
| UX-10 | Activación/licencias | - | - | - | - | **No aplica** (no se replica) | - | - | - |

## Resumen

| Bloque | Funcionalidades | Con lógica verificada en DMDE (D) | Implementadas | En paridad |
|---|---|---|---|---|
| E/S | 14 | 7 | 1 (IO-08) + 3 parciales (IO-06, IO-07, IO-09) | 0 |
| Imágenes | 8 | 0 | 1 probada (IM-01) + 4 parciales (IM-02…05); IM-06 solo detección | 0 |
| Particiones | 11 | 4 | 0 (INV-05 hecha) | 0 |
| FS | 15 | 6 | 0 | 0 |
| Escaneo | 13 | 2 | 0 | 0 |
| RAW | 8 | 0 | 0 | 0 |
| Recuperación | 14 | 1 | 0 | 0 |
| RAID | 11 | 0 | 0 | 0 |
| Clonado | 11 | 0 | 0 | 0 |
| Editor/herramientas | 9 | 0 | 3 parciales (HX-01, HX-04, HX-07) | 0 |
| Aplicación | 10 | 0 | 1 (UX-09) + 2 parciales (UX-02, UX-05) | 0 |
| **Total** | **124** | **20** | **15 (1 probada, 2 implementadas, 12 parciales)** | **0** |

Última actualización: 2026-10-17, tras M1 (iteración 2). Ninguna funcionalidad alcanza todavía "Paridad":
IM-01 está probada, pero el diálogo de referencia abre también logs y busca particiones al abrir (M2).
IM-03…05 tienen sus variantes principales probadas contra qemu-img, pero faltan diferenciales y otras
variantes; reconocer una cabecera **no** cuenta como soporte (solo cuentan las variantes con lectura
verificada byte a byte contra una referencia independiente).
