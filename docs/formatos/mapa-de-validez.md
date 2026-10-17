# Mapa de validez de lectura y sectores ilegibles

Módulo: `core/mr-io/src/resilient.rs` (pruebas: el mismo fichero y `core/mr-io/src/fault.rs`).

## Objetivo

Cuando un sector no se puede leer, el lector tolerante entrega un **patrón de relleno** en su
lugar para poder seguir. Requisito del propietario: ese relleno **nunca** debe poder confundirse
con datos recuperados. Por eso cada sector entregado lleva un estado explícito y el informe final
incluye un mapa de validez con recuentos exactos.

## Estados (`mr_io::SectorState`)

| Estado       | Contenido entregado                    | ¿Fiable? | Cuándo                                                    |
|--------------|----------------------------------------|----------|-----------------------------------------------------------|
| `Read`       | Datos del origen                       | Sí       | Leído al primer intento (en bloque o individual)          |
| `Recovered`  | Datos del origen                       | Sí       | Leído tras uno o más reintentos individuales              |
| `Unreadable` | Patrón `bad_fill` (por defecto `BAD!`) | **No**   | Falló tras todos los reintentos (`FillAndContinue`)       |
| `Skipped`    | Patrón `skip_fill` (por defecto `SKIP`)| **No**   | No se intentó leer: dentro de un salto tras un ilegible   |
| `Sparse`     | Ceros                                  | Sí       | `allocation()` lo declara no asignado; no se lee          |

`SectorState::is_trustworthy()` es `false` solo para `Unreadable` y `Skipped`.

`Sparse` incluye todo lo que el contenedor declara sin datos propios en un disco sin padre: bloques
no asignados de VHD/VMDK y bloques VHDX NOT_PRESENT, ZERO, UNDEFINED y UNMAPPED. Los dos últimos se
entregan como ceros igual que `qemu-img`, porque la especificación no define su contenido (ver
`vhdx.md`, «Decisiones»); no son daño ni se leen como datos.

## Patrones de relleno configurables (IO-08, ADR-021)

Los dos patrones son de 4 bytes y se repiten en todo el sector en el orden escrito. Por defecto
`DEFAULT_BAD_FILL` = `BAD!` (42 41 44 21) y `DEFAULT_SKIP_FILL` = `SKIP` (53 4B 49 50).

