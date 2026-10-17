# Documentación oficial de DMDE consultada (2026-10-17)

Resumen de las páginas públicas del manual de DMDE 4.4 usadas para la matriz de paridad.
Se parafrasea; no se copian textos. Cuando el manual **no** cubre algo, se indica.

Fuentes:
[editions](https://dmde.com/editions.html) ·
[about](https://dmde.com/manual/about.html) ·
[menu](https://dmde.com/manual/menu.html) ·
[fullscan](https://dmde.com/manual/fullscan.html) ·
[reconstruction](https://dmde.com/manual/reconstruction.html) ·
[datarecovery](https://dmde.com/manual/datarecovery.html) ·
[partitions](https://dmde.com/manual/partitions.html) ·
[raids](https://dmde.com/manual/raids.html) ·
[copysectors](https://dmde.com/manual/copysectors.html) ·
[devioparams](https://dmde.com/manual/devioparams.html) ·
[devices](https://dmde.com/manual/devices.html) ·
[diskeditor](https://dmde.com/manual/diskeditor.html) ·
[filepanel](https://dmde.com/manual/filepanel.html).
Índice completo del manual: <https://dmde.com/manual/toc.html>; PDF: `/docs/dmde-4-4-4-manual.pdf`.

## Ediciones

Free (árbol de directorios; 4 000 ficheros por petición), Express (licencia de 1 año, uso personal),
Standard (perpetua, uso personal y de organización), **Professional** (uso con datos de clientes).

Funciones exclusivas de **Professional** según la web:
recuperación para clientes · activación portátil y de un uso (no aplica a MemRescue) ·
**lista de ficheros en HTML** · **informes de recuperación con logs y checksums** ·
**lectura de imágenes E01** · **log para reanudar la copia de disco** (y reintento de sectores
malos desde el log) · **personalización del script de E/S** · **recuperación de flujos
alternativos NTFS** · DMA en DOS (no aplica).

## Capacidades generales

Portátil (Windows/macOS/Linux/DOS); FS: NTFS, FAT12/16/32, exFAT, ReFS, Ext2/3/4, Btrfs,
HFS+/HFSX, APFS; escaneo completo por FS y RAW; reconstrucción de FS; gestor de particiones;
clonado/imagen con manejo de errores y copia inversa; RAID virtual 0/1/4/5/6, paridad retrasada,
striping personalizado, JBOD, cálculo automático; mapa de clusters; editor de disco con plantillas;
herramientas NTFS sin driver; imágenes VHD; NTFS comprimido y EFS; nombres nacionales; discos,
ficheros y sectores grandes.

## Menús (estructura)

- **Disk/Drive**: Select Disk/Task · Open Image/Log · Save Scan Log · Construct RAID · Partitions ·
  Lock Volumes · Device I/O Parameters · Global Read-only Mode · Load Rollback/Dump · Dump Changes ·
  Undo/Redo/Reset/Apply Changes.
- **Tools**: Full Scan · All Found/Virtual FS · Search in Folders · Recover/Create File List ·
  Restore EFS from Backup · Update Cluster Map · Cluster Map Report · Reopen Volume Parameters ·
  NTFS Tools · Copy Sectors · Fill Sectors · Search.
- **Windows**, **Editor**, **Mode**, **Edit**: el manual tiene subpáginas; no se han consultado aún.
- El manual **no** describe la disposición de la ventana principal ni la barra de herramientas.

## Full Scan

Área de escaneo, FS a buscar, opciones de cálculo de volúmenes; escaneo "a saltos" (encuentra antes
los volúmenes grandes); pausa/reanudación; abrir un volumen antes de terminar; varias **versiones
de volumen**. NTFS/exFAT/HFS se contrastan con ficheros hallados por firma. Columnas: **%** (enlaces
correctos), **Check** (nº de enlaces), **Min. Size**; indicadores `+`/`-` para B/C (boot/copia),
F (FAT), R (raíz). Guardar/cargar log y autoguardado. RAW: resultados bajo `$Raw`, filtro con mapa
de clusters; solo primer fragmento de ficheros fragmentados; firmas personalizadas con `?` y `\mXY`,
offset variable dentro de los primeros 4096 bytes, generación desde muestras, opción "fiable"
(≥ 4 bytes fijos distintos), contenedores, firma EOF.

## Reconstrucción (FS virtual)

Modos: rápido (solo FS original), re-escanear FS actual, por defecto (con Full Scan), completo
(con extras, control **Más/Menos**) y personalizado (elegir fragmentos). FAT: elegir FAT1/FAT2/ninguna,
"no usar sectores malos". NTFS: filtro por nº de MFT, mejorar fusión de INDX por fechas, procesado
auto/completo de INDX, journal, registros desplazados, extras, registros técnicos. Grupos de
fragmentos: mejores, relacionados, desconocidos, ajenos, pequeños, desactivados. ReFS v3: ID de
volumen en nodos de directorio, versiones antiguas.

## Recuperación

Flujo: comprobar salud del disco → abrir volumen → escaneo rápido/completo → Más/Menos → seleccionar →
recuperar **a otro disco**. Categorías; cálculo de tamaño opcional. Listas: fichero, **HTML (Pro)**,
lista de sectores (con rangos, para clonado parcial). **Informe (Pro)**: ficheros, errores de E/S,
CRC-32/MD5/SHA-1/SHA-256. Filtros: máscaras con `*`/`?`, exclusión con `\`, tamaño, fecha, ID;
"solo directorios con ficheros". Conflictos: renombrar, auto, omitir, fusionar; umbral; errores:
omitir/ignorar/ocultar/mover a `$Bad`; manejadores de eventos. Nombres Unicode/transliteración,
rutas largas, dividir ficheros grandes. **ADS (Pro)**. EFS: `.efs/.efk`, `.efb` portátil, o
fichero cifrado NTFS.

## Particiones

MBR y GPT (híbrido/dinámico solo como encontrados). Modo avanzado obligatorio para modificar; cambios
virtuales hasta aplicar. Indicadores **T** tabla, **E** entrada, **B** boot, **C** copia, **F**
estructuras básicas, **f** MFTMirr, **x** ausente/dañado; activa **(A)**; errores en rojo. Búsqueda
exprés de particiones perdidas al abrir. Acciones: abrir, parámetros, letras, Full Scan en la
partición, imagen/clon, insertar (undelete), quitar, crear RAW, restaurar boot desde copia, activa,
MBR On/Off, GPT On/Off, escribir boot sectors; deshacer/rehacer, guardar/cargar particionado.

## RAID

0, 1 (solo lectura), 4, 5 (4 rotaciones, paridad retrasada), 6 (solo lectura, un NULL), rotación
personalizada (números = orden de datos, −1 = XOR, −2 = sin uso), JBOD (lectura/escritura).
Parámetros: stripe (bytes), delay, offset/tamaño por disco, orden, disco NULL, read-ahead,
guardar/cargar. Cálculo automático estadístico con restricciones; paso de offset LBA (2048 ≈ 1 MiB).

## Copia de sectores (imagen/clon)

Origen/destino: dispositivo, partición, fichero, editor, selección. Rangos en sectores o bytes;
dividir destino (genera descriptor para reabrir); copia inversa; doble hilo; **log** con flush
periódico; **reanudar con log (Pro)**; **reintentar sectores malos desde log (Pro)**; reintento de
saltados en pasadas posteriores; bloqueo de volúmenes de origen; exportar a **mapfile de ddrescue**;
exportar rangos malos/saltados.

## Parámetros de E/S

Interfaces IO FILE / IO SCSI / IO ATA (+ overlapped, no recomendado). Reintentos CRC (lectura sector
a sector tras error) y de seek; relleno de sectores malos y saltados (patrón de 4 bytes LE); salto
sobre sectores malos y lectura hacia atrás; omitir errores por código; continuar si no está listo;
tamaño de búfer; timeouts; refrescar handle/lista de dispositivos; reanudar al reconectar;
**script de manejo de E/S (Pro)**; informe SMART.

## Selector de dispositivos

Dispositivos físicos / volúmenes lógicos; botón de parámetros de E/S; opción "mostrar particiones".
Imágenes: sector a sector, VHD, VHDX, VMDK (solo lectura), **E01 (Pro)**; logs de RAID y de escaneo.
El manual **no** describe las columnas de la lista.

## Editor de disco

Vista hex y vistas con formato/plantillas (menú Mode), plantillas personalizadas; modo edición
(Ctrl+E); aplicar cambios (Ctrl+W); datos modificados en amarillo, no usados en gris, errores de E/S
en verde, errores lógicos en rojo; buscar cadena; buscar sector especial; copiar/rellenar sectores.
NTFS: edición de ficheros solo en modo RAW.

## Panel de ficheros

Árbol virtual con directorios virtuales (`$Root`, `$MetaData`, `$NonameFiles`, `$Raw`…); flags
E/C/s, D, R/H/S/A, x (borrado), f (encontrado); marcadores de directorio y fichero que combinan
normal/borrado/encontrado/RAW (+EOF, sin EOF, tamaño estimado) y fragmentos (`[J]` journal); ADS como
`fichero:flujo`. Marcar con Insert/Espacio/casilla; menú contextual con F2/Shift+F10/clic derecho;
Enter abre/previsualiza; Ctrl+Enter abre en el editor. El manual **no** describe columnas ni ordenación.
