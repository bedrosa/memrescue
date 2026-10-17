# 03 · Inventario de funciones y módulos

## Cifras globales (Ghidra 12.1.4, análisis automático por defecto)

| Métrica | Valor | Evidencia |
|---|---|---|
| Procedimientos totales | 15 552 | `ev_114e3cff…` (binary_overview) |
| – externos (imports) | 386 | `ev_c2872d04…` (list_procedures) |
| – thunks | 402 | ídem |
| – internos | 15 166 | ídem |
| Con nombre propio (no `FUN_`) | ~409, casi todos thunks de import + `entry`, `__dbk_fcall_wrapper`, `TMethodImplementationIntercept`, `HRESULT_FROM_WIN32`, `DelayLoad_*` | `ev_4e353785…` |
| Cadenas definidas | 6 675 | `ev_8cbaffc0…` (list_strings) |
| Mensajes de UI en inglés embebidos (`NNNN=texto`) | 1 549 | derivado de list_strings |

**No hay símbolos de depuración** ni RTTI de clases de la aplicación: todo el código propio aparece como
`FUN_xxxxxxxx`. Los nombres de módulo de la tabla siguiente son **etiquetas asignadas en este análisis**.

## Método para el mapa de módulos

El compilador Delphi coloca los literales de cadena dentro de `.text`, inmediatamente junto al código
que los usa. Por tanto, agrupar cadenas por dirección da una aproximación fiable de qué código vive
en cada rango. Las filas marcadas **(D)** tienen al menos una función decompilada que confirma el rango;
las demás se basan en la ubicación de cadenas (**INFERIDO**).

### Distribución de funciones por bloques de 256 KiB

```
0x400000 1647 ███████████████     RTL Delphi (System), thunks de import
0x440000 1464 █████████████       RTL/utilidades + hashes (MD5, CRC)
0x480000  895 ████████            shell UI, tabla de mensajes
0x4c0000  750 ███████             activación (constantes), E/S de dispositivos Windows
0x500000 1882 █████████████████   RTL: System.Rtti / Generics (instancias genéricas)
0x540000  294 ███
0x580000 2235 ████████████████████ RTL: System.Classes / Generics
0x5c0000  482 ████                zlib, núcleo de E/S, errores SCSI/ATA, SMART
0x600000  668 ██████              RAID/dumps, particiones, NTFS núcleo
0x640000  548 █████               FS núcleo (ext, btrfs), editor de particiones
0x680000  472 ████                escaneo NTFS
0x6c0000  537 █████               escaneo FAT, gestor de escaneo, RAW por firmas
0x700000  706 ██████              FS virtual, editor hex, plantillas, copia de sectores
0x740000  591 █████               imagen/clonado, licencias UI, visores de estructuras
0x780000  702 ██████              escaneos exFAT/Ext/HFS/ReFS/APFS/Btrfs
0x7c0000  441 ████                diálogos de parámetros por FS
0x800000  518 █████               ReFS/APFS/Btrfs (vistas), recuperación, informes
0x840000  334 ███                 activación en línea, red
```

Aproximadamente **5 000–5 500 funciones** (bloques 0x4dd000–0x5cc000 y parte de 0x401000–0x440000)
pertenecen al RTL de Delphi; el resto (~9 500) es lógica propia de la aplicación.

### Mapa de módulos por rango de direcciones

