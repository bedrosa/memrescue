# Análisis REA de DMDE 4.4.4.842: Fase 1

Objetivo: entender el funcionamiento de `dmde.exe` (DM Disk Editor and Data Recovery Software)
para diseñar después una alternativa propia con implementación original. En esta fase **no se ha
modificado el ejecutable ni se ha escrito código**.

## Documentos

| # | Documento | Contenido |
|---|---|---|
| 01 | [Identificación y arquitectura](01-identificacion-y-arquitectura.md) | Hash, PE32+ x64, secciones, toolchain Delphi, capas |
| 02 | [Dependencias y tecnologías](02-dependencias-y-tecnologias.md) | Imports estáticos y dinámicos, zlib, MD5/CRC/SHA, formatos |
| 03 | [Inventario de funciones y módulos](03-inventario-funciones-y-modulos.md) | 15 552 funciones, mapa por rangos, funciones nombradas |
| 04 | [Mapa funcional de la interfaz](04-mapa-funcional-interfaz.md) | Funcionalidades según los 1 549 mensajes de UI |
| 05 | [Algoritmos verificados](05-algoritmos-verificados.md) | NTFS, GPT, ExtFS, APFS, escaneo NTFS, hashes |
| 06 | [E/S de dispositivos](06-io-dispositivos.md) | IOCTLs, enumeración, errores de lectura, SMART/NVMe |
| 07 | [Particiones y sistemas de archivos](07-particiones-y-sistemas-de-archivos.md) | Matriz de soporte por formato |
| 08 | [RAID, RAW, recuperación e imagen](08-raid-raw-recuperacion-imagen.md) | Basado en cadenas (pendiente de código) |
| 09 | [Configuración, scripts y plantillas](09-configuracion-scripts-plantillas.md) | INI, `.lng`, script de E/S, lenguaje de plantillas |
| 10 | [Funcionalidades verificables](10-funcionalidades-verificables.md) | Nivel de verificación por funcionalidad + conclusiones |
| 11 | [Pendientes y fase 2](11-pendientes-y-fase-2.md) | Huecos abiertos, plan siguiente, pautas de reimplementación |
| - | `datos/imports_por_dll.txt` (solo en local, no versionado) | Imports agrupados por DLL |

## Resumen en una página

- **Qué es**: herramienta de edición de disco y recuperación de datos, portable, para Windows x64
  (el mismo núcleo existe para DOS/Linux/macOS según textos internos).
- **Cómo está hecho**: Embarcadero Delphi (RTL `System.*`, FastMM, zlib 1.2.7 estático), UI Win32
  propia sin VCL, ~9 500 funciones de aplicación sin símbolos.
- **Qué hace**: acceso de bajo nivel a discos (SCSI/ATA pass-through, SMART, tolerancia a sectores
  defectuosos con script), gestión de particiones MBR/GPT, lectura y **reconstrucción por escaneo**
  de NTFS, FAT, exFAT, ExtFS, HFS+, APFS, ReFS y Btrfs, RAID virtual con cálculo automático,
  búsqueda RAW por firmas configurable, recuperación con informes y hashes, clonado/imagen
  compatible con ddrescue, editor hex con plantillas.
- **Mejor verificado**: motor NTFS (fixups, atributos, runs, índices, LZNT1, escaneo) y validadores
  de GPT, ExtFS y APFS; IOCTLs de Windows.

## Metodología

1. `open_binary` con proveedor Ghidra 12.1.4 (base de datos efímera, el binario no se escribe).
2. `binary_overview`, `inspect_pe_resources`, inventario de imports, funciones y cadenas.
3. Extracción de la tabla de mensajes de UI y agrupación de cadenas por dirección para mapear módulos.
4. Búsqueda de constantes conocidas (CRC-32, SHA-256, MD5, magias de FS, IOCTL) con `search_bytes`.
5. Referencias cruzadas → función contenedora → decompilación (`batch_decompile`).
6. Análisis por subsistema (NTFS completado; el resto incompleto y cubierto parcialmente por
   cadenas e interfaz).

Cada afirmación se etiqueta como **OBSERVADO** (en código/bytes/cadenas) o **INFERIDO**; las
evidencias REA (`ev_…`) permiten reproducirlas. Las evidencias con prefijo abreviado proceden del
análisis NTFS (los identificadores completos se conservan en el registro local de evidencias).

## Identidad del objetivo

`dmde.exe` · 5 390 224 bytes ·
SHA-256 `57a4febbb5046df1031e2e82aac48ba37181c9545b6335dbc601e5a4d447adb4`
