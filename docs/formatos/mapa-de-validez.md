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

## API (`mr_io::resilient`)

- `ReadPolicy { chunk_sectors (2048), retries (1), on_error (Fail), bad_fill (BAD!),
  skip_fill (SKIP), jump_sectors (0), read_back (false) }`.
- `read_range(dev, start, count, policy, sink, progress) -> Result<ReadReport>`:
  `sink(lba, datos, estados)` recibe tramos con **un estado por sector**, siempre en orden
  creciente, contiguos y sin huecos ni solapes; `progress(hechos, total)` en sectores, `false` cancela.
- `read_sectors_mapped(dev, lba, count, policy) -> Result<(Vec<u8>, Vec<SectorState>)>`: lectura
  acotada a `MAX_READ_REQUEST` para el visor hexadecimal.
- `ReadReport { sectors_requested, retry_reads, map: ValidityMap }`.
- `ValidityMap`: `new`, `push`, `runs`, `truncated`, `count`, `all_trustworthy`, y además
  `total` y `untrusted_runs` (tramos `Unreadable`/`Skipped` guardados).
- Constantes: `MAX_MAP_RUNS = 100_000`, `READ_BACK_MAX_BYTES = 8 MiB`.

## Semántica exacta

1. **Validación**: `count == 0`, desbordamiento de `start + count` o rango fuera del origen →
   `IoError::InvalidRange` antes de leer o reservar nada.
2. **Asignación**: antes de cada bloque se llama a `dev.allocation(lba, n)`. Si los tramos
   devueltos no cubren exactamente el rango se devuelve `IoError::Corrupt` (no se adivina).
   Los tramos `Sparse` no se leen: se entregan ceros con estado `Sparse`.
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
6. **Progreso y cancelación**: se consulta `progress` antes de cada bloque y antes de cada trozo de
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

- `mr_api::verify` devuelve el `ReadReport` con el mapa.
- `mr-ffi` (ABI 2): `sectors_ok = Read + Recovered`, `sectors_bad = Unreadable`, los «rangos
  ilegibles» son los tramos `Unreadable`. Esta ABI no activa saltos. El resto de estados se
  expondrá en la ABI 3.
- CLI `verify`: muestra el recuento por estado y los tramos no fiables; con `--json` añade
  `states`, `untrusted_ranges` y `ranges_truncated` (se conservan `sectors`, `ok`, `bad`,
  `bad_ranges`). Código de salida 3 si hay algún sector no fiable.

## Pruebas (datos sintéticos; `cargo test -p mr-io`)

Dispositivos: `MemDevice` (sector `l` relleno con `l as u8`), `FaultyDevice` (ilegibles,
intermitentes, regiones dispersas y contador de lecturas por sector) y un dispositivo virtual de
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
| `validity_map_*` (4) | fusión, huecos, pánico por desorden/desbordamiento, truncado con recuentos exactos |
| `read_sectors_mapped_for_every_common_sector_size` | visor: estados por sector, errores y límite |
| `fault::tests` (3) | superposición de dispersos, solapes rechazados, contador por sector |

Resultado: `cargo test --workspace` correcto (mr-io: 43 pruebas, de ellas 20 de este módulo y 3 de
`fault`), `cargo clippy --workspace --all-targets -- -D warnings` sin avisos, `cargo fmt --all` aplicado.
