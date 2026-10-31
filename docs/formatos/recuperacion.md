# Recuperación de ficheros y directorios

Crate: `core/mr-recover` (`mr_recover::recover`). Issue #26 (M4), diseño en
`investigacion/dmde-fase2/INV-01-motor-de-recuperacion.md` §7.4 a §7.9, valores por defecto en la
adenda de #26 de ADR-029. El diario que se escribe durante la recuperación está en
[`diario-recuperacion.md`](diario-recuperacion.md); los informes TXT/CSV/HTML (#27) se derivan de él.

La recuperación copia una **selección** del árbol virtual ([`vfs.md`](vfs.md)) a una **carpeta de
destino**. No conoce NTFS: lee los flujos a través del rasgo `ContentSource` (`NtfsContent` sobre
`mr-fs-ntfs`; `mem::MemContent` en memoria para pruebas) y escribe a través del rasgo `OutputSink`,
que en la API es `mr_api::create_output` (`mr_api::ApiSink`).

## Invariantes

1. **El origen nunca se escribe.** El destino no es ni está dentro del origen, de uno de sus
   segmentos o de sus ficheros externos (padres de un disco diferencial), ni es una ruta de dispositivo
   (`\\.\`, `\\?\C:`, `\\?\GLOBALROOT`..., política de ADR-014). Se comprueba antes de leer nada y otra
   vez en cada fichero (`create_output`). El diario tampoco puede ser el origen. La comparación cierra
   ante la duda (como `check_output_path`): la ruta se resuelve por su antecesor existente más profundo
   (forma canónica más los componentes que faltan), así que `imagen\x\y` se rechaza aunque
   `canonicalize` falle por ser `imagen` un fichero y aunque se escriba con otra forma del mismo
   directorio (nombre corto 8.3, mayúsculas, `.`/`..`, `\\?\`); una ruta sin ningún antecesor
   resoluble se rechaza, y un destino dentro de cualquier fichero también. `start_recover_task` (y la
   ABI) lo comprueban antes de lanzar la tarea.
2. **Nada no fiable se presenta como íntegro.** Cada byte escrito tiene su estado de tramo; un fichero
   con algún tramo no fiable baja a `parcial` (nunca sube) y su hash lleva el ámbito
   `escrito_con_relleno`.
3. **Nunca se sustituye un fichero existente** sin la política `sobrescribir` con confirmación
   explícita, o sin una respuesta explícita a una pregunta.
4. **Ningún nombre sale del destino**: los nombres se escapan (abajo) y cada directorio creado o
   reutilizado se comprueba (forma canónica dentro de la raíz, sin punto de reanálisis).
5. **Atomicidad por fichero**: el contenido se escribe en un temporal (`.<nombre>.<pid>-<n>.mrtmp`) del
   mismo directorio y se publica sin sustituir (`hard_link`; en FAT y exFAT, copia exclusiva). Un nombre
   final solo aparece completo; la cancelación o un error borran el temporal.
6. **Memoria acotada**: trozos de 1 MiB, hashes incrementales, como mucho 10 000 tramos no fiables por
   flujo (recuentos exactos), diario en flujo; lineal en el plan (4 B por nodo), en la profundidad y en
   los nombres de un mismo directorio. Medido: 906 KiB de pico con 100 000 ficheros y 1 MiB con un
   fichero de 96 MiB (`core/mr-ffi/tests/recover_memory.rs`).
7. **Determinismo**: mismo árbol, selección y opciones dan los mismos nombres, contenidos y diario (salvo
   fechas de ejecución, identificador y rutas absolutas).

## Selección y orden

- **Selección**: nodos del árbol virtual. La raíz vuelca su contenido directamente en el destino; un
  directorio seleccionado se crea en el destino con su nombre; un fichero seleccionado va a la raíz del
  destino. Un nodo que cuelga de otro seleccionado se ignora.
- **Filtro** (`mr_vfs::filter`): categorías (por defecto en la CLI, la ABI y la interfaz: `vivo`,
  `borrado`, `huerfano`; los `metadatos` solo si se piden), estados, solo borrados, reglas de nombre
  `regla;!exclusión`, tamaño y fecha. Los directorios solo se crean si reciben algún fichero (o tienen
  flujos alternativos propios), salvo `create_empty_dirs`.
- **Orden**: recorrido en profundidad en el orden determinista del árbol; cada directorio se decide
  antes que su contenido y se crea al escribir el primer fichero que lleva dentro; sus fechas se
  aplican al salir de él.

## Política por estado de los datos (adenda de ADR-029)

| Estado | Por defecto | Con opción |
|---|---|---|
| `integro` | Se escribe | |
| `integro_no_verificable` | Se escribe (estado `integro_no_verificable`, nunca `integro`; código 3) | |
| `parcial` | Se escribe con el relleno en los tramos no fiables, `.mrbad` | |
| `sobrescrito` | Se omite (`omitido_estado`) | `--include-overwritten` / `write_states`: se escribe con relleno y `.mrbad` |
| `sin_datos`, `no_disponible` | Se omite | Se intenta; nunca se inventa contenido (error del origen si no hay de dónde leer) |
| `cifrado` | Se omite | `no_soportado` (la exportación en bruto de EFS, FS-05, queda pendiente) |

**Bytes reasignados.** Los tramos `possibly_overwritten` (clústeres de un borrado que ya no son
libres, ADR-027) se escriben **con el patrón de relleno** por defecto; con
`include_reallocated_bytes` (`--include-reallocated-bytes`) se escriben los bytes actuales del disco.
En ambos casos siguen siendo tramos no fiables, en el diario y en el `.mrbad`.

**Relleno**: el patrón de ADR-021 (`BAD!` por defecto, configurable con la misma regla de admisión),
alineado al desplazamiento del flujo.

**Marca de incompleto**: junto a cada fichero con algún tramo no fiable se escribe `nombre.ext.mrbad`
(texto UTF-8, CRLF): ruta en el origen, estado, patrón y cada tramo con desplazamiento, longitud,
estado, si lleva relleno y LBA del volumen y del origen. Con división, `nombre.ext.001.mrbad`. Si la
marca ya existe no se sustituye (se anota en el diario).

## Nombres

Del nombre original UTF-16 a un nombre válido en Windows (`mr_recover::names`). El nombre original
queda siempre en el diario (`nombre`, y `nombre_utf16` si tiene unidades sueltas).

| Caso | Tratamiento |
|---|---|
| `< > : " / \ \| ? *` y códigos 0 a 31 | `%XX` |
| Unidad UTF-16 suelta | `%uXXXX` (FAT y exFAT no las admiten) |
| `%` en un nombre que necesita escape | `%25` (en los demás se deja) |
| Espacio o punto final | `%20` / `%2E` en el último carácter |
| `.`, `..`, vacío | `%2E`, `%2E%2E`, `[Sin nombre]` |
| Reservado (`CON`, `PRN`, `AUX`, `NUL`, `COM0`-`COM9`, `LPT0`-`LPT9`, `COM¹`..., `CONIN$`, `CONOUT$`, `CLOCK$`) con o sin extensión | `_` tras la base: `NUL.txt` → `NUL_.txt` |
| Más de 231 unidades (255 menos una reserva para sufijos) | se recorta la base, se conserva la extensión y se añade `~XXXXXXXX` (CRC-32 de las unidades originales) |

Flujos alternativos: el mismo escape de caracteres (sin reservados). Las carpetas sintéticas
(`[Metadatos]`, `[Huérfanos]`...) son nombres válidos y se crean tal cual.

**Rutas largas**: todas las rutas de E/S se construyen desde la forma canónica de la raíz
(`\\?\X:\...`), así que funcionan por encima de 260 caracteres sin `LongPathsEnabled`. El diario marca
`ruta_larga` cuando la ruta completa supera 259 caracteres y la CLI avisa con su número.

## Conflictos

Un nombre de destino choca con otro de la **misma recuperación** (mayúsculas en un destino que no las
distingue, versión viva y borrada del mismo nombre, nombres que el escape iguala) o con algo que **ya
existe** en el destino.

- Dentro de la misma recuperación se renombra siempre (ninguna entrada seleccionada omite ni sustituye
  a otra): el primero en el orden del árbol conserva el nombre.
- Sufijos: ` (2)`, ` (3)`... antes de la extensión (`nombre (2).ext`; en directorios al final); los
  **borrados** llevan ` (borrado)`, ` (borrado 2)`... para distinguirlos de la versión viva.
- Ante algo que ya existe:

| Política (`--conflict`) | Fichero | Directorio |
|---|---|---|
| `preguntar` (ABI/interfaz; por defecto en la interfaz) | Pregunta con «aplicar a todos» | Ídem |
| `renombrar` (`rename`, por defecto en la CLI y la ABI) | Siguiente sufijo libre | `nombre (2)` |
| `omitir` (`skip`) | `omitido_conflicto` | No se usa; su contenido se omite (`omitido_directorio`) |
| `omitir_si_identico` (`skip-identical`) | Se omite si el existente tiene el mismo tamaño y SHA-256 que lo que se escribiría (con el mismo relleno); si no, se prueba el siguiente sufijo (así una repetición reconoce también `nombre (2).ext`) | Se reutiliza (fusión) |
| `sobrescribir` (`overwrite` + `--confirm-overwrite`) | Sustitución atómica (`rename`) | Se reutiliza |

- Límite de renombrados de un mismo nombre: 1000 (después se omite y se anota `omitido_limite`).
  INV-01 proponía 10; con 10, una carpeta recuperada once veces en el mismo destino dejaría de
  recuperarse.
- **Carrera**: si el nombre aparece entre la decisión y la publicación, la publicación falla sin tocar
  el fichero ajeno y se vuelve a resolver el conflicto (hasta 3 veces).
- **Reanudar** = repetir la recuperación con `omitir_si_identico`: lo ya escrito y completo se reconoce
  (también un conjunto de partes) y no se vuelve a escribir. No hay reanudación a mitad de fichero.
- El diario y su carpeta (`MemRescue-registro`) están reservados en la raíz: nada recuperado los
  sustituye.

## Capacidades del destino

Sonda en la raíz con ficheros `.mrprobe-*` que se borran al momento (fijables con
`RecoverOptions::capabilities` para pruebas):

- **Flujos con nombre**: se crea un fichero y se abre `fichero:mrprobe`. Si funciona (NTFS, ReFS), los
  flujos alternativos se escriben como flujos del fichero de salida; si no (FAT, exFAT, red), como
  ficheros hermanos `nombre.ext%3Aflujo` (atómicos, con la misma política de nombres).
- **Mayúsculas**: se crea `.mrprobe-...-a` y se mira si existe en mayúsculas.
- **Tamaño máximo**: solo en destinos sin flujos y si algún flujo principal supera 4 GiB − 1, se fija
  el tamaño de un fichero de sonda en 4 GiB; un rechazo que no es por espacio libre indica FAT32.

**Flujos alternativos y la política de rutas.** Los flujos se escriben abriendo
`<temporal>:<flujo>` del fichero que acaba de crear `create_output` (antes de publicarlo, de modo que
el flujo viaja con el fichero), o `<directorio>:<flujo>` de un directorio ya creado y comprobado. Esas
rutas con `:` **nunca** pasan por `check_output_path` (que rechaza componentes con `:` en Windows): se
valida el fichero o el directorio base y después se abre su flujo.

## División

| Política | Efecto |
|---|---|
| `auto` (por defecto) | Si el destino tiene límite de tamaño y el flujo principal lo supera, partes `nombre.ext.001`, `.002`... de 4 GiB − 1 MiB (o del límite si es menor) |
| `nunca` | Error de destino para ese fichero |
| `tamaño N` (≥ 4096) | Siempre que el flujo principal supere N, partes de N bytes |

Cada parte es atómica; si la copia se cancela o falla, se borran las partes ya publicadas de ese
fichero. El diario da los hashes del conjunto (en la misma pasada) y de cada parte. Los flujos
alternativos de un fichero dividido van como ficheros hermanos.

## Fechas y atributos

Después de escribir todos los flujos y de publicar: creación, modificación y acceso de
`$STANDARD_INFORMATION` con 100 ns (`File::set_times`; los directorios se abren con
`FILE_FLAG_BACKUP_SEMANTICS` y se fechan al salir de ellos). Al final, el atributo de solo lectura.
Oculto, sistema, comprimido, cifrado, disperso y propietario/permisos no se aplican (el diario guarda
los atributos originales); los enlaces duros se recuperan como copias independientes y los puntos de
reanálisis no se recrean.

## Errores, cancelación y progreso

- Un error del **origen** (registro reutilizado o dañado, flujo con error tipado, E/S) o del
  **destino** (permiso, nombre rechazado) se anota en el diario con su clase y la recuperación sigue.
  Destino lleno: la recuperación se detiene con estado `fallida`.
- **Cancelación** entre trozos y entre ficheros: el fichero en curso no se publica (su temporal se
  borra) y se anota como `cancelado`; el diario termina con `cancelada` y lista lo ya escrito.
- **Progreso**: ficheros hechos/total, bytes de los flujos principales (el total sale de los
  metadatos de los ficheros que se van a escribir, sin leer datos), bytes no fiables, errores y ruta en
  curso.

## Interfaces

- **Rust**: `mr_recover::recover(vfs, content, sink, source, selection, options, observer)`;
  `mr_api::recover` (síncrona) y `mr_api::start_recover_task` (tarea con progreso, preguntas de
  conflicto respondidas por sondeo y cancelación).
- **ABI 3** (`mr_recover_*`, [`API_FFI.md`](../API_FFI.md)), **C#** (`VirtualFileSystem.StartRecover`,
  `RecoverTask`), **CLI** (`recover`, abajo) y **interfaz** (pestaña «Recuperación»).

```
memrescue-cli recover <imagen> --to DIR [RUTA...] [--partition N|--found N] [--deleted]
                      [--filter MÁSCARAS] [--category LISTA] [--state LISTA]
                      [--conflict rename|skip|skip-identical|overwrite [--confirm-overwrite]]
                      [--include-overwritten] [--include-reallocated-bytes] [--journal F]
```

Códigos de salida: **0** todo lo seleccionado se escribió y es `integro` (o ya estaba, idéntico);
**3** algún fichero no es íntegro (escrito con tramos no fiables, borrado de procedencia no
verificable u omitido por su estado); **1** errores de fichero, destino rechazado, cancelación o fallo;
**2** uso incorrecto.

## Limitaciones conocidas

- EFS (FS-05): `cifrado` se informa como `no_soportado`; la exportación de `$DATA` cifrado y `$EFS` en
  ficheros separados queda pendiente.
- La sonda de 4 GiB distingue FAT32 por el error del sistema al fijar el tamaño; un destino de red sin
  flujos que rechace tamaños grandes por otra razón también se dividiría.
- La comparación de nombres dentro de la recuperación usa el plegado simple de Unicode, no la `$UpCase`
  del volumen de destino.
- Sin caché de sectores malos por LBA: un sector ilegible compartido por dos ficheros se lee dos veces.
- El mismo disco físico como origen y destino (M9) no se detecta: hoy los orígenes son imágenes.
- Una pregunta pendiente bloquea la tarea hasta la respuesta o la cancelación.