| Rango (aprox.) | Módulo | Anclas (cadenas / funciones) |
|---|---|---|
| 0x401000–0x41ffff | RTL `System` + gestor de memoria, thunks de import (0x405e70…, 0x415690–0x416ac0) | "An unexpected memory leak" 0x406246 |
| 0x41f000–0x426000 | RTL `System.SysUtils` (excepciones, formatos) | `EAccessViolation`… |
| 0x44b000–0x45b000 | Utilidades propias: memcmp `FUN_004516f0`, potencia de 2 `FUN_00451680`, **MD5** `FUN_00458720` (D), **CRC-32** `FUN_0045a370/0045a3d0` (D) | |
| 0x477000–0x48d000 | Thunks SHELL32/COMDLG32, diálogos | |
| 0x4a9000–0x4b6600 | Tabla de mensajes UI en inglés (idioma por defecto) | `1000=Exception %s…` |
| 0x4b99c4–0x4c4700 | Shell de aplicación: carga de `.lng`, comprobación admin, rutas `%appdata%`, atajos de teclado, palabras clave del script de E/S | "Language file \"%s\"…", `CONFIRM`/`DISKNUM`… 0x4c3e80 |
| 0x4cf000–0x4dc700 | **E/S de dispositivos Windows** (D): enumeración (SetupAPI, volúmenes), IOCTLs, SCSI/ATA pass-through, SMART, VHD/VHDX | ver doc 06 |
| 0x4dd000–0x5cc000 | RTL Delphi (`System.TimeSpan`, `SyncObjs`, `Generics`, `Rtti`, `Classes`) | |
| 0x5d4000–0x5dbc85 | **zlib 1.2.7** (D) | `FUN_005dbba0` |
| 0x5dcd00–0x5f8000 | Núcleo de E/S portable: códigos de error SCSI/ATA, SMART (atributos), NVMe, perfiles de medio óptico, `ioparams.ini` | "SCSI Error 0x%02lx" 0x5ddd47 |
| 0x5ec9a0 | **Validador cabecera GPT** (D) | `FUN_005ec9a0` |
| 0x5fd400–0x604100 | Dispositivos virtuales: RAID (`raidtype=`), volcados (`dump.dat`, `dumpchng.dat`), rollback | |
| 0x608b00–0x613300 | Tipos de partición, tabla de particiones, Apple Partition Map | "Apple_partition_map" |
| 0x613300–0x62a000 | **Núcleo NTFS** (D): fixups, índices, runs, LZNT1, atributos, `$LogFile`, validador de boot sector | `FUN_00613770`, `FUN_00625080` |
| 0x62e520 | ISO9660 (`CD001`) | |
| 0x63e000–0x647000 | Núcleo FAT32 / **ExtFS** (D `FUN_006420e0`) / **Btrfs** (`FUN_00646610`, ref. `_BHRfS_M`) | |
| 0x64c900–0x65c000 | Editor de particiones, escritura de boot sectors (`FAT12BS.BIN`, `MBR.BIN`) | |
| 0x665c00–0x67b800 | Coordinación de escaneo, cálculo automático de RAID (`[raidscan]`, `conf[%l]=`) | |
| 0x689a00–0x6b5b00 | **Escaneo NTFS** (D): bucle de escaneo `FUN_006aa3f0`, agrupación en volúmenes `FUN_006a8c50`, log `[ntfsscan3.1]` | |
| 0x6bb000–0x6bee00 | Escaneo FAT (`[fatscan]`) | |
| 0x6bfd00–0x6c7300 | Gestor de escaneo completo (`[devscan]`), tipos RAID (UI), `scanauto.log` | |
| 0x6c8d00–0x6e8000 | **Búsqueda RAW por firmas**: tabla integrada de tipos, `rawsign.ini`, `[rawscan]` | `%PDF-`, `GIF89a`… |
| 0x6ee800–0x72e000 | Sistema de archivos virtual / panel de ficheros (`$Root`, `$FSFragments`, `$NonameFiles`), exportación | |
| 0x72e000–0x739000 | **Editor hex** + **motor de plantillas** (`CALCSIZESTART`, `$OPENDEVOFS`…) | 0x73030a |
| 0x73bd00–0x74a000 | **Copia de sectores / imagen / clonado** (`[DevCopy]`, `cranges.log`, mapfile ddrescue) | |
| 0x74d700–0x755600 | UI de licencia, plantillas de informe (`%DEVMODEL%`, `%REPUUID%`), relleno de sectores | |
| 0x7566f0–0x765400 | Visores de estructuras: boot sectors, tabla de particiones, GPT, dir FAT, registro MFT, dir NTFS | |
| 0x76f800–0x775000 | Vistas del editor, vista previa (GDI+/Direct2D) | |
| 0x776400–0x77b300 | Escaneo exFAT | |
| 0x77b800–0x787000 | Escaneo ExtFS | |
| 0x78a000–0x790800 | Escaneo HFS+ | |
| 0x7997c0–0x79c100 | Escaneo ReFS | |
| 0x79c600–0x7a4e20 | **APFS** (D `FUN_0079d5f0`) + escaneo | |
| 0x7a4f80–0x7a8e00 | Escaneo Btrfs | |
| 0x7ac800–0x7b5500 | Coordinador de escaneo completo (persistencia del log) | |
| 0x7c8300–0x7cce00 | NTFS: creación de `$ATTRIBUTE_LIST`, `$LogFile` | |
| 0x7dca00–0x829000 | Diálogos de parámetros de volumen por FS (NTFS, FAT/exFAT, ExtFS, HFS, ReFS, APFS, Btrfs) | "Sector Size, Cluster Size:" |
| 0x82a700–0x832000 | QR, "Acerca de", lista HTML de ficheros | |
| 0x833300–0x840000 | **Motor de recuperación** + informes (CRC-32/MD5/SHA-1/SHA-256), EFS, `$Bad` | |
| 0x845400–0x851200 | **Activación en línea** (HTTP, proxy, `acode.txt`) | |
| 0x87bf50 | `entry` | |
| `.data` 0x87c000–0x8c0a00 | Tablas: SHA-256 K (0x885744), CRC-32 (0x8a7e44), punteros a cadenas (p. ej. 0x8aca60 → "NTFS    ", 0x8aa7b8 → "EFI PART") | |

