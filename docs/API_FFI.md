# Interfaz entre el motor (Rust) y la interfaz (C#): ABI 3

Fuente de verdad: [core/mr-ffi/include/memrescue_core.h](../core/mr-ffi/include/memrescue_core.h).
Implementación: [core/mr-ffi/src/lib.rs](../core/mr-ffi/src/lib.rs).
Cliente .NET: [app/src/MemRescue.Core.Interop/](../app/src/MemRescue.Core.Interop/).

## Arquitectura

```
MemRescue.exe (WPF, C#)
   └─ MemRescue.Core.Interop.dll  ── P/Invoke ([LibraryImport]) ──▶  memrescue_core.dll (Rust, cdylib)
                                                                       └─ mr-api → mr-image / mr-io / mr-store / mr-common
memrescue-cli.exe (Rust) ──────────────── llamada directa ──────────▶  mr-api
```

- La DLL es la **única** dependencia binaria de la GUI con el motor. La GUI no conoce tipos Rust.
- La CLI usa `mr-api` directamente (sin FFI); las pruebas comprueban que ambas vías dan los mismos resultados.
- Versionado: `mr_abi_version()` devuelve 3 (ABI 1 en M0, ABI 2 en M1-iteración 1). La GUI lo comprueba al arrancar (`Engine.EnsureCompatible`) y se
  niega a funcionar con una DLL incompatible. Cualquier cambio incompatible en firmas, estructuras o
  significados incrementa la versión de ABI; añadir funciones nuevas no.

## Convenciones

| Aspecto | Regla |
|---|---|
| Convención de llamada | `extern "C"` (Win64: la única existente en x64) |
| Retorno | `int32_t mr_status`: 0 = `MR_OK`; ver tabla de códigos |
| Cadenas de entrada | UTF-8, puntero + longitud en bytes; no requieren terminador. Puntero nulo solo con longitud 0 |
| Cadenas de salida | El llamador aporta `(buf, cap, out_len)`. Si cabe, el motor escribe UTF-8 + `\0` y `*out_len` = longitud sin `\0`. Si no, `MR_ERR_BUFFER_TOO_SMALL` y `*out_len` = longitud necesaria (el cliente reintenta con `out_len + 1`) |
| Estructuras | `#[repr(C)]`, campos de ancho fijo, relleno explícito (`reserved`). Tamaños comprobados por pruebas en ambos lados: `mr_image_info` = 32 B, `mr_task_status` = 24 B, `mr_verify_report` = 72 B, `mr_verify_options` = 48 B, `mr_read_options` = 32 B, `mr_partition_table_info` = 80 B, `mr_partition_info` = 96 B, `mr_partition_anomaly` = 40 B |
| Enteros | `uint64_t` para tamaños y LBA; `uint32_t` para enumeraciones y máscaras |
| Booleanos | No se usan en la ABI (evita discrepancias de tamaño) |

## Propiedad de la memoria

1. **El motor nunca devuelve memoria que el llamador deba liberar.** Toda salida se escribe en búferes o
   estructuras del llamador. No existe `mr_free`.
2. Los punteros de entrada solo se leen durante la llamada; el motor no los conserva.
3. Las **tareas** son recursos del motor identificados por un handle `uint64_t` opaco (nunca 0). El
   handle se guarda en un registro interno, **no es un puntero**: un handle inválido, liberado o
   inventado devuelve `MR_ERR_INVALID_HANDLE` en lugar de corromper memoria.
