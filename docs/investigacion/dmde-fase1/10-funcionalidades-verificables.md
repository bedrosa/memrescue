# 10 · Qué funcionalidades se pueden verificar con el código decompilado

Respuesta al punto 5 de la fase 1. Tres niveles:

- **A · Verificada en código**: hay pseudocódigo decompilado o instrucciones concretas que implementan la lógica.
- **B · Localizada**: se conoce la función o rango que la implementa (constantes/xrefs), pero no se ha leído su lógica.
- **C · Solo declarada**: existe en la UI, cadenas o ficheros auxiliares; no se ha encontrado el código.

| Funcionalidad | Nivel | Evidencia principal |
|---|---|---|
| Validar boot sector NTFS | **A** | `FUN_00625080` |
| Aplicar/reparar fixups de registros FILE/INDX/RCRD | **A** | `FUN_00613770`, `FUN_006135a0` |
| Recorrer atributos NTFS y validar cabeceras | **A** | `FUN_006156d0`, `FUN_00615980`, `FUN_00615530` |
| Decodificar data runs NTFS (incl. sparse) | **A** | `FUN_00613d60` |
| Leer índices de directorio NTFS (INDX) y `$FILE_NAME` | **A** | `FUN_006133a0`, `FUN_006a2c60` |
| Descomprimir atributos NTFS (LZNT1) | **A** | `FUN_00613930`, `FUN_0061ae40` |
| Escanear disco buscando registros MFT/INDX y agrupar en volúmenes | **A** (bucle y aceptación) / **B** (puntuación) | `FUN_006aa3f0`, `FUN_006a9b60`, `FUN_006a8c50` |
| Lectura de páginas `$LogFile` | **A** (lectura) · replay **no encontrado** | `FUN_006168d0`, `FUN_00616a30` |
| Validar cabecera GPT (CRC-32, rebasado de LBA) | **A** | `FUN_005ec9a0` |
| Validar superbloque ExtFS | **A** | `FUN_006420e0` |
| Validar superbloque de contenedor APFS | **A** | `FUN_0079d5f0` |
| Hash MD5 | **A** | `FUN_00458720` |
| CRC-32 | **A** (tabla y uso) | `FUN_0045a370`, tabla 0x8a7e44 |
| Descompresión zlib | **A** | `FUN_005dbba0` |
| Bloquear/desmontar volumen, acceso DASD extendido | **A** (IOCTL concretos) | 0x4d08d0, 0x4d0cb0 |
| Obtener tamaño/geometría/partición de disco | **A** (IOCTL concretos) | 0x4d0f50, 0x4d9c60 |
| Comandos SCSI/ATA crudos (pass-through) | **A** (IOCTL) / **B** (CDB construidos) | 0x4d6b00, 0x4d7730, 0x4d7f20, 0x4d9870 |
| SMART ATA / consultas de almacenamiento | **A** (IOCTL) | 0x4d7b90, 0x4d7e00, 0x4d7360… |
| Btrfs (detección) | **B** | `FUN_00646610` |
| Checksum de objetos APFS | **B** | `FUN_0079c320` |
| SHA-256 | **B** (solo constantes) | 0x885744 |
| Contenedores VHD/VHDX | **B** | 0x4db850, 0x4dc080, 0x4dc660 |
| MBR/EBR, FAT, exFAT, HFS+, ReFS, ISO9660, APM | **C** | cadenas, UI |
| RAID virtual (mapeo, paridad) y cálculo automático | **C** | cadenas 0x6c1d64, 0x677594 |
| Búsqueda RAW por firmas y cálculo de tamaño | **C** | tabla 0x6d2167…, `rawsign.ini` |
| Recuperación de ficheros + informe con hashes | **C** | cadenas 0x833310… |
| Copia de sectores / imagen / mapfile ddrescue | **C** | cadenas 0x744fe0… |
| Editor hex, plantillas, script de E/S | **C** (palabras clave presentes) | 0x73030a, 0x4c3e80 |
| Escritura diferida con deshacer/rollback | **C** | mensajes 1209–1568 |
| Activación / licencias | fuera de alcance (no se analiza) | - |

## Conclusiones de la fase 1

1. **Núcleo NTFS**: es la parte mejor verificada y la más útil como referencia de comportamiento:
   validación tolerante, reparación de registros, agrupación de fragmentos en volúmenes.
2. **Filosofía de diseño** observada en todos los validadores: niveles de confianza + corrección de
   campos en modo no estricto, lo que permite usar el mismo parser para volúmenes sanos y dañados.
3. **Capa de E/S** de Windows verificada a nivel de IOCTLs: con eso se puede diseñar una capa propia
   equivalente sobre las mismas APIs públicas.
4. Las áreas de nivel C son las prioridades de la fase 2.
