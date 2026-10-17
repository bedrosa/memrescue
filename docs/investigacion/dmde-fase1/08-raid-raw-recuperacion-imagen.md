# 08 · RAID, búsqueda RAW, recuperación e imagen

> Estas áreas **no tienen todavía funciones decompiladas** (análisis de código pendiente). Todo lo que sigue se basa en cadenas del binario, la tabla de UI y ficheros
> auxiliares, y se marca como OBS (cadena presente) o INF (comportamiento deducido).

## RAID / dispositivos virtuales

**OBS (cadenas 0x6c1d64)**: tipos soportados:
RAID 0 (stripe), RAID 1 (mirror), RAID 4 (paridad dedicada), RAID 5 (paridad *forward*, *backward*,
*backward dynamic*, *forward dynamic*), RAID 6 (mismas 4 variantes), rotación personalizada,
JBOD/spanned, "DVHD: Differencing VHDisks".

**OBS (claves 0x5fd3fc)**: `raidtype`, `stripe`, `raidbps`, `raiddelay`, `raidflags`, `raidrotation`,
`dev=…,{…},offset,size` por miembro, `;NULL-LBA` (miembro ausente). Ficheros `raidinfo.ini`,
`raiddev.ini`, `raidnull.ini`.

**OBS (claves 0x677594)**: cálculo automático: `params=`, `offset=`, `devfixorder=`, `testnum=`,
`conf[%l]=`, `configs=`; UI 3070–3101: nº de discos, niveles, rango de stripe, rango de *delay*,
paso de offset LBA, orden fijo, tamaño de muestra.

**INF**: el cálculo genera combinaciones (nivel × stripe × delay × orden × offset), las puntúa
leyendo muestras y "una configuración se impondrá si los parámetros son correctos" (mensaje 3101).
La métrica concreta no se ha localizado. No se ha verificado la presencia de Reed-Solomon/GF(2^8)
para RAID-6; el mensaje `1552=Write is not supported for this RAID type` indica escritura limitada.

## Búsqueda RAW por firmas

**OBS (0x6d2167–0x6d36e8)**: tabla integrada con firmas para HTML/XML, REGEDIT4, PDF, PostScript,
ejecutables DOS/PE, JPEG, AVI, GIF, RTF/DOC, VDI, MP3, RAW de cámaras (Samsung, Nikon, Sony, Epson,
Hasselblad, Kodak, Mamiya, Pentax, Canon, Fuji), BMP, OLE/CFB (xls/ppt/msi/thumbs.db), ASF/WMV,
AIFF, MIDI, M2TS, BZ2, TAR (ustar), OGG, GZ, OOXML (`[Content_Types].xml`), ODF (`mimetypeapplication/vnd.oasis…`),
APK/JAR, iWork, ZIP, Matroska, HEIC, MOV/MP4 (`ftyp`), MPEG, VHD/VHDX/VMDK, PST, DjVu,
`wallet.dat`, InDesign. Categorías: Archives, Documents, Graphics, Media, DiskImages, Executable, Other.

**OBS (claves 0x6d9534)**: definición configurable en `rawsign.ini`, `ft_op`, `ft_ext`,
`ft_szb` (límites de tamaño), `ft_sign` / `ft_signex` (firma con offset mín/máx), `ft_sort`;
condiciones `[OR]`, `[AND]`, `[AND NOT]`; firma EOF; banderas "reliable (use as EOF)",
"dense data w/o gaps", "file-container", "text file (EOF)", "use for FS calculation".

**INF**: los resultados RAW se filtran con el mapa de clusters del FS (excluir clusters usados,
truncar al encontrar un cluster usado, solo alineados a cluster: mensajes 3630–3640) y se
deduplican frente a resultados por FS ("also found by signatures", "not found by signatures").

## Motor de recuperación (OBS por cadenas 0x833310–0x83fce8)

- Estados por fichero: `Skipped (BAD)`, `Renamed`, `Merged`, `Splitted @%I`, `Aborted`, `Completed`,
  `FS error @%I, bytes: %I`, `I/O error @%I, bytes: %I`, `ERR (>$Bad\)`, `ERR (+Hidden)`.
- Informe `rreport.txt` / CSV con columnas CREATION/WRITE/ACCESS/CHANGE TIME, FLAGS, PATH NAME,
  ACTION, STATUS, BYTES, CRC-32, SHA-1, SHA-256 (MD5 en el mismo bloque, 0x835d5a).
- Rutas largas `\\?\UNC\`, restauración de fechas (`SetFileInformationByHandle`), EFS raw.
- Listas exportables: `filelist.txt`, `filelist.htm` (plantilla `listtmpl.htm` con `[TITLE]`,
  `[NUM FILES]`, `[TOT SIZE]`…), `cluslist.txt`.

## Copia de sectores / imagen / clonado (OBS por cadenas 0x744fe0–0x749637)

- Configuración `[DevCopy]`: `srcdev`, `dstdev`, `srcobj`, `dstobj`, `srcfile`, `dstfile`, `srcofs`,
  `srcsize`, `dstofs`, `copyback` (copia inversa), `copythreads` (2 hilos), `srclock`, `copylog`,
  `copylogblock`, `copylogread`, `copylogbads`, `copyautostart`; `devcopy.ini`.
- Log de rangos copiados (`cranges.log`), exportación a **mapfile de GNU ddrescue**
  (`# current_pos  current_status` / `#      pos        size  status`, 0x7477fc).
- Informes con plantilla (`%DEVMODEL%`, `%DEVSERIAL%`, `%STARTLBA%`, `%TMSTARTTXT%`, `%REPUUID%`…).

## Licencias y activación (solo arquitectura)

Por instrucciones del encargo y por respeto a la protección del producto, solo se documenta la
existencia de los componentes, no su lógica:
- Ficheros `license.key` / `lickey.txt`, `dmdeinst.dat`, `acode.txt`.
- Activación en línea (HTTP POST `application/x-www-form-urlencoded` vía sockets/WinINet/WinHTTP con
  proxy), manual (códigos/QR) y "portátil" ligada a un disco.
- Ediciones: en el binario aparecen "Free Edition", "Client", "Unregistered" y, en el changelog,
  "DMDE Professional". Sin activación: límite de 4 000 ficheros y "solo panel activo"
  (mensajes 2198–2215).

Para una alternativa propia, esta parte **no es relevante** y no debe replicarse.
