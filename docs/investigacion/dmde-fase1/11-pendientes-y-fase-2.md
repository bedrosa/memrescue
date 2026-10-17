# 11 · Pendientes y propuesta de fase 2

## Preguntas abiertas (checklist de la fase 1)

| Pregunta del encargo | Estado |
|---|---|
| 1. Abrir el ejecutable y análisis inicial | **Completado** |
| 2. Arquitectura, dependencias, tecnologías | **Completado** (versión exacta de Delphi sin determinar) |
| 3. Inventario de funciones y módulos | **Completado** (módulos por rango = aproximación basada en cadenas) |
| 4. Algoritmos y componentes principales | **Parcial**: NTFS, GPT, ExtFS, APFS, hashes, IOCTLs verificados; RAID, RAW, FAT/exFAT/HFS/ReFS/Btrfs, recuperación y copia solo por cadenas |
| 5. Funcionalidades verificables | **Completado** (ver doc 10) |

## Huecos concretos

1. Código de RAID: mapeo LBA virtual → (disco, LBA) por variante, reconstrucción de paridad, métrica del cálculo automático.
2. RAW: estructura en memoria de una firma, bucle de búsqueda, cálculo de tamaño por formato (JPEG, ZIP, OLE, MP4, PDF…).
3. Parsers de FAT, exFAT, HFS+, ReFS, Btrfs, MBR/EBR, ISO9660 y APM.
4. Bucle de lectura con reintentos/saltos y construcción de CDB SCSI / comandos ATA.
5. NTFS: puntuación de volúmenes candidatos (`FUN_006a6d80`, `FUN_006a89b0`), uso de `$ATTRIBUTE_LIST`, tabla de nombres de sistema (`DAT_008ac1bc`).
6. Motor de recuperación (escritura de ficheros, renombrado, división) e intérpretes del script de E/S y de plantillas.
7. Punto de entrada y estructura de la UI (confirmar ausencia de VCL).

## Plan de fase 2 (sugerido)

1. Completar los análisis pendientes (RAID+RAW, FS no NTFS, E/S, arranque/scripts), uno por ámbito,
   con informes acotados.
2. Para cada FS, documentar sus estructuras según la **especificación pública** y anotar solo las
   heurísticas propias observadas en DMDE (tolerancias, reparaciones, puntuación).
3. Opcional: observación en tiempo de ejecución con una imagen de prueba (no un disco real) para
   confirmar el flujo de lectura y los logs generados.

## Pautas para una implementación original

- Basar los formatos en especificaciones públicas (Microsoft NTFS/GPT/exFAT, documentación de ext4,
  Apple File System Reference, documentación de Btrfs, UEFI) y no en el pseudocódigo.
- Reutilizar del análisis **ideas de comportamiento**, no código ni estructuras internas:
  validadores con niveles de confianza, reparación tolerante de registros, escaneo grueso→fino,
  agrupación de fragmentos por parámetros de volumen, FS virtual con carpetas sintéticas,
  logs de escaneo reanudables, filtrado de RAW con el mapa de clusters.
- No reproducir textos de la interfaz, plantillas, firmas ni ficheros auxiliares de DMDE literalmente.
- No replicar ni estudiar el sistema de licencias.
- Para interoperar, son formatos abiertos útiles: mapfile de ddrescue, VHD/VHDX, imágenes RAW.
