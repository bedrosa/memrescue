# Roadmap de MemRescue

Organizado en **hitos** (orden de construcción) y **módulos**. Cada hito termina con pruebas que
mueven filas de `PARIDAD_DMDE_PRO.md` a "Probado". Las tareas de investigación (INV) se ejecutan
**una a una**, justo antes del hito que las necesita, con consultas acotadas.

Seguimiento operativo: GitHub Issues, con un milestone por hito (M1 a M12). Este documento describe los
hitos; `PROGRESO.md` recoge los resultados verificados y `PARIDAD_DMDE_PRO.md` la paridad (ver
`CONTRIBUTING.md`).

## Prioridades del propietario y cómo se reflejan

| Prioridad indicada | Dónde se aborda | Nota de dependencia |
|---|---|---|
| 1. Motor de recuperación | M4 (NTFS) y se amplía en M6–M8 | Necesita E/S, imágenes y al menos un FS (M1–M3) |
| 2. RAW | M7 | Necesita escaneo (M6) |
| 3. RAID y paridad | M5 | Solo necesita la capa de bloques (M1) |
| 4. FS distintos de NTFS | M8 | Necesita VFS (M4) |
| 5. Escaneo y particiones perdidas | M2 (exprés) y M6 (completo) | |
| 6. Errores de lectura | M1 (simulado) y M9 (dispositivos reales) | |
| 7. Clonado avanzado | M9 | |
| 8. Herramientas profesionales | M10–M11 | |

El orden de construcción respeta las dependencias técnicas; las prioridades se usan para ordenar
dentro de cada hito y para no retrasar la investigación de los temas 1–3.

---

## M0 · Fundaciones (sin funcionalidad de usuario): **TERMINADO 2026-10-17**

Resultado en `PROGRESO.md` (etapa 3). Pendiente heredado: oráculo de terceros para el corpus (WSL/VM).

- ~~Confirmar ADR-003/004/005/007/008/010.~~ Hecho (WPF en lugar de WinUI 3; ADR-007 sigue propuesta para M9).
- Repositorio git, workspace Rust, solución .NET, CI local (build + tests + clippy + fmt).
- `mr-common`: tipos LBA/rangos, `Confidence`, errores, hashes (CRC-32, MD5, SHA-1, SHA-256 vía crates auditados).
- **Generadores de imágenes sintéticas** y manifiesto del corpus (ver `PRUEBAS.md`).
- Esqueleto de `mr-api` (tareas, progreso, cancelación) y `memrescue-cli`.
- **Salida**: build reproducible; primeras imágenes del corpus con hashes.

## M1 · Capa de bloques e imágenes (solo lectura): **TERMINADO 2026-10-18**

Cerrado por el propietario tras la validación de cierre (`PROGRESO.md`, etapa 7). Las variantes adicionales
de contenedor pasan a M1.5 y no bloquean M2 a M4.

Hecho en la iteración 1 (detalle en `PROGRESO.md`, etapa 4):
- ✔ Revisión técnica previa con 25 correcciones y pruebas de regresión (`REVISION_TECNICA_M1.md`).
- ✔ `ImageFile` endurecido (política de rutas), tamaños de sector 512/1024/2048/4096, `read_sectors` estricto,
  límite de 64 MiB por petición.
- ✔ Lector tolerante (`resilient`): bloques → sector a sector, reintentos, relleno, informe de rangos,
  progreso y cancelación; `FaultyDevice` (simulador de sectores permanentes e intermitentes).
- ✔ Detección de contenedores (VHD, VHDX, VMDK, EWF, QCOW, VDI, DMG) como punto de extensión.
- ✔ ABI 2, CLI (`probe`, `sectors`, `verify`), interfaz WPF: visor de sectores, navegación, comprobación de legibilidad.

Hecho en la iteración 2 (detalle en `PROGRESO.md`, etapa 5):
- ✔ qemu-img 11.1.2 portable con procedencia verificada (firmas MSYS2) como referencia independiente,
  fuera de Git y del producto (`HERRAMIENTAS_TERCEROS.md`).
- ✔ VHD fijo y dinámico, VHDX fijo y dinámico, VMDK monolithicSparse (`mr-image`), probados byte a byte
  contra qemu-img; diferenciales, padres y extensiones externas → `ExternalReference` (nunca se abren).
- ✔ Mapa de validez por sector (leído / recuperado / ilegible / saltado / disperso) en todo el motor, la
  ABI 3, la CLI y el visor; el relleno nunca se presenta como dato.
- ✔ Salto sobre sectores malos y lectura hacia atrás (IO-07), relleno distinto para saltados (IO-08).
- ✔ Rango de sectores como dispositivo (`RangeView`), imágenes divididas (IM-02, lectura), adaptador de
  tamaño de sector para contenedores.
- ✔ Prueba automática de la interfaz (UI Automation, 9 pruebas), barra de posición para imágenes grandes.

