# 01 · Identificación y arquitectura del binario

## Identidad del artefacto

| Campo | Valor | Fuente |
|---|---|---|
| Fichero | `dmde.exe` (copia local de referencia, fuera del repositorio) | sistema de archivos |
| Tamaño | 5 390 224 bytes | sistema de archivos |
| SHA-256 | `57a4febbb5046df1031e2e82aac48ba37181c9545b6335dbc601e5a4d447adb4` | `Get-FileHash` + REA `open_binary` |
| Producto | DMDE (DM Disk Editor and Data Recovery Software) | VERSIONINFO |
| Versión | 4.4.4.842 (cadena interna `DMDE 4.4.4 (2026)`, `bld=842`) | VERSIONINFO, cadenas 0x4be6bd, 0x4c26a5 |
| Copyright | Dmitry Sidorov 2005-2026 | VERSIONINFO, cadena 0x82e5bc |
| Formato | PE32+ (máquina 0x8664, x86-64), ejecutable GUI | `inspect_pe_resources` (`machine: 34404`), `open_binary` |
| Base de imagen | `0x400000` | `binary_overview` |
| Punto de entrada | `0x87bf50` (`entry`) | listado de procedimientos |

El ejecutable original **no se ha modificado**: Ghidra trabaja sobre una base de datos efímera
(`import_mode: ephemeral-source-immutable`).

## Secciones (Ghidra, extremo final exclusivo)

| Sección | Inicio | Fin | Tamaño | Comentario |
|---|---|---|---|---|
| Headers | 0x400000 | 0x400400 | 1 KiB | |
| `.text` | 0x401000 | 0x87c000 | 4,48 MiB | Código **y** literales de cadena (típico del compilador Delphi) |
| `.data` | 0x87c000 | 0x8c0a00 | 274 KiB | Tablas: CRC-32 (0x8a7e44), constantes SHA-256 (0x885744), punteros a cadenas |
| `.bss` | 0x8c1000 | 0xaef958 | 2,2 MiB | Datos no inicializados |
| `.idata` | 0xaf0000 | 0xaf3600 | 13,5 KiB | Imports estáticos |
| `.didata` | 0xaf4000 | 0xaf4400 | 1 KiB | Imports *delay-load* (Delphi `delayed`) |
| `.edata` | 0xaf5000 | 0xaf5200 | 512 B | Exports (tabla mínima) |
| `.tls` | 0xaf6000 | 0xaf61b8 | 440 B | TLS (threadvars Delphi) |
| `.rdata` | 0xaf7000 | 0xaf7200 | 512 B | |
| `.pdata` | 0xaf8000 | 0xb23400 | 173 KiB | Tabla de excepciones x64 |
| `.rsrc` | 0xb24000 | 0xb53e00 | 191 KiB | Recursos |

La combinación `.didata` + `.tls` + `.pdata` y el orden de secciones es la disposición
habitual del enlazador de Embarcadero (Delphi/C++Builder) para Win64.

Evidencia: `ev_114e3cff4df3bb8f98e5e734131ca2597b74951612834f737c0d96cdf41caa0b` (binary_overview).

## Toolchain (compilador / runtime)

**OBSERVADO**
- Recursos RCDATA `DVCLAL` y `PACKAGEINFO` (índices 44 y 45 de `inspect_pe_resources`):
  firmas exclusivas de binarios generados por Embarcadero Delphi / C++Builder.
  Evidencia `ev_0a91b9779d4c9584bcc05c11bd31c2daa768d6e13110383b75c4ce6e445bd8c1`.
- Información RTTI con nombres de unidades del RTL de Delphi: `System`, `System.SysUtils`,
  `System.SyncObjs`, `System.Generics.Defaults`, `System.Generics.Collections`, `System.Rtti`,
  `System.Classes` (TStream, TFileStream, TReader/TWriter, TComponent…).
- Claves de registro del RTL: `Software\Embarcadero\Locales`, `Software\CodeGear\Locales`,
  `Software\Borland\Locales` (0x411518…).