4. El cliente libera cada tarea con `mr_task_free` (en C#, `HashTask.Dispose`, idempotente). Liberar una
   tarea en curso la cancela; su hilo termina por sí solo.

## Errores

| Código | Nombre | Significado |
|---|---|---|
| 0 | `MR_OK` | Correcto |
| 1 | `MR_ERR_INVALID_ARGUMENT` | Puntero nulo, UTF-8 inválido, máscara o algoritmo no válido |
| 2 | `MR_ERR_IO` | Error de E/S (fichero inexistente, tamaño de sector no válido, lectura) |
| 3 | `MR_ERR_PATH_REJECTED` | Ruta de dispositivo (`\\.\…`, `\\?\GLOBALROOT…`) rechazada: acceso a discos deshabilitado |
| 4 | `MR_ERR_STORE` | Error de la base de datos de trabajo SQLite |
| 5 | `MR_ERR_CANCELLED` | La tarea se canceló |
| 6 | `MR_ERR_NOT_FINISHED` | Se pidió el resultado de una tarea en curso |
| 7 | `MR_ERR_INVALID_HANDLE` | Handle de tarea desconocido o ya liberado |
| 8 | `MR_ERR_BUFFER_TOO_SMALL` | Búfer de salida insuficiente (`*out_len` indica lo necesario) |
| 9 | `MR_ERR_TASK_FAILED` | La tarea terminó con error (mensaje en `mr_last_error`) |
| 10 | `MR_ERR_UNSUPPORTED_FORMAT` | Contenedor detectado sin lector (EWF, QCOW, VDI, DMG); se puede forzar RAW |
| 11 | `MR_ERR_OUT_OF_RANGE` | LBA, recuento o rango fuera del origen; petición mayor de 64 MiB |
| 12 | `MR_ERR_CORRUPT` | Estructura del contenedor dañada o incoherente (cabeceras, sumas, tablas fuera del fichero…) |
| 13 | `MR_ERR_EXTERNAL_REFERENCE` | El contenedor necesita otro fichero (padre, extensión, descriptor); **nunca** se abre automáticamente |
| 14 | `MR_ERR_UNSUPPORTED_FEATURE` | Variante del formato aún no soportada (VMDK streamOptimized, VHDX con registro pendiente…) |
| 99 | `MR_ERR_PANIC` | Error interno capturado; el proceso **no** se cae |

- `mr_last_error` devuelve el mensaje del **último error del hilo que llama** (almacenamiento local de
  hilo). El cliente debe leerlo inmediatamente después del fallo y en el mismo hilo; `Engine.Check` en
  C# lo hace de forma síncrona antes de cualquier `await`.
- Cada función exportada envuelve su cuerpo en `catch_unwind`: un pánico de Rust nunca cruza la frontera.

## Operaciones asíncronas

Modelo **sondeo + cancelación cooperativa**, sin callbacks (evita que hilos de Rust entren en código .NET
y problemas de *marshalling* de delegados):

```
mr_hash_start(...) → handle
loop: mr_task_status_get(handle) → {state, done, total}     // la GUI sondea cada ~100 ms
      [opcional] mr_task_cancel(handle)                     // marca de cancelación
state ≠ RUNNING → mr_hash_result(handle, alg) / mr_last_error
mr_task_free(handle)
```

- Estados: 1 en curso, 2 completada, 3 fallida, 4 cancelada.
- En C#, `HashTask.WaitAsync(IProgress<TaskProgress>, CancellationToken)` encapsula el bucle: el token
  solicita la cancelación al motor y espera a que el motor la confirme (no abandona la tarea).
- Persistencia opcional: si se pasa una ruta de base de datos de trabajo, el motor registra el trabajo
  (fuente, estado, progreso, resultado) en SQLite. Ver [ARQUITECTURA.md](ARQUITECTURA.md) §4.

## Funciones (ABI 3)

| Función | Descripción |
|---|---|
| `mr_abi_version()` | Versión de la ABI (3) |
| `mr_engine_version(buf, cap, out_len)` | Versión del motor |
| `mr_last_error(buf, cap, out_len)` | Mensaje del último error del hilo |
| `mr_image_probe(path, len, out_format)` | Formato detectado por firmas: 1 RAW, 2 VHD, 3 VHDX, 4 VMDK, 5 VMDK descriptor, 6 EWF, 7 QCOW, 8 VDI, 9 DMG |
| `mr_image_info_get(path, len, sector_size, out)` | Tamaño, sectores, bytes sobrantes y formato (abre el contenedor si hay lector) |
| `mr_source_open(path, len, sector_size, flags, out_source)` | Abre un origen en solo lectura. `sector_size` 0 = nativo del formato (512 en RAW); si difiere del nativo, mismos bytes con otra división en sectores. `flags`: 1 = forzar RAW, 2 = imagen dividida, 4 = modo tolerante (`MR_OPEN_TOLERANT`, ver abajo); otros bits → `MR_ERR_INVALID_ARGUMENT` |
| `mr_source_info(source, out)` | Datos del origen (`format` = formato con el que se interpreta) |
| `mr_source_describe(source, buf, cap, out_len)` | Descripción legible: variante del contenedor, segmentos, rango |
| `mr_source_read(source, lba, count, buf, cap, out_len)` | Lectura **estricta** de `count` sectores (máx. 64 MiB); falla ante cualquier error |
| `mr_source_read_mapped(source, lba, count, retries, buf, cap, states, states_cap, out_len)` | Lectura **tolerante** con un byte de estado por sector (0 leído, 1 recuperado, 2 ilegible, 3 saltado, 4 disperso). Devuelve `MR_OK` aunque haya ilegibles: el llamador **debe** consultar `states` |
| `mr_source_read_mapped_ex(source, lba, count, options, buf, cap, states, states_cap, out_len)` | Igual que la anterior con `mr_read_options` (32 bytes; `struct_size` = `sizeof`, otro tamaño se rechaza; `reserved` = 0): `retries` (0–100), `jump_sectors` (saltados tras cada ilegible), `flags` 1 = `MR_READ_READ_BACK` y los **patrones de relleno** `bad_fill[4]` / `skip_fill[4]`. Patrones con los cuatro bytes iguales (`00 00 00 00` incluido) o iguales entre sí → `MR_ERR_INVALID_ARGUMENT` (ADR-021). `mr_source_read_mapped` no cambia |
| `mr_source_open_range(source, start_lba, count, out_source)` | Rango de sectores como origen independiente (vista acotada; anidable; sobrevive al cierre del padre) |
| `mr_split_discover(path, len, buf, cap, out_len, out_count)` | Lista los segmentos de una imagen dividida (uno por línea) **sin abrirlos** |
| `mr_source_close(source)` | Cierra el origen (las tareas en curso conservan su referencia) |
| `mr_hash_start(path, …)` / `mr_source_hash_start(source, mask, store, store_len, out_task)` | Hashes (CRC-32=1, MD5=2, SHA-1=4, SHA-256=8); estricto ante errores |
| `mr_hash_result(task, alg, buf, cap, out_len)` | Resumen hexadecimal de un algoritmo |
| `mr_source_verify_start(source, start_lba, count, retries, jump_sectors, flags, out_task)` | Lectura completa tolerante (`count` 0 = hasta el final; reintentos 0–100; `jump_sectors` tras cada ilegible; `flags` 1 = lectura hacia atrás) |
| `mr_source_verify_start_ex(source, options, out_task)` | Igual que la anterior con `mr_verify_options` (`struct_size` = `sizeof`, otro tamaño se rechaza), que añade la **política de omisión de errores**: `skip_codes` (≤ 256 códigos del sistema positivos, p. ej. 23, 1117) que no se reintentan y `flags` 2 = `MR_VERIFY_IGNORE_ALL_ERRORS` (ningún error se reintenta). Los sectores afectados quedan ilegibles (estado 2, relleno), nunca como datos. `mr_source_verify_start` no cambia |
| `mr_verify_error_summary(task, out_omitted, out_code_count)` | Sectores cuyo error se omitió por la política y nº de códigos del sistema distintos registrados |
| `mr_verify_error_code(task, index, out_code, out_sectors)` | Código nº `index` (orden creciente) y nº de sectores ilegibles cuyo último intento falló con él |
| `mr_verify_result(task, out)` | Informe con el recuento exacto de cada estado, relecturas, nº de tramos no fiables y truncado |
| `mr_verify_untrusted_range(task, index, out_lba, out_count, out_state)` | Tramo no fiable nº `index` (2 ilegible, 3 saltado). Sustituye a `mr_verify_bad_range` (ABI 2). Coste O(1) por índice: los tramos no fiables y los códigos de error se materializan una sola vez, en la primera consulta del resultado, y se guardan con él, así que recorrer todos los índices es lineal y el bloqueo global del registro de handles se mantiene un tiempo constante por llamada |
| `mr_task_status_get(task, out)` | Estado y progreso (bytes en hash, sectores en verificación) |
| `mr_task_cancel(task)` | Solicita la cancelación |
| `mr_task_free(task)` | Libera el handle (cancela si sigue en marcha) |

Códigos añadidos en ABI 2: 10 y 11; en ABI 3: 12, 13 y 14. Las tres funciones de la política
de errores (IO-09) y `mr_source_read_mapped_ex` con `mr_read_options` (IO-08) se añadieron sin cambiar la
versión: no modifican firmas ni estructuras existentes. Límites:
cadenas de entrada ≤ 64 KiB (`MR_MAX_STRING_BYTES`), lecturas ≤ 64 MiB (`MR_MAX_READ_BYTES`).

## Tablas de particiones (ampliación compatible de la ABI 3, issue #12, ADR-022)

Funciones nuevas; ninguna firma, estructura ni significado existente cambia, así que la versión sigue
siendo 3. La tabla se lee una vez y se consulta **por índice** a través de un handle propio (del mismo
registro que orígenes y tareas). Las estructuras de salida son **versionadas**: el llamador escribe
`struct_size = sizeof(...)` de su cabecera antes de llamar; otro valor se rechaza con
`MR_ERR_INVALID_ARGUMENT` (mismo esquema que `mr_verify_options`, aplicado a una salida).

| Función | Descripción |
|---|---|
| `mr_source_partitions(source, out_table)` | Lee el MBR y su cadena de EBR y, con MBR protector o híbrido (o sin MBR utilizable pero con firma GPT), la GPT con su copia de respaldo (ADR-022 y adenda); devuelve el handle de la tabla. Lectura tolerante: un sector de tabla ilegible es la anomalía `MR_ANOMALY_UNREADABLE_SECTOR`, nunca un error ni datos |
| `mr_partitions_info(table, out)` | `mr_partition_table_info`: `scheme` (`MR_SCHEME_NONE` 0, `MBR` 1, `GPT` 2, `HYBRID` 3, `APM` 4), `sector_size` (unidades de las LBA), `confidence` del conjunto (`MR_CONFIDENCE_VALID` 1, `VALID_WITH_FIXES` 2, `SUSPICIOUS` 3, `INVALID` 4), nº de particiones y de anomalías de la tabla; GPT: `gpt_selected` (`MR_GPT_NONE` 0, `PRIMARY` 1, `BACKUP` 2 = copia de respaldo en uso), `gpt_flags` (`MR_GPT_PRESENT`), `shift` (con signo: LBA disco = LBA imagen + shift), `trailing_sectors_missing`, `gpt_sector_size` y `disk_guid` |
| `mr_partition_get(table, index, out)` | `mr_partition_info`: `origin` (`MR_ORIGIN_PRIMARY` 1, `LOGICAL` 2, `GPT` 3, `APM` 4), `slot` (en APM, número de entrada desde 1), `mbr_type`, `ebr_lba`, `start_lba`, `sector_count`, `active`, `confidence`, nº de anomalías propias `gpt_type`, `unique_guid` (GUID en el orden del disco; ceros en MBR y APM) y `attributes` (en APM, `pmPartStatus`). Orden: primarias y lógicas en orden de cadena (en un híbrido, las entradas MBR distintas de 0xEE) y después las GPT, que llevan LBA **del disco** |
| `mr_partition_name(table, index, buf, cap, out_len)` | Nombre GPT o APM (cadena vacía en MBR) |
| `mr_partition_apm_type(table, index, buf, cap, out_len)` | Tipo APM tal como está en el mapa (`pmParType`, p. ej. `Apple_HFS`); cadena vacía si la partición no es APM (#17) |
| `mr_partition_type_name(table, index, buf, cap, out_len)` | Nombre legible del tipo MBR o del GUID de tipo GPT (lista propia de identificadores públicos; cadena vacía si es desconocido) |
| `mr_partition_anomaly(table, partition_index, anomaly_index, out)` | `mr_partition_anomaly`: `code` (`MR_ANOMALY_*`, numérico estable), `severity` (`MR_SEVERITY_NOTICE` 1, `SUSPICIOUS` 2, `INVALID` 3, `COMPENSATED` 4 = daño compensado por la otra copia GPT), `lba` (posición en la imagen), `target_lba` y `value` con `flags` que indican si tienen significado (`MR_ANOMALY_VALUE_SIGNED`: `value` es `int64_t`, desplazamiento de `image_shifted`). `partition_index = MR_PARTITION_TABLE` (0xFFFFFFFF) para las anomalías de la tabla |
| `mr_partition_anomaly_text(table, partition_index, anomaly_index, buf, cap, out_len)` | `"<código estable>\n<descripción en español>"` (p. ej. `ebr_loop`) |
| `mr_partition_open(table, index, out_source)` | La partición como origen independiente (`RangeView` sobre el origen de la tabla). Si termina fuera del origen se abre la parte existente y `mr_source_describe` dice «recortada»; si empieza fuera, si la tabla usa otro tamaño de sector que el origen o si es una partición GPT de una imagen desplazada, `MR_ERR_INVALID_ARGUMENT`. Se cierra con `mr_source_close` |
| `mr_partition_open_ex(table, index, flags, out_source)` | Igual; con `MR_PARTITION_OPEN_APPLY_SHIFT` una partición GPT de una imagen desplazada se abre en `LBA disco - shift` y `mr_source_describe` dice «desplazamiento aplicado». El desplazamiento nunca se aplica en silencio |
| `mr_partitions_free(table)` | Libera la tabla; los orígenes abiertos desde ella siguen siendo válidos y la tabla sobrevive al cierre de su origen |

Índices fuera de límites → `MR_ERR_OUT_OF_RANGE`; handle que no es una tabla → `MR_ERR_INVALID_HANDLE`
(y una tabla no es un origen ni una tarea). Códigos de anomalía: 1 sector ilegible; 100–121 MBR/EBR
(lista en la cabecera; los códigos textuales coinciden con los del manifiesto del corpus cuando existe
equivalente; 122 `hybrid_mbr_mismatch`); 201-241 GPT (las de una copia: impar la primaria, par la de respaldo;
`image_shifted` = 232); 400-414 Apple Partition Map (#17, `formatos/apm.md`; `lba` = sector del origen del bloque afectado, `value` = número de entrada salvo en 401-407 y 414). Con `MR_SCHEME_APM` las LBA están en bloques del mapa (`sector_size`); si difiere del sector del origen, abrir una partición se rechaza. En C#: `Source.ReadPartitions()` devuelve
`PartitionTable` (`EngineHandle`: listas materializadas de `PartitionEntry` y `PartitionAnomaly`, `HasGpt`,
`GptSelected`, `Shift`, `DiskGuid`, `OpenPartition(index, applyShift)`; `PartitionScheme.Apm`, `PartitionOrigin.Apm` y `PartitionEntry.ApmType`).

## Búsqueda exprés de particiones perdidas (ampliación compatible de la ABI 3, issue #15, ADR-023)

Funciones nuevas; ninguna firma, estructura ni significado existente cambia. La búsqueda es una
**tarea** (mismo registro y mismas funciones comunes: `mr_task_status_get` con progreso = lecturas
hechas / `max_reads`, `mr_task_cancel`, `mr_task_free`) y su resultado se consulta **por índice** con
estructuras de salida versionadas por `struct_size`. Al cancelarla, la tarea termina en estado
cancelado (4) **con** resultado parcial (`status = MR_SEARCH_CANCELLED`); las demás tareas no cambian.
Detalle del algoritmo, indicadores y confianza en `formatos/busqueda-particiones.md`.

| Función | Descripción |
|---|---|
| `mr_source_partition_search_start(source, options, out_task)` | Lee la tabla del origen y lanza la búsqueda. `options` (`mr_partition_search_options`, 64 B, versionada) puede ser `NULL`; cada campo a 0 toma su valor por defecto (`max_reads` 4096, `max_bytes` 32 MiB, `start_grid_bytes` 64 MiB, `end_grid_bytes` 16 MiB, `max_volumes` 256, `probe_window` 8192, `cylinders` 64, `probes_per_gap` 8, `families` = todas `MR_SEARCH_FAMILY_*`); `flags` y `reserved` deben ser 0; valores fuera de límites → `MR_ERR_INVALID_ARGUMENT` antes de lanzar nada |
| `mr_partition_search_result(task, out)` | `mr_partition_search_info` (96 B): `status` (`MR_SEARCH_COMPLETE` 1, `BUDGET_EXHAUSTED` 2, `CANCELLED` 3), recuentos de volúmenes, tablas encontradas y anomalías de la búsqueda, `flags` (`MR_SEARCH_HAS_SHIFT`, `MR_SEARCH_VOLUME_LIMIT`), `shift_bytes` (con signo), lecturas, `max_reads`, bytes, sectores no verificables, candidatos sondeados y pendientes, sector y sectores del origen |
| `mr_found_volume_get(task, index, out)` | `mr_found_volume` (136 B): `fs` (`MR_FS_*`), `start_byte` y `start_lba` con signo, longitud, sectores, sector del FS, `confidence`, los seis indicadores (`MR_CHECK_YES` 1, `NO` 2, `UNVERIFIABLE` 3, `OUTSIDE` 4, `NOT_APPLICABLE` 5), nivel (0-4 = P0-P4), `flags` (`MR_FOUND_PARTIAL`, `LISTED`, `REQUIRES_SHIFT`, `HAS_BACKUP`), partición de la tabla y tabla encontrada que lo describen (`MR_PARTITION_TABLE` si ninguna), posiciones de principal y copia, inicio declarado, número de serie, tipo MBR sugerido y puntuación |
| `mr_found_volume_origin(task, index, buf, cap, out_len)` | Qué candidato propuso el volumen (texto) |
| `mr_found_volume_anomaly(task, volume_index, anomaly_index, out)` | `mr_partition_anomaly` de un volumen (códigos 301-314) o, con `volume_index = MR_PARTITION_TABLE`, de la búsqueda (`image_shifted` 320, `mbr_displaced` 321). `lba` es `int64_t` (`MR_ANOMALY_LBA_SIGNED`) y `value` también (`MR_ANOMALY_VALUE_SIGNED`) |
| `mr_found_volume_anomaly_text(...)` | `"<código estable>\n<descripción>"` |
| `mr_found_table_get(task, index, out)` | `mr_found_table` (32 B): `kind` (`MR_FOUND_TABLE_MBR` 1, `GPT` 2), `lba`, `declared_lba` (`MyLBA`), nº de entradas |
| `mr_found_volume_open(task, index, flags, out_source)` | El volumen como origen (`RangeView` sobre el origen de la búsqueda, que se conserva aunque se cierre). Parcial o desalineado: `MR_ERR_INVALID_ARGUMENT` siempre; con `MR_FOUND_REQUIRES_SHIFT`, solo con `MR_PARTITION_OPEN_APPLY_SHIFT` (la descripción dice «desplazamiento detectado aplicado»); si termina fuera del origen se abre la parte existente |

Consultar el resultado antes del final → `MR_ERR_NOT_FINISHED`; handle de otro tipo →
`MR_ERR_INVALID_HANDLE`; índice fuera de límites → `MR_ERR_OUT_OF_RANGE`. En C#:
`Source.StartPartitionSearch(PartitionSearchOptions?)` devuelve `PartitionSearchTask` (`EngineTask`) con
`GetResult()` (`PartitionSearchResult` con `FoundVolume`, `FoundTable` y anomalías) y
`OpenVolume(index, applyShift)`.

## Modo tolerante de contenedores (`MR_OPEN_TOLERANT`, ADR-020)

Ampliación compatible de la ABI 3 (nuevo bit de `flags`; las estructuras y la versión no cambian). Sin el
bit, un VHD, VHDX o VMDK con entradas de asignación inválidas o ambiguas falla con `MR_ERR_CORRUPT`. Con
él, esas entradas solo descartan su bloque o grano: `mr_source_read` y los hashes fallan sobre ellos
(`MR_ERR_IO`), `mr_source_read_mapped` y la verificación los entregan como `MR_SECTOR_UNREADABLE` con el
patrón de relleno, y `mr_source_describe` termina en «modo tolerante: N bloques descartados (no
fiables)» («granos» en VMDK). Las estructuras críticas dañadas siguen dando `MR_ERR_CORRUPT`. Sin efecto
en RAW (también con `MR_OPEN_FORCE_RAW`) ni en imágenes divididas. En C#:
`Engine.OpenSource(path, ..., tolerant: true)`.

## Garantía sobre datos sustituidos (ABI 3)

- Los sectores ilegibles o saltados se entregan con un patrón de relleno (`BAD!` / `SKIP` por defecto,
  configurables con `mr_source_read_mapped_ex`) **y** su estado.
  Ninguna función presenta el relleno como dato: `mr_source_read` y los hashes son estrictos (fallan) y
  `mr_source_read_mapped` / la verificación devuelven siempre el estado de cada sector.
- En C#, `Source.ReadSectorsMapped(lba, count, retries, badFill, skipFill, jumpSectors, readBack)`
  devuelve `MappedSectors(Data, States)` (sin patrones ni salto usa `mr_source_read_mapped`; si no,
  `mr_source_read_mapped_ex`). `FillPattern` (`TryParse`, `Validate`, `Describe`) aplica la misma regla
  que el motor. El visor colorea cada sector según su estado y marca cada sector no fiable con «NO
  FIABLE: patrón de relleno, no datos del origen».

## Propiedad en C#

`Source`, `HashTask` y `VerifyTask` derivan de `EngineHandle`: `IDisposable` idempotente + finalizador de
seguridad (si el código olvida liberarlos, el recolector lo hace) y `ObjectDisposedException` si se usan
después de liberarlos.

## Pruebas que garantizan el contrato

- Rust (`core/mr-ffi/src/tests.rs`, 33 pruebas): búsqueda de particiones perdidas (tamaños y desplazamientos de las 4 estructuras, flujo completo con apertura de volúmenes también tras cerrar el origen, opciones versionadas y validadas, presupuesto agotado, familias, desplazamiento con anomalías con signo y apertura solo con `MR_PARTITION_OPEN_APPLY_SHIFT`, cancelación con resultado parcial, handles de otro tipo), tamaños de estructuras (incluidas las de particiones), tabla GPT sintética (copia, GUID de disco, nombre, GUID único, nombre de tipo, apertura) y GPT desplazada (copia de respaldo, `image_shifted` con signo, apertura rechazada sin `MR_PARTITION_OPEN_APPLY_SHIFT` y correcta con él), tablas de particiones por índice (esquema, confianza, anomalías de tabla y de partición, nombres de tipo con búfer pequeño, apertura de una partición y de una recortada, tabla que sobrevive a su origen, `struct_size` incorrecto, punteros nulos, handles de otro tipo, EBR ilegible como anomalía), verificación extendida con
  códigos de error simulados (origen `FaultyDevice` registrado con `mr_api::source_from_device`) y
  validación de `mr_verify_options`, protocolo de búfer pequeño, rutas
  de dispositivo, UTF-8 inválido, punteros nulos y cadenas desmesuradas, handles inválidos o de otro
  tipo, orígenes en los 4 tamaños de sector, cierre con tarea en curso, verificación con salto y lectura
  hacia atrás, códigos 10/12/13/14, lectura con estados, `mr_read_options` (patrones configurables con
  salto y lectura hacia atrás, valores por defecto idénticos a `mr_source_read_mapped`, hash estricto,
  validación de patrones, tamaño, bits y punteros), rangos anidados, imágenes divididas, modo tolerante
  (`MR_OPEN_TOLERANT`: estricto → 12, tolerante → descripción, sectores ilegibles con relleno, lectura
  estricta → 2, sin efecto con RAW forzado, inmutabilidad).
- C# (`app/tests/MemRescue.Core.Interop.Tests`): los mismos contratos desde .NET contra la DLL
  real (política de errores y `ErrorCodeList` en `VerifyPolicyTests`; patrones de relleno, preferencias y
  paleta en `FillPatternTests`, incluidos VHD fijo construido según la especificación, descriptor VMDK malicioso, rangos, estados y
  divididas y modo tolerante con un VHD dinámico dañado; particiones en `PartitionTests` con un disco MBR sintético y el corpus completo; búsqueda de particiones perdidas en `PartitionSearchTests` con discos sintéticos y los 38 manifiestos), finalizadores, rutas no ASCII, cancelación, base de datos de trabajo, inmutabilidad del origen y el
  corpus sintético (lectura de marcadores en LBA exactas y cabeceras GPT).