Completado en M1 por issues:
- ✔ `NullDevice` (ceros, asignación dispersa) y `PatternDevice` (patrón constante).
- ✔ Omitir errores por código y modo «ignorar todo» (IO-09): sin reintentos para los códigos elegidos, sectores siempre `Unreadable`.
- ✔ Tablas de asignación parcialmente dañadas en VHD/VHDX/VMDK (issue #4, ADR-020): modo estricto por
  defecto y modo tolerante solo por activación explícita (API, ABI `MR_OPEN_TOLERANT`, CLI `--tolerant`,
  confirmación y aviso en la interfaz); los bloques descartados se entregan como ilegibles.
- ✔ Patrones de relleno configurables desde la interfaz (preferencias por usuario) y colores por estado
  dentro del volcado (issue #7, ADR-021): `mr_source_read_mapped_ex` sin cambiar la ABI 3, CLI
  `sectors --bad-fill/--skip-fill`, leyenda y estados por sector accesibles.

- ✔ Revisión de robustez de cierre: memoria acotada al validar tablas de asignación (#38), metadatos de
  asignación ilegibles tratados como datos en lugar de abortar (#39), endurecimiento y pruebas de corrupción
  (#40).

- **Paridad objetivo**: IM-01, IM-02, IM-03, IM-04, IM-05, IO-06…IO-09 (simulados).

## M1.5 · Variantes adicionales de contenedores

Milestone propio para no retrasar particiones y sistemas de archivos. Exigen abrir otros ficheros con
confirmación explícita del usuario o descomprimir; hoy se rechazan con errores tipados
(`ExternalReference`, `UnsupportedFeature`).
- VHD/VHDX diferenciales con selección explícita del padre (#8).
- VMDK streamOptimized (#9).
- Reproducción en memoria del registro pendiente de VHDX (#10).
- VMDK multiextensión y flat con confirmación explícita (#11).

## M2 · Particiones: **EN CURSO** (plan técnico en `PLAN_M2.md`)

- INV-05 (acotada) → MBR/EBR, GPT con copia y tolerancias, APM.
- Búsqueda exprés de particiones perdidas (boot sectors/superbloques en ubicaciones típicas).
- Indicadores de estructuras.
- **Paridad objetivo**: PT-01…PT-06, PT-10 (lectura/guardar).

## M3 · NTFS lectura

- `mr-fs-ntfs`: boot, MFT, fixups, atributos, runs, índices, LZNT1, sparse, ADS, $LogFile (lectura).
- Ficheros borrados (registros MFT no en uso, entradas INDX residuales).
- **Paridad objetivo**: FS-01…FS-04.

## M4 · VFS + motor de recuperación v1  *(prioridad 1)*

- INV-01 → `mr-vfs` (árbol, estados, carpetas sintéticas) y `mr-recover` (copiado, filtros,
  categorías, conflictos, errores, rutas largas, división, fechas).
- `mr-report`: listas TXT/CSV/HTML, informe con hashes, lista de sectores.
- CLI: `recover` de extremo a extremo sobre imágenes NTFS.
- **Paridad objetivo**: RC-01…RC-13, FS-05 (lectura raw EFS).

## M5 · RAID virtual  *(prioridad 3)*

- INV-03b → RAID 0/1/4/5 (4 rotaciones + delay)/6, rotación personalizada, JBOD, NULL, offsets.
- Reconstrucción de paridad XOR (RAID 4/5) y P+Q (RAID 6) en lectura.
- Cálculo automático de parámetros (diseño propio, estadístico).
- **Paridad objetivo**: RD-01…RD-10.

## M6 · Escaneo completo y reconstrucción

- INV-03 → `mr-scan` (a saltos, persistencia SQLite, pausa/reanudación), escáner NTFS
  (FILE/INDX), candidatos de volumen con calidad, versiones, grupos de fragmentos.
- Modos de reconstrucción y Más/Menos; opciones NTFS avanzadas.
- **Paridad objetivo**: SC-01…SC-11.

## M7 · RAW por firmas  *(prioridad 2)*

- INV-02 → biblioteca de firmas propia, matcher (comodines, máscaras, offsets), EOF, límites,
  calculadores de tamaño por formato, filtrado con mapa de clusters, asistente de firmas.
- Investigación propia sobre recuperación de fragmentados (RW-08, mejora opcional).
- **Paridad objetivo**: RW-01…RW-07, SC-12.

## M8 · Otros sistemas de archivos  *(prioridad 4)*

Orden propuesto por uso real y disponibilidad de especificación:
FAT → exFAT → Ext2/3/4 → HFS+ → APFS → Btrfs → ReFS → ISO9660.
Cada FS: lector + escáner de fragmentos + reconstrucción (INV-04 por FS, una cada vez).
- **Paridad objetivo**: FS-07…FS-15, SC-06, SC-10.

## M9 · Dispositivos reales y clonado  *(prioridades 6 y 7)*

- `mr-io-win`: enumeración, solo lectura, IOCTLs, SCSI/ATA pass-through, SMART/NVMe, bloqueo.
  Pruebas en VM con discos virtuales antes de cualquier disco físico.
- INV-06 → política de errores en hardware real.
- `mr-clone`: copia de sectores, inversa, doble hilo, log/reanudar, reintentos, mapfile ddrescue,
  división, exportación de rangos. Destino: solo ficheros de imagen.
- E01 (IM-06).
- **Paridad objetivo**: IO-01…IO-14, CL-01…CL-09.

## M10 · Interfaz completa

- Ventanas: selector, particiones, principal (árbol/panel/editor), escaneo, reconstrucción,
  recuperación, RAID, clonado, informes, ajustes; tema oscuro; ES/EN.
- INV-12 → revisión de la organización de menús y paneles de DMDE con el manual completo.
- **Paridad objetivo**: UX-01…UX-09.

## M11 · Editor hex y herramientas profesionales  *(prioridad 8)*

- `mr-hex`: vistas, plantillas integradas y lenguaje propio de plantillas, navegación, búsqueda,
  `mr-changes` (deshacer, rollback, aplicar solo sobre imágenes).
- INV-07 → script de manejo de E/S (IO-11).
- Mapa de clusters y su informe.
- **Paridad objetivo**: HX-01…HX-07, IO-11, SC-12.

## M12 · Escritura segura (último)

- Activar escritura sobre dispositivos físicos tras auditoría: edición de particiones, editor,
  rellenar sectores, clonado a disco, herramientas NTFS (NT-01).
- **Paridad objetivo**: PT-07…PT-09, CL-10, CL-11, NT-01, RD-07 (escritura).

---

## Investigaciones adicionales necesarias

Cada INV: alcance cerrado, fuentes en este orden: (1) especificación pública, (2) manual oficial,
(3) REA sobre `dmde.exe` con consultas acotadas a funciones concretas. Resultado: un documento
`docs/investigacion/dmde-faseN/INV-xx-*.md` con OBSERVADO/INFERIDO y evidencias.

| ID | Tema | Origen | Antes de | Pregunta concreta | Punto de partida |
|---|---|---|---|---|---|
| INV-01 | Motor de recuperación | Área pendiente "arranque/recuperación" | M4 | ¿Cómo escribe ficheros con sectores malos, renombra, divide y registra? | Cadenas 0x833310–0x83fce8 (F1-08) |
| INV-02 | Algoritmos RAW | Área "RAID/RAW" | M7 | Estructura de firma, granularidad del barrido, cálculo de tamaño por formato | Tabla 0x6d2167…, claves 0x6d9534 |
| INV-03 | Puntuación de volúmenes candidatos | Pendiente NTFS | M6 | Métrica de % / Check / votos | `FUN_006a6d80`, `FUN_006a89b0`, `FUN_006a8c50` |
| INV-03b | RAID: mapeo, delay, cálculo automático | Área "RAID/RAW" | M5 | Fórmulas de rotación y métrica de detección | Cadenas 0x6c1d64, 0x677594; manual RAID |
| INV-04 | FS no NTFS (uno por sesión) | Área "otros FS" | M8 | Validadores y escáneres de FAT, exFAT, HFS+, ReFS, Btrfs | Rangos del doc F1-03 |
| INV-05 | MBR/EBR y búsqueda exprés de particiones | Área "otros FS" | M2 | **Hecha** (2026-10-17): `investigacion/dmde-fase2/INV-05-…md`. Bucle de candidatos: INV-05b (2026-10-18, `investigacion/dmde-fase2/INV-05b-…md`, propuesta propia) | 0x608b00–0x613300 |
| INV-06 | Bucle de lectura con errores, CDB SCSI/ATA | Área "E/S" | M9 | Secuencia de reintentos/saltos/retroceso; opcodes | 0x4d6b00, 0x4d7730, 0x4d7f20, 0x4d9870 |
| INV-07 | Script de manejo de E/S | Área "arranque/scripts" | M11 | Gramática y semántica | `deviohsc.txt`, 0x4c3e80 |
| INV-08 | Escritura NTFS sin driver | Nueva | M12 | Qué estructuras actualiza al crear/borrar | Mensajes 1780–1823 |
| INV-09 | Vistas de estructura del editor | Nueva | M11 | Lista completa de vistas integradas | 0x7566f0–0x765400, manual Mode |
| INV-10 | Formato APFS/ReFS: disponibilidad de imágenes de prueba | Nueva | M8 | ¿Cómo generar imágenes APFS y ReFS reproducibles en este entorno? | - |
| INV-11 | E01 | Nueva | M9 | Especificación EWF y herramientas libres para generar corpus | Documentación libewf |
| INV-12 | Organización completa de la UI | Nueva | M10 | Submenús Windows/Editor/Mode/Edit, columnas de listas, disposición | Páginas del manual aún no consultadas |
