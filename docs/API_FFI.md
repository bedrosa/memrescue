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
| Estructuras | `#[repr(C)]`, campos de ancho fijo, relleno explícito (`reserved`). Tamaños comprobados por pruebas en ambos lados: `mr_image_info` = 32 B, `mr_task_status` = 24 B, `mr_verify_report` = 72 B, `mr_verify_options` = 48 B |
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
| `mr_verify_untrusted_range(task, index, out_lba, out_count, out_state)` | Tramo no fiable nº `index` (2 ilegible, 3 saltado). Sustituye a `mr_verify_bad_range` (ABI 2) |
| `mr_task_status_get(task, out)` | Estado y progreso (bytes en hash, sectores en verificación) |
| `mr_task_cancel(task)` | Solicita la cancelación |
| `mr_task_free(task)` | Libera el handle (cancela si sigue en marcha) |

Códigos añadidos en ABI 2: 10 y 11; en ABI 3: 12, 13 y 14. Las tres funciones de la política
de errores (IO-09) y `mr_source_read_mapped_ex` con `mr_read_options` (IO-08) se añadieron sin cambiar la
versión: no modifican firmas ni estructuras existentes. Límites:
cadenas de entrada ≤ 64 KiB (`MR_MAX_STRING_BYTES`), lecturas ≤ 64 MiB (`MR_MAX_READ_BYTES`).

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

- Rust (`core/mr-ffi/src/tests.rs`, 21 pruebas): tamaños de estructuras, verificación extendida con
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
  divididas y modo tolerante con un VHD dinámico dañado), finalizadores, rutas no ASCII, cancelación, base de datos de trabajo, inmutabilidad del origen y el
  corpus sintético (lectura de marcadores en LBA exactas y cabeceras GPT).
