# 07 · Particiones y sistemas de archivos soportados

Resumen del grado de evidencia por formato. Los detalles algorítmicos verificados están en
[05-algoritmos-verificados.md](05-algoritmos-verificados.md).

| Formato | Detección / validación | Lectura (navegación) | Reconstrucción por escaneo | Escritura |
|---|---|---|---|---|
| MBR / EBR | Cadenas de tipos (`DOSEXT`, `FAT32x`, `Lin SWAP`…) y editor; validador no decompilado | UI de particiones | "Partition searching" | Sí (crear, borrar, firma, activa, código MBR): por UI |
| **GPT** | **OBS** `FUN_005ec9a0` (CRC-32, rangos, rebasado) | Visor de cabecera y entradas (etiquetas 0x75a6ab) | Sí (copia de respaldo `GPT Copy`) | Sí (GPT On/Off): por UI |
| Apple Partition Map | Cadenas `Apple_partition_map`, `Apple_HFS`, `Apple_Free` | - | - | - |
| **NTFS** | **OBS** `FUN_00625080` | **OBS** registros, atributos, runs, índices, LZNT1, ADS, EFS | **OBS** `[ntfsscan3.1]`, FILE/INDX | Herramientas NTFS (crear dir, copiar, borrar, reparar INDX): por UI |
| FAT12/16/32 | Etiquetas de boot sector 0x75979a; reglas en mensajes 1576–1607 | Visor de directorio FAT, tablas FAT | `[fatscan]`, validez de tablas FAT | Plantillas de boot (`FAT12BS.BIN`…) |
| exFAT | `EXFAT   ` 0x756fbb | Volumen exFAT | `[exfatscan]` | - |
| **ExtFS** (ext2/3/4) | **OBS** `FUN_006420e0` (0xEF53) | Inodos, extents, journal, `meta_bg`, 64-bit, codificaciones de nombre | `[extfsscan]`, tablas de inodos | - |
| HFS+/HFSX | Etiquetas 0x7ff5f0; ficheros especiales `HFSCatalogFile`, `HFSExtentsFile`… | Catálogo, resource fork (`:resfork`) | `[hfsscan]` | - |
| **APFS** | **OBS** `FUN_0079d5f0` (NXSB) | Omap, árbol raíz (etiquetas 0x81b428) | `[apfsscan]`, nodos | - |
| ReFS | Cadena `ReFS` en tablas y diálogos (0x80d27c); bandas, nodos | Vista ReFS (hasta v3.14, changelog) | `[refsscan]` | - |
| Btrfs | `FUN_00646610` → `_BHRfS_M` | Chunks, árboles root/fs, subvolúmenes | `[btrfsscan]` | - |
| ISO9660 | `CD001` (0x62e520) | INF | - | - |

## Comportamientos comunes deducidos

- **Doble vía**: cada FS tiene (a) un lector normal desde boot sector/superbloque y (b) un *escáner*
  con su propia sección en el log de escaneo (`[ntfsscan3.1]`, `[fatscan]`, `[exfatscan]`, `[extfsscan]`,
  `[hfsscan]`, `[refsscan]`, `[apfsscan]`, `[btrfsscan]`) que guarda fragmentos de metadatos
  encontrados y volúmenes candidatos para reconstruir después sin re-escanear.
- **FS virtual**: los resultados se presentan como un árbol virtual con carpetas sintéticas
  (`$Root`, `$FSFragments`, `$MetaData`, `$NonameFiles`, `$OrphanFS`, `$OtherVol`, `$Raw Files by Signatures`)
  y ficheros sin nombre (`$F_%IX`, `$Fx%08lx`).
- **Selección de la mejor versión de volumen**: la UI ordena candidatos por nº de ficheros, tamaño,
  calidad % y un indicador "+" (mensaje 3756).

## Pendiente de verificar en código

MBR/EBR, FAT (boot y escaneo), exFAT, HFS+, ReFS, Btrfs (interior), ISO9660 y Apple Partition Map
solo tienen evidencia por cadenas/UI. El análisis de código de este ámbito quedó pendiente.
