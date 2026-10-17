# Plan técnico de M2 · Particiones

Estado: aprobado por el propietario el 2026-10-18, tras el cierre de M1. Seguimiento en el milestone M2.

## Objetivo

Detectar las particiones de un origen (imagen, contenedor o rango) de forma fiable y tolerante, y abrir
cada una como dispositivo de bloques para M3 (NTFS) y M4 (recuperación). Paridad objetivo: PT-01, PT-02,
PT-03, PT-05, PT-06; PT-04 (APM) con prioridad baja; PT-07 a PT-10 quedan fuera de M2 (requieren la capa
de cambios diferidos de M12 o son secundarias).

## Orden y dependencias

| Paso | Issue | Contenido | Depende de | Paralelizable |
|---|---|---|---|---|
| 1 | #14 | Corpus de tablas dañadas y casos límite | - | No (base de todo) |
| 2 | (andamiaje) | Crate `mr-part` vacío con tipos comunes, integrado en el workspace | #14 | No (lo hace el integrador) |
| 3a | #12 | Parser MBR/EBR | 1, 2 | Sí, con 3b |
| 3b | #13 | Parser GPT con copia de respaldo y tolerancias | 1, 2 | Sí, con 3a |
| 4 | #16 | Investigación INV-05b: generación de candidatos | - | Sí, desde el paso 1 (solo documentos) |
| 5 | #15 | Búsqueda exprés de particiones perdidas e indicadores | #12, #13, #16 | No |
| 6 | #17 | Apple Partition Map | #12 | Opcional, prioridad baja |

## Diseño de `mr-part` (original, basado en especificaciones públicas e INV-05)

- Entrada: `Source` de `mr-api` (lee por sectores con mapa de validez). Un sector `Unreadable`, `Skipped`
  o de relleno **nunca** se interpreta como tabla: produce la anomalía `SectorIlegible` y la estructura
  afectada queda sin confirmar.
- Tipos:
  - `Scheme`: `None`, `Mbr`, `GptProtective`, `Hybrid`, `Apm`.
  - `Partition { index, origin (Primaria/Lógica/Gpt/Encontrada), start_lba, sector_count, kind
    (tipo MBR o GUID GPT), name, active, confidence, anomalies }`.
  - `Confidence` (ADR-003): `Valid`, `Suspicious`, `Invalid`.
  - `Anomaly`: firma ausente, varias activas, fuera del disco, solapada, ciclo EBR, límite de enlaces,
    CRC de cabecera o entradas, primaria dañada y respaldo usado, desplazamiento detectado, etc.
- Aritmética comprobada (`checked_*`) en todos los cálculos de LBA; límites explícitos (enlaces EBR,
  número y tamaño de entradas GPT, memoria máxima de la tabla de entradas).
- MBR/EBR: reglas de aceptación de INV-05 (§2.3), ambos modos de direccionamiento de enlaces, tipos 0x05,
  0x0F y 0x85, detección de ciclos y enlaces fuera del disco.
- GPT: firma, revisión, tamaño de cabecera, CRC-32 de cabecera y de entradas, `MyLBA`/`AlternateLBA`,
  `FirstUsable`/`LastUsable`, entradas dentro del rango utilizable; selección de copia (primaria, respaldo
  o ninguna) con informe; 512 y 4096 bytes; tolerancia a imagen recortada o desplazada (PT-03) calculando
  el desplazamiento a partir de la copia encontrada y comunicándolo, nunca aplicándolo en silencio.
- Cada partición se abre con `open_range` (ya existe, `RangeView`), así que M3 recibe un dispositivo
  normal con su mapa de validez.

## Integración

- `mr-api`: `partitions(source) -> PartitionTable` y tarea cancelable para la búsqueda exprés.
- FFI (ampliación compatible de la ABI 3): enumeración por índice con estructura versionada
  (`struct_size`), como en `mr_verify_options`.
- CLI: `partitions <imagen>` (tabla y anomalías) y `--partition N` para abrir una partición.
- WPF: lista de particiones con indicadores y confianza; abrir partición en el visor.

## Pruebas

- #14 (corpus, generador determinista con manifiesto y SHA-256; validador independiente en Python):
  MBR sin firma 0x55AA, dos activas, tipo 0x85, EBR en bucle, EBR fuera del disco, entradas solapadas,
  tipo desconocido, tabla borrada con sectores de arranque NTFS/FAT32/exFAT sintéticos y sus copias,
  GPT con primaria dañada, entradas con CRC incorrecto, cabecera de respaldo ausente, imagen recortada al
  inicio, GPT 4Kn, híbrido MBR+GPT y disco sin tabla.
- #12 y #13: unitarias por regla, corpus completo comparado con el manifiesto y el validador, fuzzing
  acotado con entradas aleatorias (sin pánicos, sin bucles), integración con contenedores (VHD/VHDX/VMDK
  del corpus convertido con qemu-img) y con sectores ilegibles simulados (`FaultyDevice`).
- Referencia independiente adicional (propuesta): `sgdisk`/`sfdisk` solo para validar el corpus, con la
  misma política que qemu-img (descarga fijada y verificada, imágenes desechables, nunca discos).
- #15: corpus con tablas borradas y FS intactos; progreso y cancelación; lectura tolerante.
- UI Automation: lista de particiones y apertura de una partición.

## Riesgos

- Sin formateadores reales hasta disponer de WSL o una VM: los boot sectors son sintéticos según la
  especificación (limitación documentada).
- INV-05b puede no cerrar el algoritmo de candidatos; se usará el diseño propio de INV-05 §5.3.
- Imágenes de varios TB: la búsqueda exprés debe limitarse a posiciones candidatas, nunca a un barrido.