## Funciones identificadas con nombre propuesto

| Dirección | Nombre propuesto | Verificación |
|---|---|---|
| 0x4516f0 | `mem_equal(a, b, n)` | uso con "NTFS    " y "EFI PART" |
| 0x451680 | `is_power_of_two(x)` | uso en validadores |
| 0x458720 | `md5_transform(state, block)` | decompilado |
| 0x45a370 / 0x45a3d0 | `crc32_update(crc, buf, len)` / `crc32(buf,len)` | uso en GPT + tabla |
| 0x5dbba0 | `zlib_uncompress(src, srclen, dst, dstlen) -> total_out` | decompilado |
| 0x5ec9a0 | `gpt_header_validate(hdr, lba, disk_sectors, sector_size, strict)` | decompilado |
| 0x613770 | `ntfs_apply_fixups_strict(rec, magic, size)` | análisis NTFS |
| 0x6135a0 | `ntfs_record_repair(rec) -> damage_mask` | análisis NTFS |
| 0x6134d0 | `ntfs_record_size(rec)` | análisis NTFS |
| 0x613930 | `lznt1_decompress` | análisis NTFS |
| 0x613d60 / 0x614170 | `ntfs_decode_runs` | análisis NTFS |
| 0x6133a0 | `ntfs_index_node_validate` | análisis NTFS |
| 0x615530 | `ntfs_attr_header_validate` | análisis NTFS |
| 0x6156d0 / 0x615980 | `ntfs_attr_first` / `ntfs_attr_next` | análisis NTFS |
| 0x615f00 | `ntfs_file_record_analyze` | análisis NTFS |
| 0x61ae40 | `ntfs_read_compressed_attr` | análisis NTFS |
| 0x6168d0 / 0x616a30 | `ntfs_logfile_page_check` / `…_read` | análisis NTFS |
| 0x625080 | `ntfs_boot_sector_validate(bs, sector_size, strict)` | decompilado |
| 0x625640 | `ntfs_estimate_volume_size` | análisis NTFS |
| 0x6420e0 | `ext_superblock_validate(sb)` | decompilado |
| 0x646610 | `btrfs_superblock_check` (referencia `_BHRfS_M`) | solo xref |
| 0x6a3870 | `ntfs_scan_accept_file_record` | análisis NTFS |
| 0x6a8c50 | `ntfs_scan_cluster_volumes` | análisis NTFS |
| 0x6a9b60 | `ntfs_scan_on_hit` | análisis NTFS |
| 0x6aa3f0 | `ntfs_scan_loop` | análisis NTFS |
| 0x79d5f0 | `apfs_nx_superblock_validate(blk, max_bs)` | decompilado |
| 0x4d08d0 / 0x4d0cb0 | `volume_allow_dasd` / `volume_lock_dismount` | IOCTL inmediato |
| 0x4d6b00, 0x4d7730, 0x4d9870 | rutas SCSI pass-through directo | IOCTL inmediato |
| 0x4d7f20 | ruta ATA pass-through directo | IOCTL inmediato |
| 0x4d7b90 | lectura SMART (ATA) | IOCTL inmediato |
| 0x4d7360, 0x4d84e0, 0x4d8b60 | consultas `IOCTL_STORAGE_QUERY_PROPERTY` | IOCTL inmediato |
| 0x4d9c60 | apertura de disco + geometría + tamaño | IOCTL inmediato |