- Función `__dbk_fcall_wrapper` (0x4140e0) y `TMethodImplementationIntercept` (0x518370): símbolos del RTL de Delphi.
- Mensajes del gestor de memoria FastMM integrado en Delphi ("An unexpected memory leak has occurred", 0x406246).

**INFERIDO**
- La versión de Delphi es moderna (≥ XE2 por ser Win64; `System.Rtti` con `TMethodImplementation`
  y `TRttiInstanceMethodEx` apunta a 10.x/11.x/12.x). No se ha determinado la versión exacta.
- No hay RTTI de formularios VCL (`TForm`, `TButton`, `Vcl.*` no aparecen en cadenas). La UI parece
  construida directamente sobre Win32 (`CreateWindowExW`, `RegisterClassExW`,
  `CreateDialogIndirectParamW`, `LoadMenuIndirectW`, `DefMDIChildProcW`), lo que es coherente con
  un código base portable (ver más abajo). Pendiente de confirmar en el análisis de arranque.
- Las cadenas de formato del núcleo usan especificadores propios (`%l`, `%I`, `%~I`, `%IV`, `%W`,
  `%"127s`), es decir, el programa tiene su propio motor de formateo independiente del RTL.

## Indicadores de código base multiplataforma

**OBSERVADO** en cadenas y ficheros auxiliares (no en código ejecutable de Windows):
- `deviohsc.txt` (script de E/S distribuido con el programa) enumera servicios de E/S:
  `0-ATA 1-ATAPI 3-BIOSINT13OLD 4-BIOSINT13 5-DOSINT25 6-DOSINT73 8-DOSMSCDEX 9-DOSFILE 11-WINFILE 12-WINSCSI 14-WIN9XINT13 20-LINUXFILE`.
- Mensajes de UI que solo tienen sentido en DOS/Win9x: `1006=To get access reload computer in MS-DOS mode`,
  `1730=Long names are not supported under DOS…`, menús `DOS ASPI`, `E&xt BIOS Service`, `&MSCDEX Services` (0x4a9870…).
- `changelog.txt`: "high CPU load in GUI versions for Linux and macOS".

**INFERIDO**: el mismo núcleo se compila para DOS, Windows, Linux y macOS; en esta compilación
Win64 quedan restos de textos de otras plataformas. Las rutas de código DOS/BIOS no se han
buscado en el binario; probablemente están excluidas en compilación.

## Arquitectura lógica (vista de capas)

Reconstrucción basada en cadenas agrupadas por dirección + funciones decompiladas
(ver [03-inventario-funciones-y-modulos.md](03-inventario-funciones-y-modulos.md)):

```
┌───────────────────────────────────────────────────────────────┐
│ UI Win32 propia: selector de disco, panel de particiones,     │
│ árbol de directorios/panel de archivos, editor hexadecimal    │
│ con plantillas, diálogos de escaneo/recuperación/RAID         │
├───────────────────────────────────────────────────────────────┤
│ Servicios: recuperación de archivos + informes (CRC/MD5/SHA), │
│ copia/imagen de sectores (log, mapfile ddrescue), particiones,│
│ búsqueda RAW por firmas, licencias/activación                 │
├───────────────────────────────────────────────────────────────┤
│ Motores de FS (lectura + reconstrucción por escaneo):         │
│ NTFS, FAT12/16/32, exFAT, ExtFS, HFS+/HFSX, APFS, ReFS, Btrfs │
│ Esquemas de partición: MBR/EBR, GPT, Apple Partition Map      │
├───────────────────────────────────────────────────────────────┤
│ Dispositivos virtuales: RAID 0/1/4/5/6/JBOD, VHD diferencial, │
│ imágenes (IMG/VHD/VHDX/VMDK/E01), cambios diferidos/rollback  │
├───────────────────────────────────────────────────────────────┤
│ Capa de E/S: \\.\PhysicalDrive%l, volúmenes, SCSI/ATA         │
│ pass-through, SMART/NVMe, reintentos/saltos en sectores       │
│ dañados, script de manejo de errores                          │
└───────────────────────────────────────────────────────────────┘
```

La capa de E/S, los motores de FS y RAID se detallan en documentos posteriores con su
grado de verificación.
