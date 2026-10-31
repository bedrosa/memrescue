# Árbol virtual (VFS): entradas vivas, borradas y huérfanas con su estado

Crate: `core/mr-vfs` (`mr_vfs::Vfs`). Issue #25 (M4), diseño en
`investigacion/dmde-fase2/INV-01-motor-de-recuperacion.md` §7.2 y §7.3, decisiones por defecto en
ADR-029. Base de la recuperación (#26, [`recuperacion.md`](recuperacion.md)) y de sus informes
(#27): este árbol solo describe; no lee datos ni escribe nada.

## Modelo

Un árbol inmutable por volumen. Cada nodo tiene:

| Campo | Contenido |
|---|---|
| Identificador | `u32` estable: posición en la arena. La raíz es 0 y el padre de un nodo siempre tiene un identificador menor (el árbol no puede tener ciclos) |
| Padre, hijos, profundidad | Hijos en una lista compacta, en el orden determinista de abajo |
| Nombre | Unidades UTF-16 **originales** (sin pérdida, también las sueltas) en un búfer compartido; la forma visible y el escape se calculan al pedirlos |
| Tipo | `fichero`, `directorio`, `sintetico` (carpeta propia de MemRescue) |
| Categoría | `vivo`, `borrado`, `huerfano`, `metadatos`; reservadas `encontrado` (M6) y `raw` (M7) |
| Origen del nombre | `estructura` (índice vivo o `$FILE_NAME` de un registro en uso), `residual` (entrada de índice residual), `registro` (solo el `$FILE_NAME` de un registro no en uso), `sintetico` |
| Estado de los datos | Solo ficheros; ver abajo |
| Indicadores | Ver abajo |
| Tamaño, tamaño asignado, 4 fechas FILETIME, atributos DOS, espacio de nombres | De `$STANDARD_INFORMATION` y `$DATA` (vivos), del índice si el registro no se pudo usar (`times_from_index`), o de `DeletedEntry` (borrados) |
| Referencia al origen | Registro MFT y secuencia (y los del padre que cita el nombre): con ellos la recuperación vuelve a abrir el fichero (`NtfsVolume::open_stream`). Las carpetas sintéticas no tienen |

### Estado de los datos

Resumen por fichero sin leer datos (INV-01 §7.3). Solo `integro` se presenta como íntegro; la
recuperación puede rebajar el estado al leer (sectores ilegibles), nunca subirlo.

| Estado | Cuándo |
|---|---|
| `integro` | Registro en uso (vivo, huérfano en uso o metadatos), flujo principal sin tramos sin mapa ni avisos; también un fichero en uso de tamaño 0 sin `$DATA` |
| `integro_no_verificable` | Borrado con recuperabilidad `full` (ADR-027: todos los clústeres con contenido libres, o residente, o vacío). Nada prueba que no se reutilizaran y liberaran después |
| `parcial` | Vivo con tramos sin mapa o extensiones con runs no válidos; borrado `partial` |
| `sobrescrito` | Borrado `none` con algún clúster en duda (de un fichero vivo, asignado, desconocido o compartido con otro borrado) |
| `sin_datos` | Borrado `none` sin clústeres en duda (sin runs); borrado sin `$DATA` o de tamaño 0 cuando su `$FILE_NAME` dice otro; fichero en uso sin `$DATA` con tamaño distinto de 0; entrada solo residual cuyo registro se reutilizó o está libre |
| `no_disponible` | Registro dañado, ilegible o referencia no válida citados por un índice vivo; flujo con un error tipado de disposición (`stream_*` de ADR-026) |
| `cifrado` | Flujo EFS: el contenido en disco no es el del fichero |

### Indicadores

`deleted` (1), `system` (2), `name_lossy` (4), `times_from_index` (8), `from_index` (16),
`from_mft` (32), `record_damaged` (64), `parent_reused` (128), `cycle_cut` (256), `duplicate_name`
(512, otro hermano con el mismo nombre sin distinguir mayúsculas), `anomalies` (1024),
`assessment_truncated` (2048), `dir_incomplete` (4096, directorio no listado del todo: índice
ilegible, ciclo o profundidad), `hard_link` (8192). Los mismos valores en la ABI
(`MR_VFS_NODE_*`) y como códigos en el JSON de la CLI.

## Construcción desde NTFS (`mr_vfs::ntfs::build_ntfs`)

Consume solo interfaces públicas de `mr-fs-ntfs`: el árbol (`NtfsVolume::tree`), los borrados
(`scan_deleted`) y los ficheros (`NtfsVolume::file`).

1. **Vivos**: los nodos del árbol, con los enlaces duros en cada directorio. Una entrada cuyo
   registro está dañado, es ilegible o es una referencia no válida queda como `vivo`,
   `no_disponible` y `record_damaged`.
2. **Metadatos**: las entradas del sistema de la raíz (registros 0 a 15 y `$Extend` con su
   contenido) cuelgan de `[Metadatos]`, con categoría `metadatos`.
3. **Huérfanos en uso** (registros en uso que no se alcanzan desde la raíz): en `[Huérfanos]`,
   dentro de su directorio si este también es un huérfano en uso con la misma secuencia y, si no, en
   `[Registro N]` (N = registro padre que cita su `$FILE_NAME`).
4. **Borrados** (`scan_deleted`, ADR-027): bajo el directorio vivo (o huérfano en uso) que cita, si
   tiene la **misma secuencia**; bajo el directorio borrado que cita si su secuencia es la citada o
   una más (el sistema la incrementa al liberar el registro), que a su vez se coloca con las mismas
   reglas; si no, en `[Borrados sin carpeta]/[Registro N]` con categoría `huerfano` y, si el padre se
   reutilizó, `parent_reused`. Un ciclo entre directorios borrados se corta (`cycle_cut`) y la
   entrada va a `[Borrados sin carpeta]`. Los hijos de un nodo `huerfano` o `metadatos` heredan esa
   categoría.
5. **Entradas de índice residuales** del árbol vivo (registro libre o reutilizado): si el recorrido
   de borrados encontró el registro, solo añaden `from_index` a su entrada (nunca hay dos); si no,
   se añaden como `borrado`, nombre `residual`, datos `sin_datos`.
6. **Orígenes de los borrados** (`EntryOrigin`, #23): el registro de la MFT da `from_mft`; las
   entradas de índice residuales (parte en uso caducada, espacio libre de un nodo, nodo libre,
   índice de un directorio borrado) dan `from_index`; una entrada puede tener los dos. El nombre es
   `registro` si sale de un registro no en uso y `residual` si solo lo dan los índices; en estas
   últimas los datos son `sin_datos` (registro reutilizado o libre) o `no_disponible` (registro no
   interpretable) y el nombre es el del índice (sin releer el registro, que es de otro fichero).
   La enumeración no es exhaustiva: un origen futuro cuenta como índice.
7. **Nombres sin pérdida**: los vivos llevan las unidades del índice (`DirEntry::name_units`); en
   los borrados y huérfanos, si la forma visible tiene U+FFFD, se vuelve a leer el registro y se
   toman las unidades de su `$FILE_NAME` (`NtfsFile::file_names_with_units`).

Fases con progreso en milésimas (árbol, evaluación de los ficheros en uso, borrados: 3000 en total)
y cancelación en cada una (`Ok(None)`; en `mr-api`, `ApiError::Cancelled`). Los problemas del
origen (anomalías del árbol, registros dañados del recorrido, `$Bitmap`, truncados) se guardan como
**notas** (código, texto y nodo; como mucho 10 000). Un límite truncado del recorrido de borrados
marca el árbol como **parcial**.

## Orden de los hijos

Determinista e independiente del orden de construcción: primero los nodos reales y después las
carpetas sintéticas; dentro de cada grupo por nombre sin distinguir mayúsculas (plegado simple de
Unicode unidad a unidad), a igualdad por las unidades originales, la categoría, el registro y el
identificador. Dos construcciones del mismo origen dan el mismo árbol y los mismos identificadores.

## Carpetas sintéticas

`[Metadatos]`, `[Huérfanos]`, `[Borrados sin carpeta]` y `[Registro N]`: nombres en español entre
corchetes (ADR-029), válidos como nombres de Windows. Un nodo real con el mismo nombre en la raíz
es posible: los dos se listan y la búsqueda por ruta devuelve el primero en el orden (el real).

## Rutas visibles y escape

Ruta = `/` y los componentes separados por `/`. Un componente se escapa solo si lo necesita:
contiene `/`, `\`, un código de control (U+0000 a U+001F, U+007F) o una unidad UTF-16 suelta, o es
`.`, `..` o vacío. En un componente escapado, `%` pasa a `%25`, cada carácter afectado a `%XX` y cada
unidad suelta a `%uXXXX` (hexadecimal en mayúsculas); `.` y `..` pasan a `%2E` y `%2E%2E`; el nombre
vacío a `[Sin nombre]`. Ejemplos: `b<D800>c` → `b%uD800c`, `a/b%` → `a%2Fb%25`, `100%.txt` →
`100%.txt` (sin nada que escapar). La búsqueda por ruta acepta la forma visible, la escapada y,
después, sin distinguir mayúsculas. El escape es solo para mostrar: el nombre de destino de la
recuperación sale de las unidades originales (INV-01 §7.6, #26).

## Filtros y consultas (`mr_vfs::filter`)

Capa pura sobre un árbol terminado. `query(vfs, inicio, recursivo, filtro)` devuelve los nodos
visibles bajo el inicio (en profundidad y en el orden de los hijos, o solo los hijos directos).

- **De todos los nodos**: categorías (máscara) y `deleted_only`.
- **Solo de ficheros**: estados de los datos (máscara), reglas de nombre, tamaño y fecha (rangos
  cerrados, cada extremo opcional; fecha de modificación por defecto, o creación, cambio o acceso).
- **Visibilidad**: un fichero es visible si cumple todo; un directorio, si tiene algún descendiente
  visible o si no hay criterios de fichero y cumple los suyos (con `files_only`, solo por sus
  descendientes).
- **Reglas de nombre**: lista separada por `;`, gana la primera que coincide, `!` delante la
  excluye, comodines `*` y `?`, sin distinguir mayúsculas; con `/` se comparan con la ruta relativa
  al inicio. Sin reglas, todo pasa; si ninguna coincide, pasa solo cuando todas son de exclusión
  (`!*.tmp` excluye los `.tmp` y deja el resto). Como mucho 256 reglas de 1024 caracteres.

## Límites y memoria

| Límite | Por defecto | Al superarlo |
|---|---|---|
| Nodos (incluidas la raíz y las sintéticas) | 16 777 216 | `VfsError::LimitExceeded` (el árbol de `mr-fs-ntfs` usa el mismo máximo) |
| Unidades UTF-16 de nombre en total | 2^29 (1 GiB) | Ídem |
| Profundidad | 4096 | Ídem |
| Notas | 10 000 | Se cuentan y se añade una nota final |

Un nodo ocupa 104 B fijos (comprobado al compilar: ≤ 128 B) más su nombre (2 B por unidad) y 4 B en
la lista de hijos. La prueba de escala construye un millón de nodos y comprueba que la arena no pasa
de 156 B por nodo; las consultas usan un bit por nodo y un recorrido del subárbol. La construcción
NTFS guarda además, mientras dura, el árbol de `mr-fs-ntfs` y la lista de borrados (con sus
propios límites, ADR-027 y ADR-028).

## Interfaces

- Rust: `mr_vfs::{Vfs, VfsBuilder, NewNode, NodeInfo, Filter, NameRules, query, build_ntfs}`;
  `mr_api::{vfs_build_ntfs, start_vfs_build_task, vfs_query}`.
- ABI 3 (ampliación compatible): `mr_vfs_*` (ver `API_FFI.md`).
- C#: `NtfsVolume.StartVirtualFileSystem()` → `VfsBuildTask.GetResult()` → `VirtualFileSystem`.
- CLI: `vfs-ls <imagen> [RUTA] [-r] [--deleted] [--filter MÁSCARAS] [--category LISTA] [--state
  LISTA] [--json] [--partition N | --found N]`.
- Interfaz gráfica: pendiente; el panel de recuperación de #26 usará este árbol.

## Pendiente

- Flujos alternativos por nodo y anomalías detalladas por nodo (hoy, indicadores y notas).
- Estado `reasignado` por tramo y lectura de datos: #26.
- Volúmenes encontrados (`encontrado`, M6) y RAW (M7) como nuevas fuentes del árbol.
- Arena en la base de trabajo SQLite (ADR-016) para volúmenes que no quepan en memoria (M6).
