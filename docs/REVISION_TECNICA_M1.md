# Revisión técnica previa a M1 (2026-10-17)

Alcance pedido por el propietario: (1) lector de imágenes y rechazo de dispositivos, (2) errores de
lectura, (3) desbordamientos y límites de memoria, (4) propiedad de memoria Rust ↔ C#, (5) integridad y
durabilidad de SQLite, (6) autodiagnóstico limitado a 512 B. Ninguna comprobación ha accedido a discos
físicos: los casos de dispositivo se prueban por rechazo **antes** de abrir nada.

Gravedad: **Alta** = riesgo de acceso indebido o pérdida de datos · **Media** = resultado incorrecto o
cuelgue · **Baja** = robustez/diagnóstico.

## Hallazgos y correcciones

| # | Área | Hallazgo | Gravedad | Corrección | Prueba de regresión |
|---|---|---|---|---|---|
| R1 | Rutas | La lista negra de prefijos admitía `\\?\C:` (sin barra), que **abre el volumen C: como dispositivo**; también `\\?\HarddiskVolumeN`, `\\?\CdRom0`, `\\?\Harddisk0Partition1` | **Alta** | `path_policy`: lista blanca léxica (solo `X:\…`, UNC y `\\?\X:\…`/`\\?\UNC\…`), rechazo de `\\.\`, `\??\`, `\Device\`, GLOBALROOT, volúmenes GUID, tuberías/mailslots (también remotas) y nombres reservados (CON, NUL, COM1…, LPT1…, incluso con extensión) | `mr-io` `lexical_rejects_every_device_form` (25 formas), `rejects_device_namespace_paths_without_opening`; CLI `volume_alias_is_refused`; C# `PhysicalDevicePathsAreRejected` (6 casos) |
| R2 | Rutas | Un **enlace simbólico** a `\\.\PhysicalDrive0` se habría seguido al abrir | **Alta** | Resolución manual de la cadena de enlaces con `read_link` (no abre el destino) y validación léxica de cada destino; ruta canónica obligatoria | `symlink_to_device_is_rejected_when_symlinks_are_available` (se ejecuta si el sistema permite crear enlaces; en este equipo no, sin privilegios) + pruebas léxicas |
| R3 | Rutas | Carrera entre comprobación y apertura (sustituir el fichero por un enlace) | Media | Apertura con `FILE_FLAG_OPEN_REPARSE_POINT` y verificación de atributos del objeto abierto (ni directorio, ni dispositivo, ni punto de reanálisis) | `admits_regular_file_and_rejects_directory` |
| R4 | Rutas | Puntos de montaje / uniones | Baja | Se siguen (llevan a ficheros normales), pero la unión en sí nunca se acepta como imagen | `junction_to_directory_is_followed_but_never_opened_as_image` (unión real creada sin privilegios) |
| R5 | Rutas | Falso positivo: un fichero normal llamado `physicaldrive-copia.img` se rechazaba | Baja | Eliminada la regla por subcadena | `lexical_accepts_normal_file_paths` |
| R6 | Formatos | Un VHD/VHDX/VMDK/E01 se habría interpretado como disco RAW (resultados erróneos) | Media | Detección de firmas públicas; apertura rechazada con `UnsupportedFormat` salvo forzado explícito | `format::tests` (todas las firmas), `container_formats_are_not_opened_as_raw_unless_forced`, C# `ContainersAreDetectedAndNotOpenedAsRawUnlessForced` |
| R7 | Lectura | Los errores de lectura no indicaban **dónde** fallaba | Baja | `IoError::Read { offset, len, source }` | `truncated_file_reports_offset_instead_of_garbage`, `strict_policy_fails_with_exact_position` |
| R8 | Lectura | `ErrorKind::Interrupted` abortaba la lectura | Baja | Se reintenta | (revisión de código; no reproducible de forma determinista) |
| R9 | Lectura | Fichero acortado tras abrirlo | Media | Error con posición; nunca datos inventados | `truncated_file_reports_offset_instead_of_garbage` |
| R10 | Memoria | Sin límite por petición de lectura | Media | `MAX_READ_REQUEST` = 64 MiB, comprobado con `u128` antes de reservar | `huge_requests_are_refused_before_allocating` |
| R11 | Aritmética | Rangos LBA + cuenta sin comprobación unificada | Media | `sector_span` con `checked_add` y validación contra el nº de sectores | `read_sectors_validates_strictly` (4 tamaños de sector, `u64::MAX`) |
| R12 | Memoria | Lista de sectores dañados sin límite | Baja | Máximo 100 000 rangos (recuento total exacto, indicador de truncado) | `bad_range_list_is_bounded` |
| R13 | FFI | `from_raw_parts` con longitudes enormes = comportamiento indefinido | **Alta** | Cadenas de entrada limitadas a 64 KiB antes de construir el slice | `oversized_strings_are_rejected_before_reading` (incl. `usize::MAX`) |
| R14 | FFI / C# | Handles no liberados si la GUI olvidaba `Dispose` → crecimiento del registro | Media | `EngineHandle` con finalizador de seguridad (además de `IDisposable`) | C# `ForgottenHandlesAreReleasedByFinalizer`, `DisposedHandlesCannotBeUsed` |
| R15 | FFI | Handles de un tipo usados como de otro | Media | Registro con tipos de entrada; error `INVALID_HANDLE` | `handles_of_the_wrong_kind_are_rejected` |
| R16 | FFI | Cerrar un origen con una tarea en curso | Media | La tarea conserva su propia referencia (`Arc`) | `closing_a_source_does_not_break_a_running_task` |
| R17 | Tareas | Un pánico dentro de una tarea la dejaba **siempre "en curso"** (la GUI esperaría indefinidamente) | Media | `catch_unwind` en el hilo → la tarea pasa a fallida | `panic_inside_task_marks_failed_instead_of_hanging` |
| R18 | Tareas | `spawn` hacía `panic` si el sistema no podía crear el hilo | Baja | Error `ApiError::Spawn` | (revisión de código) |
| S1 | SQLite | Al abrir, todo trabajo en `running` se marcaba `interrupted` **aunque otra instancia lo estuviera ejecutando** | **Alta** | Propietario exclusivo con `<base>.lock` (`File::try_lock`); la segunda apertura falla con `InUse` | `second_owner_is_refused_and_running_jobs_are_not_hijacked` |
| S2 | SQLite | Transiciones de estado leer-luego-escribir (no atómicas) | Media | Transacción `IMMEDIATE` + `UPDATE … WHERE state = <origen>` | `terminal_states_are_final_and_complete_requires_running` |
| S3 | SQLite | `checkpoint` con transacción diferida; checkpoints de trabajos no activos | Media | `IMMEDIATE`; solo trabajos en curso o en pausa | `checkpoints_only_for_running_or_paused_jobs` |
| S4 | SQLite | `synchronous=NORMAL` podía perder el último estado confirmado ante un corte | Media | `synchronous=FULL` | `durability_settings_and_corruption_detection` |
| S5 | SQLite | Base corrupta usada sin aviso | Media | `PRAGMA quick_check` al abrir → `Corrupt` | `durability_settings_and_corruption_detection` (corrompe páginas reales) |
| S6 | SQLite | Conversiones u64↔i64 que saturaban o convertían negativos en 0 | Baja | Conversiones comprobadas → `OutOfRange`; progreso `done ≤ total` | `out_of_range_values_are_errors` |
| A1 | Autodiagnóstico | Abría siempre con sectores de 512 B | Baja | `--sector-size N` y `--read-lba N`; `test.ps1` lo ejecuta con 512 y 4096 comparando con el manifiesto | Pasos GUI de `scripts/test.ps1` |
| Q1 | Calidad | `rust-version` declarada (1.80) inferior a la realmente necesaria | Baja | 1.89 (por `File::try_lock`); clippy lo comprueba | `cargo clippy -D warnings` |

## Riesgos residuales conocidos

- La creación de **enlaces simbólicos de fichero** requiere privilegios en este equipo, así que la prueba
  específica de enlace→dispositivo no se ejecuta aquí; la protección se apoya en la validación léxica de
  cada destino (probada) y en la apertura sin seguir el último enlace (probada con directorios/uniones).
- El bloqueo de propietario es por **fichero de base de datos**: dos tareas simultáneas que registren en
  la misma base dentro del mismo proceso fallarán la segunda con `InUse` (comportamiento seguro, no
  silencioso). M6 introducirá un servicio de almacenamiento compartido.
- `ErrorKind::Interrupted` y fallos de creación de hilos no tienen prueba determinista.