- **Regla de admisión** (`validate_fill_patterns(bad, skip)`, `FillPatternError`): ningún patrón puede
  tener sus cuatro bytes iguales (`Zero` para `00 00 00 00`, indistinguible de los sectores `Sparse` y de
  datos a cero; `Uniform` para `FF FF FF FF`, `E5 E5 E5 E5`…, rellenos habituales en discos reales) y
  ilegibles y saltados deben ser distintos (`Identical`). La aplican igual el motor (`mr-api`, `mr-ffi`),
  la CLI y la interfaz (`FillPattern.Validate` en C#).
- **Formato de entrada** (`parse_fill_pattern`, `FillPattern.TryParse`): exactamente 4 caracteres ASCII
  imprimibles (`BAD!`) o exactamente 8 dígitos hexadecimales con prefijo `0x` y espacios opcionales
  (`DEADBEEF`, `0xDE AD BE EF`). Las preferencias guardan siempre los 8 dígitos.
- La regla solo evita volcados engañosos: la **autoridad es el estado** de cada sector. Un sector del
  origen que contenga el patrón configurado sigue siendo `Read` (`fill_pattern_in_source_does_not_change_states`).
- Los hashes nunca incluyen relleno, sea cual sea el patrón: `hash_image` es estricto (pruebas
  `mapped_read_with_custom_fills_and_jump` en mr-api, `read_mapped_ex_uses_the_configured_fill_patterns` en
  mr-ffi y `HashNeverIncludesFillWhateverThePattern` en C#).
- La verificación no admite patrones: no entrega datos, solo recuentos y tramos, así que el patrón no
  cambiaría su resultado.

## API (`mr_io::resilient`)

- `ReadPolicy { chunk_sectors (2048), retries (1), on_error (Fail), bad_fill (BAD!),
  skip_fill (SKIP), jump_sectors (0), read_back (false), skip_error_codes ([]),
  ignore_all_errors (false) }` y `ReadPolicy::omits(&IoError) -> bool`.
  `retries` se acota dentro del lector a `MAX_RETRIES` (100), el mismo límite que validan la FFI y la
  CLI: un valor mayor desde la API de Rust no prolonga la lectura de un sector ilegible.
- `IoError::os_error_code() -> Option<i32>`: código de error del sistema de un fallo de E/S.
- `read_range(dev, start, count, policy, sink, progress) -> Result<ReadReport>`:
  `sink(lba, datos, estados)` recibe tramos con **un estado por sector**, siempre en orden
  creciente, contiguos y sin huecos ni solapes; `progress(hechos, total)` en sectores, `false` cancela.
- `read_sectors_mapped(dev, lba, count, policy) -> Result<(Vec<u8>, Vec<SectorState>)>`: lectura
  acotada a `MAX_READ_REQUEST` para el visor hexadecimal.
- `validate_fill_patterns`, `parse_fill_pattern`, `FillPatternError`, `DEFAULT_BAD_FILL`,
  `DEFAULT_SKIP_FILL` (ver la sección anterior).
- En `mr-api`: `read_sectors_mapped(src, lba, count, retries)` (rellenos por defecto, sin salto) y
  `read_sectors_mapped_with(src, lba, count, &MappedReadOptions { retries, jump_sectors, read_back,
  bad_fill, skip_fill })`, que valida los patrones antes de leer (`ApiError::InvalidArgument`).
- `ReadReport { sectors_requested, retry_reads, omitted_errors, error_codes, map: ValidityMap }`:
  `omitted_errors` cuenta los sectores cuyo error se omitió por la política; `error_codes`
  (`BTreeMap<i32, u64>`) cuenta, por código del sistema, los sectores `Unreadable` cuyo último
  intento falló con él (incluida la lectura hacia atrás; los errores sin código no aparecen).
- `ValidityMap`: `new`, `push`, `runs`, `truncated`, `count`, `all_trustworthy`, y además
  `total` y `untrusted_runs` (tramos `Unreadable`/`Skipped` guardados).
- Constantes: `MAX_MAP_RUNS = 100_000`, `READ_BACK_MAX_BYTES = 8 MiB`.

## Semántica exacta

1. **Validación**: `count == 0`, desbordamiento de `start + count` o rango fuera del origen →
   `IoError::InvalidRange` antes de leer o reservar nada.
2. **Asignación**: antes de cada bloque se llama a `dev.allocation(lba, n)`. Si los tramos
   devueltos no cubren exactamente el rango se devuelve `IoError::Corrupt` (no se adivina).
   Los tramos `Sparse` no se leen: se entregan ceros con estado `Sparse`.
   **Error al consultar la asignación** (issue #39; p. ej. el mapa de bits de un bloque VHD está
   en un sector defectuoso del soporte):
   - `FillAndContinue`: no aborta. El bloque entero se trata como `Data` (conservador): se intenta
     leer con la fase 3 y, si falla, sus sectores quedan `Unreadable` con `bad_fill`; nunca se
     suponen `Sparse` ni se presentan como datos sin haberlos leído. Lo mismo dentro de un salto
     (`Skipped` con `skip_fill`) y en la lectura hacia atrás (se intenta leer toda la ventana).
     Si la lectura funciona, el sector es `Read` con los bytes que entrega el dispositivo, aunque
     el contenedor lo considerase no escrito (ceros). El mapa del resto del rango no cambia.
   - `Fail` (hashes): el error se propaga con su posición, como antes.
   - Política de errores: la consulta de asignación **no** se reintenta ni se evalúa contra
     `skip_error_codes`/`ignore_all_errors`. Reintentos y omisión se aplican, igual que en
     cualquier otro tramo `Data`, a la lectura de los sectores afectados; su código de error
     queda en `error_codes` si siguen ilegibles. Así el comportamiento es uno solo, y un fallo de
     metadatos cuesta como mucho una consulta por bloque más los intentos normales de lectura.
   - Un mapa **incoherente** (tramos que no cubren el rango) sigue abortando en ambos modos: es
     un fallo de la implementación del dispositivo, no del soporte.
   - `Cancelled` devuelto por la consulta se propaga siempre.
3. **Datos**: lectura por bloques de `chunk` sectores (acotado a `MAX_READ_REQUEST`). Si el bloque
   falla, se relee sector a sector: éxito al primer intento individual → `Read`; éxito tras
   reintento → `Recovered`; fallo tras `1 + retries` intentos →
   - `Fail`: se devuelve el error del dispositivo con el desplazamiento exacto del sector;
   - `FillAndContinue`: relleno `bad_fill`, estado `Unreadable`.
   Nota: si un fallo intermitente lo «consume» la lectura del bloque y el primer intento individual
   funciona, el sector es `Read`. Ambos estados son fiables; la distinción es informativa.
4. **Salto** (`jump_sectors > 0`, solo con `FillAndContinue`): tras un `Unreadable` en el sector
   `s`, los sectores `[s+1, min(s+1+N, fin del rango))` no se leen y se marcan `Skipped` con
   `skip_fill`, salvo los `Sparse`, que siguen `Sparse` con ceros. Los sectores del bloque en curso
   posteriores a `s` se descartan y pasan a formar parte del salto. Tras el salto se reanuda la
   lectura normal (un nuevo ilegible provoca un nuevo salto).
5. **Lectura hacia atrás** (`read_back`): antes de entregar el salto se lee desde su último sector
   hacia atrás, **un intento por sector** (sin reintentos), saltándose los `Sparse`. Los legibles
   pasan a `Read`; el primer fallo queda `Unreadable` y el resto (hacia el inicio del salto) sigue
   `Skipped`. La lectura hacia atrás no provoca nuevos saltos.
6. **Omisión de errores** (IO-09; `skip_error_codes`, `ignore_all_errors`): se evalúa en **cada
   intento individual fallido** de la fase 3. Si el error tiene un código del sistema incluido en
   `skip_error_codes` (p. ej. 23 `ERROR_CRC`, 1117 `ERROR_IO_DEVICE`), o si `ignore_all_errors`, el
   sector **no se reintenta más** y pasa directamente a fallo definitivo. Detalles:
   - El estado resultante es siempre `Unreadable` con `bad_fill` (se intentó leer y falló), nunca
     `Recovered` ni `Read`. `Skipped` sigue reservado a sectores que no se intentan leer (saltos).
   - La política decide **si se insiste**, no si se aborta: con `OnError::Fail` el error omitido
     sigue abortando con su posición exacta (los hashes siguen siendo estrictos).
   - `retries` solo se aplica a errores no omitidos; con «ignorar todo» nunca hay `Recovered`.
   - Un error sin código del sistema (estructural, simulado sin código) solo se omite en modo
     «ignorar todo».
   - La lectura del **bloque** no se evalúa: un bloque fallido siempre se relee sector a sector (un
     intento por sector como mínimo) para no marcar como ilegibles sectores legibles.
   - **Salto**: un sector omitido es `Unreadable` como cualquier otro, así que provoca el salto de
     `jump_sectors` si está configurado. **Lectura hacia atrás**: ya es de un intento sin reintentos;
     la política no la cambia, pero su fallo también se registra en `error_codes`.
7. **Progreso y cancelación**: se consulta `progress` antes de cada bloque y antes de cada trozo de
   un salto (los saltos enormes también se pueden cancelar). Cancelación → `IoError::Cancelled`.
   Un error devuelto por `sink` se propaga tal cual.

## Garantías (lo que nunca puede ocurrir)

- Un sector con relleno (`bad_fill`/`skip_fill`) nunca se entrega con estado `Read`, `Recovered`
  ni `Sparse`; un sector `Read`/`Recovered` contiene exactamente los bytes del origen.
- Si el origen contiene literalmente el patrón (`BAD!`…), el estado sigue siendo `Read`: la única
  fuente de verdad es el estado, nunca el contenido.
- `ValidityMap::count` y `all_trustworthy()` son exactos aunque la lista de tramos se trunque.
- La suma de recuentos por estado es igual a `sectors_requested`.
- Un hash (`mr_api::hash_image`) nunca se calcula sobre relleno: es estricto y aborta ante el
  primer error de lectura.
- Un sector `Skipped` o `Sparse` no recibe ninguna lectura individual (comprobado contando
  lecturas por sector); solo puede haber sido tocado por la lectura de bloque que falló antes de
  conocerse el error.

## `ValidityMap`

Tramos `(lba, nº, estado)` ordenados, sin solapes, fusionados (dos tramos contiguos nunca tienen el
mismo estado); se admiten huecos. `push` con `count == 0` no hace nada. Un `push` desordenado o
solapado (`lba` menor que el final del último tramo) o que desborde `u64` **provoca pánico**: es un
error de programación y aceptarlo duplicaría sectores y falsearía los recuentos. A partir de
`MAX_MAP_RUNS` tramos no se guardan más, `truncated()` pasa a `true` y los recuentos siguen exactos.

## Límites de memoria

Independientes del tamaño del rango y del salto:
- Búfer de salida: `chunk × sector` bytes (≤ `MAX_READ_REQUEST` = 64 MiB). Los saltos se entregan
  troceados en piezas de ese tamaño.
- Búfer de lectura hacia atrás: ≤ `READ_BACK_MAX_BYTES` (8 MiB). Si se llena sin encontrar un error,
  la lectura hacia atrás termina y el resto del salto queda `Skipped` (conservador: nunca afirma
  fiabilidad sin haber leído).
- `ValidityMap`: ≤ `MAX_MAP_RUNS` tramos.

## Uso en el resto del motor

- Contenedores en modo tolerante (ADR-020): los bloques o granos con entradas de asignación inválidas o
  ambiguas fallan en `read_at` con `IoError::Read` y se informan como `Data` en `allocation`, así que este
  lector los entrega como `Unreadable` con `bad_fill`; nunca aparecen como `Read` ni como `Sparse`.
- VHD dinámico: `allocation` lee del fichero el mapa de bits de cada bloque asignado. Si esa lectura
  falla, el bloque se informa como `Data` en lugar de hacer fallar toda la consulta; `read_at` de esos
  sectores vuelve a necesitar el mapa y falla con `IoError::Read` y el desplazamiento virtual exacto,
  así que con `FillAndContinue` el bloque queda `Unreadable` y con `Fail` la operación aborta. VHDX y
  VMDK construyen su mapa de asignación en memoria al abrir (BAT y tablas de granos) y su
  `allocation` no lee del fichero, así que no pueden fallar por esta causa.

- `mr_api::verify` devuelve el `ReadReport` con el mapa.
- `mr-ffi` (ABI 3): `mr_source_read_mapped` (rellenos por defecto) y `mr_source_read_mapped_ex` con
  `mr_read_options` (reintentos, salto, lectura hacia atrás y los dos patrones; patrones no admitidos →
  `MR_ERR_INVALID_ARGUMENT`);
  `mr_source_verify_start` (reintentos, salto, lectura hacia atrás) y
  `mr_source_verify_start_ex` con `mr_verify_options` (además, `skip_codes` y
  `MR_VERIFY_IGNORE_ALL_ERRORS`); `mr_verify_error_summary` / `mr_verify_error_code` exponen
  `omitted_errors` y `error_codes`. Ver `docs/API_FFI.md`.
- Interfaz, visor: cada sector se muestra en un bloque con el color de su estado (`SectorStatePalette`),
  su cabecera con el estado en texto y, si no es fiable, la línea «!! SECTOR NO FIABLE … PATRÓN DE
  RELLENO <patrón> … NO datos del origen» y una barra lateral gruesa; fila de estados por sector y
  leyenda con los patrones en uso. Herramientas → «Patrones de relleno de sectores no fiables…» edita y
  guarda los patrones (preferencias por usuario, ADR-021).
- CLI `sectors`: `--bad-fill P`, `--skip-fill P`, `--jump N` y `--read-back` (patrones no admitidos →
  código 2; solo se aceptan en `sectors`).
- Interfaz: pestaña Comprobaciones, casilla «Ignorar todos los errores (sin reintentos)»
  (`IgnoreErrorsBox`) y lista «No reintentar los códigos de error» (`SkipCodesBox`).
- CLI `verify`: muestra el recuento por estado y los tramos no fiables; con `--json` añade
  `states`, `untrusted_ranges` y `ranges_truncated` (se conservan `sectors`, `ok`, `bad`,
  `bad_ranges`). Código de salida 3 si hay algún sector no fiable. `--skip-codes 23,1117` e
  `--ignore-errors` activan la política; el texto y el JSON (`error_policy`, `omitted_errors`,
  `error_codes`) registran la política aplicada y su efecto.

## Pruebas (datos sintéticos; `cargo test -p mr-io`)

Dispositivos: `MemDevice` (sector `l` relleno con `l as u8`), `FaultyDevice` (ilegibles,
intermitentes, regiones dispersas, contador de lecturas por sector y un código de error del sistema
por sector con `with_bad_code` / `with_flaky_code`, mediante `io::Error::from_raw_os_error`, y
regiones cuya consulta de asignación falla con `with_allocation_error`) y un dispositivo virtual de
2 GiB sin datos en memoria. Una comprobación común (`assert_honest`) verifica sector a sector que el
contenido corresponde al estado y que mapa, recuentos y `all_trustworthy()` coinciden con lo entregado.

| Prueba | Qué demuestra |
|---|---|
| `clean_read_matches_source_for_every_common_sector_size` | 512/1024/2048/4096, todo `Read` |
| `strict_policy_fails_with_exact_position` | `Fail` con desplazamiento exacto (también con saltos configurados) |
| `fill_marks_unreadable_sectors_and_never_reports_them_as_read` | tramos, recuentos, `all_trustworthy()` falso |
| `fill_pattern_in_source_does_not_change_states` | el estado manda, no el contenido |
| `retries_recover_flaky_sectors` | `Recovered`, `Read` y fallo definitivo |
| `jump_without_read_back_contiguous_damage` | salto sin lecturas en la zona saltada |
| `jump_with_read_back_contiguous_damage` | lectura hacia atrás hasta el siguiente error |
| `jump_with_scattered_damage_and_repeated_jumps` | daño disperso, saltos repetidos, con y sin lectura atrás |
| `jump_is_bounded_by_the_end_of_the_range` | el salto no supera el final del rango |
| `sparse_regions_are_not_read_and_are_zeros` | dispersos sin lecturas |
| `sparse_inside_a_jump_stays_sparse` | `Sparse` dentro de un salto, con y sin lectura atrás |
| `read_back_stops_at_buffer_limit_and_leaves_rest_skipped` | límite de 8 MiB |
| `huge_jump_uses_bounded_memory` | salto de ~4 M sectores con entregas ≤ `chunk` |
| `cancellation_and_invalid_ranges` | cancelación (también dentro de un salto), rangos inválidos, error del `sink` |
| `incoherent_allocation_is_rejected` | asignación incoherente → `Corrupt` |
| `allocation_error_is_read_as_data_in_fill_mode` | consulta de asignación fallida: el bloque se lee como `Data` (sus dispersos también), el ilegible queda `Unreadable`, el resto del mapa exacto; si la lectura también falla, `Unreadable` y nunca `Sparse` |
| `allocation_error_aborts_in_strict_mode` | con `Fail`, el error de asignación aborta con su posición |
| `allocation_error_inside_a_jump_is_skipped_or_read_back` | dentro de un salto, `Skipped` sin leer; con lectura hacia atrás, la ventana se lee entera |
| `allocation_error_keeps_the_read_error_policy` | reintentos y omisión por código se aplican a la lectura del tramo afectado |
| `validity_map_*` (4) | fusión, huecos, pánico por desorden/desbordamiento, truncado con recuentos exactos |
| `read_sectors_mapped_for_every_common_sector_size` | visor: estados por sector, errores y límite |
| `skip_codes_avoid_retries_only_for_listed_codes` | códigos de la lista sin reintentos (2 lecturas), otros códigos y sin código con reintentos; recuentos y tabla de códigos |
| `ignore_all_never_retries_and_never_recovers` | «ignorar todo»: un intermitente recuperable queda `Unreadable`; la lista no afecta a errores sin código |
| `omitted_errors_still_abort_in_strict_mode` | con `Fail`, el error omitido aborta con posición y código, sin reintentos |
| `omitted_errors_combine_with_jump_and_read_back` | omisión + salto (con y sin lectura hacia atrás), saltados sin leer |
| `omitted_errors_at_the_edges_of_the_range` | primer/último sector del rango, vecinos fuera del rango nunca leídos, salto acotado, rango de 1 sector |
| `policy_omits_by_code_or_all` | `ReadPolicy::omits` y `IoError::os_error_code` |
| `fill_patterns_are_parsed_and_validated` | formato ASCII/hex, textos rechazados, regla de admisión (ceros, uniformes, iguales) |
| `custom_fills_are_delivered_with_their_state` | patrones propios con salto: cada relleno solo con su estado, dispersos intactos |
| `fault::tests` (5) | superposición de dispersos, solapes rechazados, contador por sector, códigos por sector, errores de asignación simulados |
| `mr-image`: `vhd::tests::unreadable_bitmap_reports_data_and_verify_marks_it_unreadable` | VHD truncado en mitad del mapa de bits de un bloque tras abrirlo: `allocation` lo informa como `Data`, la verificación con relleno termina con ese bloque `Unreadable` y el resto exacto (incluidos dispersos y bloques posteriores), y en modo estricto aborta con la posición |

Resultado: `cargo test --workspace` correcto (mr-io: 73 pruebas, de ellas 32 de este módulo y 5 de
`fault`), `cargo clippy --workspace --all-targets -- -D warnings` sin avisos, `cargo fmt --all` aplicado.
