# INV-01 · Motor de recuperación: comportamiento visible y diseño propio de M4

Fecha: 2026-10-18 · Issue #24 (hito M4 · VFS and recovery engine) · Alimenta #25 (`mr-vfs`), #26
(`mr-recover`) y #27 (`mr-report`).

> Sala limpia (ADR-001): en esta fase **no** se ha ejecutado DMDE ni se ha analizado su binario (ni
> decompilación, ni desensamblado, ni cadenas, ni herramientas de ingeniería inversa). El punto de
> partida que cita `ROADMAP.md` (rango de cadenas de F1-08) **no** se ha consultado. Las fuentes son el
> manual público de DMDE, documentación pública de otras herramientas para la práctica general,
> documentación pública de Windows y de los formatos, y los documentos y el código de este repositorio.
> Del manual solo se recogen hechos breves con su cita; el diseño de §7 a §10 es propio de MemRescue.

Leyenda:

- **OBS** = observado en la documentación pública de DMDE (se indica URL y sección).
- **REPO** = consta en un documento o en el código de este repositorio (se indica dónde).
- **SPEC** = especificación o documentación pública de terceros (se indica URL y sección).
- **INF** = inferencia propia, razonada a partir de lo anterior.
- **DESC** = desconocido: no se puede afirmar con las fuentes permitidas.

## 1. Fuentes consultadas

| Fuente | URL | Uso |
|---|---|---|
| Manual de DMDE: *Data Recovery* | <https://dmde.com/manual/datarecovery.html> | Diálogo de recuperación, opciones, listas, informe, EFS |
| Manual de DMDE: *File Panel* | <https://dmde.com/manual/filepanel.html> | Árbol virtual, marcas de estado, carpetas virtuales, flujos |
| Manual de DMDE: *Virtual Reconstruction* | <https://dmde.com/manual/reconstruction.html> | Más/menos resultados, fragmentos por fiabilidad |
| Manual de DMDE: *Removed Data Recovery* | <https://dmde.com/manual/removedrecovery.html> | Sobrescritura, fiabilidad de borrados por FS |
| Manual de DMDE: *Working with Failing Drives* | <https://dmde.com/manual/badrecovery.html> | Discos con errores, clonar antes de recuperar |
| Manual de DMDE: *Cluster Map* | <https://dmde.com/manual/clustermap.html> | Informe de clústeres por fichero |
| Manual de DMDE: *Editions* | <https://dmde.com/manual/editions.html> | Funciones de recuperación por edición |
| Manual de DMDE: *Tools* y *Program Settings* | <https://dmde.com/manual/menu_tools.html>, <https://dmde.com/manual/inifile.html> | Entradas de menú; ajustes (sin detalle de recuperación) |
| Microsoft, *Naming Files, Paths, and Namespaces* | <https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file> | Caracteres y nombres reservados, espacio o punto final, `\\?\`, `\\.\` |
| Microsoft, *Maximum Path Length Limitation* | <https://learn.microsoft.com/en-us/windows/win32/fileio/maximum-file-path-limitation> | `MAX_PATH`, rutas extendidas, `longPathAware` |
| Microsoft, *File System Functionality Comparison* | <https://learn.microsoft.com/en-us/windows/win32/fileio/filesystem-functionality-comparison> | Límites de tamaño, flujos, marcas de tiempo por FS |
| Microsoft, *File Streams (Local File Systems)* | <https://learn.microsoft.com/en-us/windows/win32/fileio/file-streams> | Sintaxis de flujos, `$EFS` en `$LOGGED_UTILITY_STREAM`, tiempos por fichero |
| Microsoft, *SetFileTime* | <https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-setfiletime> | Fijar tiempos, resolución en FAT |
| Microsoft, *FILE_BASIC_INFO* | <https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_basic_info> | `ChangeTime` y atributos con `SetFileInformationByHandle` |
| Microsoft, *ReadEncryptedFileRaw* | <https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-readencryptedfileraw> | Exportación en bruto de ficheros EFS |
| Microsoft, [MS-EFSR] §2.2.3 *EFSRPC Raw Data Format* | <https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-efsr/e67520ea-0cb6-4349-bbe5-427a644d9492> | Formato en bruto de EFS |
| Rust, `std::fs::rename` | <https://doc.rust-lang.org/std/fs/fn.rename.html> | Semántica de sustitución en Windows |
| The Sleuth Kit, `tsk_recover` | <https://www.sleuthkit.org/sleuthkit/man/tsk_recover.html> | Práctica general: categorías asignado / no asignado |
| GNU ddrescue, manual | <https://www.gnu.org/software/ddrescue/manual/ddrescue_manual.html> | Práctica general: mapa de bloques, relleno marcado, reanudación |
| PhotoRec, *Step By Step* | <https://www.cgsecurity.org/wiki/PhotoRec_Step_By_Step> | Práctica general: carpetas de salida numeradas |
| Documentos del repositorio | `ROADMAP.md` (M4), `PARIDAD_DMDE_PRO.md` (RC-01 a RC-13, FS-05), `DECISIONES.md` (ADR-001, ADR-006, ADR-014, ADR-019, ADR-021, ADR-025, ADR-026), `ARQUITECTURA.md` §3.4 y §4, `formatos/ntfs.md`, `formatos/mapa-de-validez.md`, `PRUEBAS.md` §1.2 y §3 | Restricciones y piezas existentes |
| Código del repositorio | `core/mr-api/src/lib.rs` (`check_output_path`, `create_output`, `OutputFile`), `core/mr-fs-ntfs/src/stream.rs` (`RangeState`, `CopyReport`), `core/mr-fs-ntfs/src/values.rs` (`StandardInformation`, `FileName`) | Piezas reutilizables |

En esta sesión se recuperaron y leyeron todas las páginas de la tabla. Las páginas del manual de DMDE
se citan por su sección; las afirmaciones «por ausencia» indican que la página no trata el tema.

## 2. Lo que dice el manual de DMDE (comportamiento visible)

### 2.1 Flujo general y destino

| # | Afirmación | Etiqueta y fuente |
|---|---|---|
| D1 | El flujo es: abrir el volumen, si hace falta reconstruir (escaneo rápido o completo), ajustar «más/menos» resultados, marcar elementos y recuperarlos **a otro disco**; si casi todo está dañado, probar otras versiones del volumen. | OBS: datarecovery.html, *Data Recovery Steps* |
| D2 | Se recomienda recuperar siempre a otro disco físico; a otra partición del mismo dispositivo solo es seguro si no se solapan y el dispositivo no tiene problemas físicos. No se debe escribir en el disco de origen. | OBS: datarecovery.html, notas finales del diálogo |
| D3 | El botón «Size» calcula el total a recuperar y se desaconseja en discos con sectores malos porque añade carga al dispositivo. | OBS: datarecovery.html, *Button "Size"* |
| D4 | Con un disco que falla, se recomienda clonar primero y trabajar sobre la copia; si no es posible, recuperar primero lo más importante, guardar los resultados del escaneo y comprobar que lo recuperado se abre. | OBS: badrecovery.html, *Device Cloning*, *If Cloning is Impossible* |
| D5 | La edición gratuita limita el número de ficheros por operación y no recupera directorios anidados; la Professional no tiene límite y añade listas HTML, informe con sumas, flujos alternativos NTFS. | OBS: editions.html; datarecovery.html, nota de ediciones |

### 2.2 Árbol virtual, categorías y estados

| # | Afirmación | Etiqueta y fuente |
|---|---|---|
| D6 | El panel de ficheros muestra una estructura virtual de directorios; los nombres virtuales van entre corchetes o empiezan por «$» y suelen estar en el nivel superior; la carpeta raíz virtual corresponde aproximadamente a la raíz del volumen. | OBS: filepanel.html, introducción |
| D7 | Los ficheros encontrados y borrados aparecen tras la reconstrucción virtual. | OBS: filepanel.html; reconstruction.html |
| D8 | Hay carpetas virtuales para ficheros especiales del FS, ficheros sin nombre ni directorio padre, y ficheros encontrados por firmas (RAW), y un directorio virtual que agrupa varios registros de un mismo fichero. | OBS: filepanel.html, tabla de iconos |
| D9 | Las marcas distinguen: normal, borrado, encontrado, encontrado y borrado; directorios con contenido no del todo conocido; directorios que contienen borrados o encontrados; directorio borrado con ficheros no borrados («posible error del FS»); fichero normal que también se encontró por firma o que **no** se encontró por firma en su sitio («posiblemente dañado»); RAW con o sin firma de final, o con tamaño detectado «probablemente fragmentado o parcialmente sobrescrito». | OBS: filepanel.html, *File icons*, *Directory icons* |
| D10 | Atributos mostrados: cifrado, comprimido, disperso, directorio, solo lectura, oculto, sistema, archivo. | OBS: filepanel.html, atributos |
| D11 | Los flujos alternativos NTFS se muestran como `Fichero:Flujo` tras la reconstrucción. | OBS: filepanel.html |
| D12 | El diálogo de recuperación selecciona **categorías** de ficheros remitiendo a las marcas del panel; no enumera las categorías. | OBS: datarecovery.html, *File Categories* |
| D13 | Los fragmentos de un escaneo se agrupan por probabilidad de pertenecer al volumen y los resultados adicionales son «de menor calidad». | OBS: reconstruction.html, *Results Control*, *Volume FS Fragments* |
| D14 | Hay una opción para mejorar la fusión de índices usando marcas de tiempo y evitar mezclar ramas por error. | OBS: reconstruction.html, opciones avanzadas |

### 2.3 Filtros

| # | Afirmación | Etiqueta y fuente |
|---|---|---|
| D15 | Filtros por máscara de nombre, tamaño, fecha de modificación e identificador; un 0 como segundo valor deja abierto el límite superior de tamaño o identificador. | OBS: datarecovery.html, *Filters* |
| D16 | Máscaras separadas por «;», con `*` y `?`; una barra invertida inicial marca una máscara de exclusión; las máscaras que van primero tienen prioridad. | OBS: datarecovery.html, *Filters* |
| D17 | Opción para procesar solo los directorios que contienen ficheros que cumplen el filtro. | OBS: datarecovery.html, *Filters* |
| D18 | Sin una opción concreta, los elementos excluidos por el filtro pierden su marca de selección; con ella la conservan. | OBS: datarecovery.html, *File Categories* y *Filters* |

### 2.4 Nombres, rutas largas y división

| # | Afirmación | Etiqueta y fuente |
|---|---|---|
| D19 | Nombres Unicode activos por defecto donde el sistema lo admite; sin ellos, los caracteres fuera de la página de códigos se transliteran o sustituyen. | OBS: datarecovery.html, *Unicode names* |
| D20 | En DOS se pueden sustituir los nombres largos y generar un lote que los restaura después en Windows. | OBS: datarecovery.html, *Substitute names* |
| D21 | Opción para rutas de más de 259 caracteres en Windows NT y posteriores; alternativamente, poner a mano el prefijo de ruta extendida en el destino. Esas rutas pueden ser difíciles de abrir en el explorador. | OBS: datarecovery.html, *Paths longer than 259 symbols* |
| D22 | FAT no admite ficheros de más de 4 GB (a veces 2 GB); la opción de dividir parte los ficheros grandes en trozos que se unen después con una herramienta del sistema. | OBS: datarecovery.html, *Split large files* |

### 2.5 Duplicados, errores y otros eventos

| # | Afirmación | Etiqueta y fuente |
|---|---|---|
| D23 | Los nombres duplicados pueden venir de versiones del fichero, enlaces, fusiones incorrectas de directorios, destinos que no distinguen mayúsculas o ficheros ya existentes. | OBS: datarecovery.html, *Duplicate Names, Errors in Files, and Other Events Handling* |
| D24 | Acciones ante duplicados: renombrar, renombrar automáticamente, omitir (todos), fusionar (todos, para directorios). Hay un **umbral** de duplicados del mismo nombre: al superarlo se vuelve a preguntar. | OBS: ídem |
| D25 | Errores en ficheros (de E/S o del FS): **omitir** (el fichero queda marcado tras la recuperación), **ignorar** (seguir con errores), **ignorar y ocultar** (atributo oculto en Windows, permisos cero en Linux y macOS), **mover a una carpeta de malos** en el destino. | OBS: ídem, *Errors in files* |
| D26 | Los manejadores se pueden fijar antes o durante la recuperación; también se puede cambiar de destino o dividir el fichero si se acaba el espacio. | OBS: ídem |
| D27 | La página no dice cuándo se pregunta al usuario por un error de lectura, ni qué contiene el fichero en los sectores ilegibles (relleno o ceros). | OBS (por ausencia): datarecovery.html, badrecovery.html |

### 2.6 Marcas de tiempo, atributos, flujos y EFS

| # | Afirmación | Etiqueta y fuente |
|---|---|---|
| D28 | El manual no describe si se restauran las fechas, los atributos, el propietario o los permisos; los atributos solo aparecen en «ignorar y ocultar». | OBS (por ausencia): datarecovery.html |
| D29 | Casilla para incluir los flujos alternativos NTFS (Professional). | OBS: datarecovery.html, *Include NTFS altstreams* |
| D30 | Los ficheros NTFS cifrados se recuperan **sin descifrar**; para abrirlos hace falta el certificado del sistema de origen. Tres modos: flujos separados (datos y claves en dos ficheros), copia de seguridad en un formato portátil propio que se restaura después con una herramienta del programa, o directamente a un fichero cifrado en una plataforma con EFS. | OBS: datarecovery.html, sección de cifrados |

### 2.7 Listas e informes

| # | Afirmación | Etiqueta y fuente |
|---|---|---|
| D31 | «List» crea una lista de los ficheros seleccionados con sus atributos; hay variante HTML (Professional). | OBS: datarecovery.html, *Button "List"* |
| D32 | «Sector List» lista los sectores que ocupan los ficheros seleccionados, para que otro programa haga un clonado parcial; límites de fragmento en sectores o en bytes; rutas opcionales; se puede limitar a fragmentos dentro de rangos dados (p. ej. para saber qué ficheros caen en sectores malos). | OBS: datarecovery.html, *Sector List* |
| D33 | El informe de clústeres funciona en los dos sentidos: ficheros en unos sectores dados, o clústeres de unos ficheros; puede dar límites como desplazamientos en bytes desde el inicio del disco. | OBS: clustermap.html, *Cluster Map Report* |
| D34 | «Create Report» guarda un informe que lista ficheros y directorios, informa de los errores de E/S por fichero y calcula CRC-32, MD5, SHA-1 y SHA-256 con fines forenses (Professional). | OBS: datarecovery.html, *Create Report* |
| D35 | No se documenta el formato del informe (texto, columnas), si las sumas se calculan sobre el fichero escrito o sobre los datos leídos, ni qué se hace con las sumas de un fichero con errores. | OBS (por ausencia): datarecovery.html |
| D36 | No se documenta si una recuperación interrumpida se puede reanudar. | OBS (por ausencia): datarecovery.html, badrecovery.html |

### 2.8 Borrados y sobrescritura

| # | Afirmación | Etiqueta y fuente |
|---|---|---|
| D37 | Lo sobrescrito no es recuperable; no se puede predecir si un dato ha sido sobrescrito, así que se intenta de todas formas. TRIM hace ilegibles los bloques liberados. | OBS: removedrecovery.html, *Overwritten Is Not Recoverable* |
| D38 | La recuperación basada en el FS conserva nombres y rutas y tiene en cuenta la fragmentación; la RAW no. En NTFS la disponibilidad de borrados es alta, aunque el controlador de Windows puede dejar a tamaño cero ficheros de más de 2 GB. | OBS: removedrecovery.html, *Removed Data Recovery Based on a File System* |

## 3. Práctica general de otras herramientas y documentación de plataforma

| # | Afirmación | Etiqueta y fuente |
|---|---|---|
| G1 | `tsk_recover` exporta por defecto solo los ficheros no asignados; `-a` solo los asignados y `-e` todos; puede limitarse a un directorio por su inodo. Es decir, la categoría (asignado / no asignado) es el filtro principal. | SPEC: tsk_recover, opciones |
| G2 | ddrescue mantiene un fichero de mapa con el estado de cada bloque (no intentado, fallido sin recortar, fallido sin raspar, sector malo, terminado) que permite reanudar; su «modo relleno» escribe una cadena reconocible en los bloques malos de la copia para encontrar después qué ficheros caen en ellos. | SPEC: ddrescue manual, *Mapfile structure*, *Fill mode* |
| G3 | PhotoRec escribe en subcarpetas numeradas que se pueden consultar antes de terminar. | SPEC: PhotoRec *Step By Step* |
| G4 | Windows prohíbe en nombres `< > : " / \ | ? *`, el carácter 0 y los códigos 1 a 31 (salvo en flujos alternativos), y reserva `CON`, `PRN`, `AUX`, `NUL`, `COM0-9`, `LPT0-9` y sus variantes con superíndices, también seguidos de extensión. Recomienda no terminar un nombre en espacio o punto. | SPEC: naming-a-file, *Naming Conventions* |
| G5 | No se debe suponer distinción de mayúsculas; NTFS la admite como opción, no por defecto. | SPEC: naming-a-file, *Naming Conventions*; filesystem-functionality-comparison, *Functionality* |
| G6 | `MAX_PATH` = 260 incluyendo unidad, «:\» y el nulo final; con `\\?\` el límite es de unos 32 767 caracteres, la ruta debe ser absoluta, sin `/`, `.` ni `..`, y cada componente está limitado (normalmente 255). Desde Windows 10 1607 se puede quitar el límite si el registro **y** el manifiesto (`longPathAware`) lo activan. Un directorio no puede superar `MAX_PATH − 12` al crearlo sin `\\?\`. El explorador puede no manejar esas rutas. | SPEC: maximum-file-path-limitation |
| G7 | `\\?\` desactiva la normalización y se envía la cadena tal cual al FS; `\\.\` accede al espacio de dispositivos (discos, volúmenes). | SPEC: naming-a-file, *Win32 File Namespaces*, *Win32 Device Namespaces* |
| G8 | Tamaño máximo de fichero: FAT32 4 GiB; NTFS y exFAT 2^64 − 1. Flujos alternativos: NTFS y UDF sí; exFAT y FAT32 no. | SPEC: filesystem-functionality-comparison, *Limits*, *Functionality* |
| G9 | Los tiempos son del fichero, no de cada flujo; escribir cualquier flujo actualiza los tiempos del fichero. | SPEC: file-streams |
| G10 | `SetFileTime` fija creación, último acceso y última escritura (con `FILE_WRITE_ATTRIBUTES`); en FAT la creación tiene resolución de 10 ms, la escritura de 2 s y el acceso de 1 día; NTFS retrasa hasta una hora la actualización del acceso. | SPEC: SetFileTime, *Remarks* |
| G11 | `FILE_BASIC_INFO` (con `SetFileInformationByHandle`) incluye además `ChangeTime` (cambios de metadatos) y los atributos; 0 en atributos no los cambia. | SPEC: FILE_BASIC_INFO |
| G12 | `ReadEncryptedFileRaw` exporta un fichero EFS sin descifrar a partir de un fichero abierto con `OpenEncryptedFileRaw` (por tanto, en un volumen montado), y `WriteEncryptedFileRaw` lo importa. | SPEC: ReadEncryptedFileRaw, *Remarks* |
| G13 | El formato en bruto de EFS tiene cabecera, flujo de metadatos EFS y flujos adicionales, pero sus detalles «dependen de la implementación» y el cliente no debe analizarlo ni depender de su estructura. | SPEC: [MS-EFSR] §2.2.3 |
| G14 | Los metadatos EFS de NTFS están en el flujo `:$EFS:$LOGGED_UTILITY_STREAM`. | SPEC: file-streams, *Stream Types* |
| G15 | `std::fs::rename` en Windows sustituye el destino si existe. | SPEC: documentación de `std::fs::rename` |

## 4. Lo que ya existe en el repositorio

- REPO (`mr-api`, `check_output_path`): rechaza una salida que sea ruta de dispositivo (lista blanca de
  ADR-014), un directorio o el propio origen o uno de sus segmentos (comparación de rutas canónicas sin
  distinguir mayúsculas). No comprueba el disco físico (no hace falta mientras solo hay imágenes).
- REPO (`mr-api`, `create_output` y `OutputFile`): escribe en un temporal nuevo
  (`.<nombre>.<pid>-<n>.mrtmp`) del mismo directorio, sincroniza y renombra al confirmar; el temporal se
  borra si no se confirma (error, cancelación, pánico). Sin `overwrite` vuelve a comprobar antes de
  renombrar que el destino no existe.
- INF (de G15 y del código): entre esa comprobación y `std::fs::rename` hay una ventana en la que otro
  proceso puede crear el destino, que `rename` sustituiría. Para M4, con miles de ficheros, conviene un
  renombrado **sin sustitución** (`MoveFileExW` sin `MOVEFILE_REPLACE_EXISTING`, o enlace duro y borrado
  del temporal) que falle de forma atómica si el nombre ya existe (§7.4).
- REPO (ADR-026, `formatos/ntfs.md`): la lectura de flujos entrega el estado de cada tramo (`data`,
  `sparse`, `uninitialized` fiables; `unreadable`, `unmapped`, `outside_source`, `corrupt` no fiables,
  con patrón `BAD!` alineado al flujo); `copy_to` copia en trozos de 1 MiB y da recuentos exactos por
  estado y hasta 10 000 tramos no fiables con su LBA.
- REPO (ADR-019, ADR-021): los hashes del origen son estrictos (nunca sobre relleno); la CLI termina con
  código 3 si hubo bytes no fiables; patrones de relleno configurables con reglas de admisión.
- REPO (`values.rs`): `$STANDARD_INFORMATION` (cuatro tiempos, atributos DOS, `security_id`, `owner_id`)
  y `$FILE_NAME` (padre, tiempos, espacio de nombres) ya se interpretan. El nombre se decodifica con
  sustitución de UTF-16 no válido (`utf16_lossy`): dos nombres distintos con suplentes sueltos pueden
  acabar iguales y el original no se puede reproducir en el destino (INF; §7.6).
- REPO (`ARQUITECTURA.md` §4): los eventos que requieren decisión (conflicto de nombre, error con
  política «preguntar») se publican como **peticiones** que la UI o la CLI responden; con política
  automática no bloquean. Lectura secuencial por dispositivo.
- REPO (issues #22 y #23, abiertos): árbol por índices `$I30` y detección de borrados (registros no en
  uso y entradas INDX residuales). `mr-vfs` depende de ambos.

## 5. Inferencias sobre la recuperación de DMDE

| # | Inferencia | Base |
|---|---|---|
| I1 | La recuperación copia la vista **virtual**, no el disco: lo que se selecciona son nodos del árbol reconstruido (incluidas las carpetas virtuales), y la ruta de destino reproduce esa vista. | INF: D6, D8, D12 |
| I2 | Las categorías del diálogo son las clases de marcas del panel (normal, borrado, encontrado, RAW y sus combinaciones). | INF: D9, D12 |
| I3 | «Ignorar» escribe el fichero completo aunque haya sectores ilegibles; qué bytes pone en su lugar no está documentado, y el informe es el único sitio donde queda constancia de los errores. | INF: D25, D27, D34; DESC sobre el relleno |
| I4 | Los duplicados por versiones o por destinos sin distinción de mayúsculas son frecuentes en recuperaciones de borrados, por eso hay umbral para no preguntar sin fin. | INF: D23, D24 |
| I5 | El cálculo del tamaño lee datos o estructuras del dispositivo (por eso se desaconseja con sectores malos). | INF: D3 |
| I6 | La lista de sectores está pensada para encadenar con un clonador (clonar solo lo que ocupan los ficheros importantes) y para el camino inverso (qué ficheros caen en sectores malos). | INF: D32, D33, G2 |
| I7 | Para EFS desde una imagen, «flujos separados» es la única vía que no necesita la API de Windows: `ReadEncryptedFileRaw` exige el fichero en un volumen montado y su formato no está garantizado. | INF: D30, G12, G13 |

## 6. Desconocido

- DESC: qué bytes escribe DMDE en las zonas ilegibles de un fichero «ignorado» y si las distingue de
  datos en el propio fichero.
- DESC: si las sumas del informe se calculan sobre lo escrito, sobre lo leído o se omiten en ficheros con
  errores; el formato del informe y de las listas (columnas, codificación).
- DESC: si se restauran tiempos, atributos, propietario o permisos, y cuáles.
- DESC: cómo se forma un nombre en «renombrar automáticamente»; cómo se cuenta el umbral (por nombre,
  por directorio o global).
- DESC: si hay reanudación de una recuperación interrumpida.
- DESC: el nombre y la estructura de la carpeta de malos en el destino, y si conserva la ruta relativa.
- DESC: cómo se escriben los flujos alternativos en un destino que no los admite.
- DESC: si la fiabilidad de un borrado (clústeres ya reutilizados) se calcula y se muestra por fichero.

Ninguna incógnita bloquea M4: §7 a §9 definen un comportamiento propio, completo y comprobable.

## 7. Diseño propio para M4

### 7.1 Principios (invariantes comprobables)

1. **El origen nunca se escribe** (ADR-006). La salida nunca es el origen, uno de sus segmentos, una ruta
   de dispositivo ni un directorio dentro de una ruta `\\.\` (ADR-014, `check_output_path`). Con
   dispositivos (M9), además, el destino no puede estar en el mismo disco físico que el origen salvo
   anulación explícita (ADR-006, RC-04).
2. **Nada no fiable se presenta como íntegro** (ADR-019, ADR-026). Cada byte escrito tiene un estado; un
   fichero solo es «íntegro» si todos sus bytes son fiables **y** su procedencia no está en duda (§7.3).
   El informe y el código de salida lo reflejan siempre.
3. **Nunca se sustituye un fichero existente** sin una elección explícita por operación (ni por defecto ni
   por umbral). La sustitución, si se elige, también es atómica.
4. **Ningún nombre puede salir del directorio de destino**: los nombres del origen son datos, no rutas
   (`..`, separadores, unidades, prefijos `\\?\` o `\\.\`, flujos con «:» se neutralizan, §7.6).
5. **Atomicidad por fichero**: un fichero con su nombre final está completo según su estado declarado;
   nunca queda un parcial con el nombre final (cancelación, error, corte de luz tras `sync`).
6. **Memoria acotada e independiente del tamaño de los ficheros**: trozos de 1 MiB, listas de tramos
   acotadas con recuentos exactos, informes escritos en flujo.
7. **Determinismo**: mismo origen, misma selección y mismas opciones producen los mismos nombres de
   salida, el mismo contenido y el mismo informe (salvo fechas de ejecución y rutas absolutas).
8. **Cancelable con progreso**, y el resultado parcial es válido y lo dice.

### 7.2 `mr-vfs` (#25): árbol virtual con estados

**Capa.** Crate genérico sobre los lectores de FS (hoy `mr-fs-ntfs`). Un FS aporta un proveedor con
entradas, flujos y comprobación de asignación; `mr-vfs` las fusiona y no conoce NTFS.

**Nodo** (arena de `u32`, sin punteros; nombres en un búfer compartido):

| Campo | Contenido |
|---|---|
| `id`, `padre` | Índices en la arena; la raíz es 0 |
| `nombre` | UTF-16 **original** (`Vec<u16>` en el búfer) y forma visible (UTF-8 con sustitución); el destino usa el original (§7.6) |
| `tipo` | Fichero, directorio, carpeta sintética |
| `categoría` | `vivo`, `borrado`, `huérfano`, `metadatos`; reservadas: `encontrado` (M6), `raw` (M7) |
| `origen` | Referencia en el FS (registro MFT y secuencia; en M6, fragmento) y espacio de nombres del nombre elegido |
| `fiab_nombre` | `estructura` (índice vivo o `$FILE_NAME` de registro en uso), `residual` (entrada INDX residual), `registro` (solo `$FILE_NAME` de un registro no en uso), `sintético` |
| `fiab_datos` | Resumen de §7.3, calculado bajo demanda y guardado en caché |
| `tamaños`, `tiempos`, `atributos` | De `$STANDARD_INFORMATION` (y de `$FILE_NAME` como alternativa informada) |
| `flujos` | Principal y alternativos con su tamaño y marcas (disperso, comprimido, cifrado) |
| `anomalías` | Lista corta: padre reutilizado, nombre duplicado en el directorio, ciclo cortado, registro dañado... |

**Fusión (NTFS, M4).**

1. Vivos: árbol de #22 (índices `$I30`), con enlaces duros como varias entradas que comparten registro.
2. Borrados: registros no en uso de #23 colocados bajo su padre si el padre existe con la **misma
   secuencia** que cita `$FILE_NAME`; un directorio borrado intacto aparece como directorio borrado con
   sus hijos.
3. Huérfanos: padre inexistente, ilegible o reutilizado (secuencia distinta) → carpeta sintética
   «Huérfanos», con la ruta parcial que se pueda reconstruir como subcarpetas.
4. Metadatos: `$MFT`, `$LogFile`, `$Bitmap`... en la carpeta sintética «Metadatos» (recuperables como
   cualquier fichero, no seleccionados por defecto).
5. Registros dañados (ADR-025: `damaged`, `unreadable`) que un índice vivo cita: entrada visible con
   categoría `vivo`, `fiab_datos = no_disponible` y anomalía; nunca se inventa contenido.
6. Ciclos y profundidad: recorrido con conjunto de visitados y profundidad máxima (p. ej. 4096); un ciclo
   se corta y se anota.

**Carpetas sintéticas.** Nombres propios de MemRescue, distintos de los de la referencia (ADR-001) y
válidos como nombres de Windows para poder recuperarlas tal cual: se proponen `[Raiz]`, `[Huerfanos]`,
`[Metadatos]`, `[Sin nombre]` y, en hitos posteriores, `[Encontrados]` y `[RAW]` (pregunta abierta 2).

**Escala.** Arena compacta (objetivo ≤ 128 B por nodo más nombres) que admite el árbol de un volumen con
millones de registros MFT en memoria; límite configurable con error tipado al superarlo (M6 podrá mover
la arena a la base SQLite de ADR-016). Hijos ordenados de forma estable (nombre sin distinción de
mayúsculas y, a igualdad, categoría y registro) para que las rutas de salida sean deterministas.

**Exposición.** API Rust (`mr_api::vfs_*`: abrir, listar hijos paginados, buscar por ruta o registro,
estadísticas por categoría), ABI (`mr_vfs_*` con iteradores paginados, ADR-013) y CLI (`tree` con filtro
por categoría y estado).

### 7.3 Estados de los datos por tramo y por fichero

Se amplía el estado por tramo de ADR-026 con la **procedencia** de los clústeres de un fichero borrado,
que el estado de lectura no puede ver:

| Estado de tramo | Origen | ¿Fiable? |
|---|---|---|
| `data`, `sparse`, `uninitialized` | ADR-026 | sí |
| `unreadable`, `unmapped`, `outside_source`, `corrupt` | ADR-026 | no |
| `reasignado` (nuevo) | Fichero borrado cuyos clústeres están hoy asignados (en `$Bitmap` o en los runs de un fichero vivo) | **no**: casi seguro contenido de otro fichero |

El cálculo es por runs (rangos de LCN) contra el mapa de asignación, sin leer datos: barato y válido para
el botón de tamaño (D3, I5). El mapa de `$Bitmap` (1 bit por clúster) se consulta por ventanas acotadas.

**Resumen por fichero (`fiab_datos`):**

| Valor | Significado | ¿Se presenta como íntegro? |
|---|---|---|
| `integro` | Vivo; todos los tramos fiables | Sí |
| `integro_no_verificable` | Borrado; todos los tramos fiables y sus clústeres libres. Pueden haberse reutilizado y liberado después: nada lo prueba (D37) | No: «sin errores de lectura, procedencia no verificable» |
| `parcial` | Algún tramo no fiable (cualquier estado no fiable, incluido `reasignado`) | No |
| `sobrescrito` | Todos los tramos con clústeres son `reasignado` | No |
| `sin_datos` | No hay runs o el tamaño es cero con tamaño original distinto de cero (D38) | No |
| `no_disponible` | Registro dañado o ilegible, o flujo con error tipado (`stream_*` de ADR-026) | No |
| `cifrado` | Flujo EFS: solo exportación en bruto (§7.9) | No (no es el contenido) |

### 7.4 `mr-recover` (#26): copia

**Entrada.** Selección (nodos marcados, con o sin descendientes), categorías, filtros (§7.5), destino y
políticas (§7.6 a §7.8). **Salida.** Árbol en el destino, informe (§7.10) y resultado tipado.

**Orden.** Recorrido en profundidad en el orden estable de §7.2; directorios creados antes de sus hijos;
tiempos y atributos de directorios aplicados al final en postorden (crear un hijo cambia los tiempos del
padre). Los ficheros se copian en el orden del recorrido; ordenar por LBA para discos mecánicos queda como
opción (pregunta abierta 7).

**Escritura de un fichero.**

1. Calcular la ruta de destino (§7.6) y resolver conflictos (§7.7) **antes** de leer datos.
2. `create_output` en el directorio final (temporal único, nunca el nombre final).
3. Copiar cada flujo con `copy_to` (1 MiB por trozo): los tramos no fiables llevan el patrón de relleno
   de ADR-021, alineado al flujo; a la vez se calculan los hashes (§7.10) y se registran los tramos con su
   LBA (hasta 10 000 por fichero, recuentos exactos).
4. Aplicar la política de errores (§7.8) cuando el fichero no es `integro`.
5. Flujos alternativos (§7.9) sobre el temporal antes de confirmar.
6. Confirmar: `sync`, renombrado **sin sustitución** (falla si el nombre apareció; entonces se vuelve al
   paso 1 con la política de conflictos), tiempos y atributos (§7.9).
7. Escribir la línea del fichero en el diario (§7.10) y vaciarlo.

**Destino.** Se comprueba una vez al empezar y se repite por directorio creado:

- `check_output_path` sobre la **raíz** de destino y comprobación de que cada ruta creada, canónica,
  sigue dentro de la raíz (ningún componente es un punto de reanálisis creado por otro proceso: se abre
  con `FILE_FLAG_OPEN_REPARSE_POINT` y se verifica, como en ADR-014).
- Información del volumen de destino (`GetVolumeInformationW`): sistema de archivos, longitud máxima de
  componente, si admite flujos con nombre, si distingue mayúsculas. Con ella se eligen por defecto la
  división (§7.6), el tratamiento de flujos (§7.9) y la comparación de nombres (§7.7).
- Espacio libre: aviso previo si el total estimado no cabe; al agotarse durante la copia, el fichero en
  curso se descarta (temporal borrado) y se emite una **petición** con opciones: otro destino, dividir,
  omitir el fichero, cancelar (D26). Con política automática: cancelar con resultado parcial.

**Progreso y cancelación.** Tarea de `mr-api` con total en bytes calculado de los metadatos (sin leer
datos), ficheros hechos/total, ruta en curso, bytes no fiables acumulados y ficheros por estado.
Cancelación comprobada entre trozos y entre ficheros; el fichero en curso no se confirma (el temporal se
borra), el informe se cierra con estado `cancelada` y lista lo ya escrito.

**Reanudación.** M4 no reanuda a mitad de fichero. Un diario por recuperación (§7.10) permite repetirla
con la política de conflictos `omitir_si_identico`: los ficheros ya escritos con el mismo tamaño y
SHA-256 se saltan sin escribir. Una reanudación real con estado en la base de trabajo (ADR-016) queda
para M6 si hace falta (pregunta abierta 6).

**Memoria.** Constante por fichero (trozo de 1 MiB, hashes incrementales, lista de tramos acotada), lineal
en la profundidad del árbol para los tiempos pendientes de directorios, y lineal en el número de nombres
**de un mismo directorio** para detectar colisiones entre entradas de la propia recuperación (con un
límite y error tipado si un directorio supera, p. ej., 10 millones de entradas).

**Paralelismo.** Una sola lectura secuencial del origen (ARQUITECTURA §4); los hashes se calculan en el
mismo hilo o en uno auxiliar con cola acotada.

### 7.5 Filtros y categorías

- **Categorías**: `vivo`, `borrado`, `huérfano`, `metadatos` (y `encontrado`, `raw` cuando existan);
  por defecto, todas salvo `metadatos`. Filtro adicional por `fiab_datos` (p. ej. «solo íntegros»).
- **Reglas de nombre** en una lista ordenada; **gana la primera que coincide** (como D16); cada regla es
  de inclusión o de exclusión (prefijo `!` en la CLI, para no reutilizar la barra invertida, que en Windows
  es separador); comodines `*` y `?`; sin distinción de mayúsculas por defecto; una regla con `/` se
  compara con la ruta relativa desde la selección, si no, con el nombre. Sin reglas, todo incluido; con
  reglas y ninguna coincidencia, excluido.
- **Rangos** cerrados u opcionales en ambos extremos (en lugar del «0 = sin límite» de D15): tamaño,
  fechas (escritura por defecto; se elige cuál de los tiempos), número de registro.
- **Directorios**: un directorio se recorre siempre; con «solo directorios con ficheros» (D17, por defecto
  activado) solo se crea si contiene algún fichero que pasa los filtros; sin esa opción se crean también
  los vacíos.
- El filtro no cambia la selección guardada (equivalente a conservar marcas, D18); la UI muestra cuántos
  elementos excluye.

### 7.6 Nombres, rutas largas y división

**Nombres.** Del nombre original UTF-16 al destino Windows:

| Caso | Tratamiento |
|---|---|
| Caracteres prohibidos `< > : " / \ | ? *` y códigos 0 a 31 (G4) | Sustituidos por `%XX` (hexadecimal del código UTF-16) y `%` se escapa como `%25` **solo** en nombres que necesitan escape; nombre original y escapado en el informe |
| Nombres reservados `CON`, `NUL`, `COM1`... con o sin extensión (G4) | Sufijo `_` tras la base (`NUL_.txt`) |
| Espacio o punto final (G4) | El último carácter se escapa (`%20`, `%2E`) |
| `.` y `..` como nombre completo, o nombre vacío | Se escapan; nunca se interpretan como ruta |
| Suplentes UTF-16 sueltos | Se conservan con la API ancha (`OsString::from_wide`); si el destino los rechaza, `%XXXX` |
| Nombre de más de la longitud máxima de componente del destino | Se recorta conservando la extensión y se añade `~<n>` determinista; original en el informe |

El nombre elegido es el `$FILE_NAME` Win32 (o POSIX); el DOS 8.3 se ignora salvo que sea el único.

**Rutas largas.** Siempre se construye la ruta absoluta con `\\?\` (o `\\?\UNC\`) en las llamadas de E/S
(G6, G7), sin depender de `LongPathsEnabled`. Como el explorador puede no abrirlas (D21, G6), el informe
marca los ficheros cuya ruta supera 259 caracteres y la CLI avisa al final con su número.

**División (D22, G8).** Política `auto` por defecto: si el destino es FAT/FAT32 y el flujo supera
4 GiB − 1, se divide en partes `nombre.ext.001`, `.002`... de 4 GiB − 1 MiB como máximo (o el tamaño
indicado), cada una atómica; un fichero de partes solo es `integro` si todas lo son. El informe da el hash
del fichero completo (calculado en la misma pasada) y el de cada parte, de forma que la unión se pueda
verificar. Política `nunca`: error tipado para ese fichero. Política `tamaño N`: divide siempre por encima
de N.

### 7.7 Conflictos de nombre

Un conflicto es: el nombre ya existe en el destino, o dos entradas de la recuperación producen el mismo
nombre (mayúsculas en un destino que no las distingue, versión viva y borrada, nombres que el escape hace
iguales, enlaces duros).

| Política | Fichero | Directorio |
|---|---|---|
| `preguntar` (por defecto en la UI) | Petición con las demás opciones y «aplicar a todos» | Ídem |
| `renombrar_auto` (por defecto en la CLI) | `nombre (2).ext`, `(3)`...; el primero en el orden estable conserva el nombre | `nombre (2)` |
| `omitir` | No se escribe; queda en el informe como `omitido_conflicto` | No se crea; su contenido se omite |
| `omitir_si_identico` | Se omite si el existente tiene el mismo tamaño y SHA-256; si no, `renombrar_auto` | `fusionar` |
| `fusionar` | No aplica | Se reutiliza el directorio existente; los hijos se resuelven uno a uno |
| `sobrescribir` | Solo con confirmación explícita por operación (CLI: `--overwrite-existing` y confirmación); sustitución atómica | No aplica (los directorios se fusionan) |

**Umbral (D24).** Contador por (directorio de destino, nombre); al superar N (por defecto 10) renombrados
del mismo nombre se emite una petición o, en automático, se pasa a `omitir` y se anota. La versión borrada
puede llevar un sufijo distinto (`nombre (borrado 2).ext`) con una opción (pregunta abierta 3).

### 7.8 Errores y sectores ilegibles

Los ficheros que no son `integro` siguen una política (D25), siempre con informe por tramos:

| Política | Efecto |
|---|---|
| `escribir` (por defecto) | Se escribe con relleno en los tramos no fiables; queda como `parcial`, `sobrescrito`... en el informe |
| `escribir_y_ocultar` | Además, atributo oculto |
| `apartar` | Se escribe bajo `[No fiables]\<ruta relativa>` en el destino, conservando la estructura |
| `omitir` | No se escribe; queda listado con sus tramos |
| `preguntar` | Petición por fichero con «aplicar a todos» |

Reglas adicionales:

- Las políticas se pueden elegir **por estado** (p. ej. `parcial` → `escribir`, `sobrescrito` → `omitir`).
  Por defecto, `sobrescrito` y `sin_datos` → `omitir` (nada útil que escribir) y `integro_no_verificable`
  → `escribir` (pregunta abierta 1).
- Los tramos `reasignado` se escriben con relleno, **no** con los bytes actuales del disco, salvo con la
  opción explícita `--include-reallocated-bytes` (útil en análisis forense); con ella los tramos siguen
  marcados como no fiables.
- Los reintentos y saltos los decide la política de E/S de M1 (`MappedReadOptions`), no la recuperación;
  la recuperación nunca vuelve a leer un sector ya leído como ilegible en la misma ejecución (caché de
  sectores malos por LBA, acotada), para no castigar el disco (D4).
- Errores del destino (permiso, disco lleno, nombre rechazado) son otra clase: no se confunden con los del
  origen y no consumen la política de errores de lectura.
- Fichero lateral opcional `nombre.ext.mrbad` (desactivado por defecto) con los tramos no fiables del
  fichero, para quien recibe solo la carpeta sin el informe (pregunta abierta 4).

### 7.9 Tiempos, atributos, flujos y EFS

- **Tiempos** de `$STANDARD_INFORMATION`: creación, escritura y acceso con `SetFileTime` (G10);
  `ChangeTime` solo con opción y `SetFileInformationByHandle` (G11). Se aplican **después** de escribir
  todos los flujos (G9) y tras el renombrado; en directorios, en postorden al final. Con destino FAT se
  aplican con la pérdida de resolución que impone el FS (G10) y el informe guarda los valores originales
  con 100 ns. Opción para usar los de `$FILE_NAME`.
- **Atributos**: solo lectura, oculto, sistema, archivo, por defecto. No se reproducen comprimido, cifrado,
  disperso ni punto de reanálisis (son de almacenamiento, no del contenido); la salida dispersa
  (`FSCTL_SET_SPARSE` y huecos) es opcional para ahorrar espacio. Solo lectura se aplica al final (si no,
  impide fijar tiempos).
- **Propietario y permisos** (RC-09): no se aplican a la salida en M4 (serían de otra máquina); el
  informe guarda `owner_id` y `security_id` y, cuando se lea `$Secure`, el descriptor en SDDL (pregunta
  abierta 5).
- **Enlaces y puntos de reanálisis**: no se recrean nunca (ni enlaces simbólicos ni uniones); un enlace
  duro se recupera como copias independientes en cada ruta; un punto de reanálisis se recupera como
  fichero vacío o con sus datos si los tiene, y su destino como texto en el informe.
- **Flujos alternativos** (D29, RC-01): incluidos por defecto. En destino NTFS se escriben como flujos del
  fichero de salida (`nombre:flujo`, nombre del flujo con el escape de §7.6 salvo los códigos 1 a 31 que
  G4 admite). En destino sin flujos (G8) se escriben como ficheros hermanos `nombre.ext%3Aflujo` con
  la misma política de conflictos. Cada flujo tiene sus tramos y sus hashes en el informe.
- **EFS (FS-05)**: sin descifrar (D30). Desde una imagen no se puede usar `ReadEncryptedFileRaw` (exige el
  fichero montado, G12) y el formato en bruto no es estable (G13), así que M4 exporta: el `$DATA`
  cifrado tal como está en disco (`nombre.ext.efsdata`, con sus tramos) y el flujo `$EFS`
  (`nombre.ext.efsmeta`), con estado `cifrado` y una nota en el informe. La importación a un fichero
  cifrado de Windows queda fuera de M4 (pregunta abierta 8).

### 7.10 `mr-report` (#27): diario, listas e informes

**Diario** (fuente de verdad, escrito durante la recuperación): JSON Lines UTF-8 en el destino
(`memrescue-recuperacion-<fecha>-<id>.jsonl`), una línea por entrada con vaciado tras cada fichero; si se
corta la ejecución, lo escrito es válido. Contiene: versión del esquema, origen (ruta, tamaño, SHA-256 si
se calculó, volumen y desplazamiento), opciones, y por entrada: ruta virtual, ruta de destino, nombre
original si se escapó, categoría, registro y secuencia, tamaños, tiempos originales, atributos, estado
(`integro`, `integro_no_verificable`, `parcial`...), acción (`escrito`, `apartado`, `omitido_*`,
`renombrado`), bytes por estado, tramos no fiables (desplazamiento, longitud, estado, LBA y sectores,
`truncado`), hashes y su ámbito, partes de división, flujos, errores del destino. Línea final con
totales y estado (`completa`, `cancelada`, `fallida`).

**Hashes** (CRC-32, MD5, SHA-1, SHA-256 de `mr-common`, RC-10), calculados en la misma pasada sobre los
bytes **escritos** de cada flujo. Cada conjunto lleva su **ámbito**, siempre visible junto a los valores:

| Ámbito | Cuándo | Lectura |
|---|---|---|
| `contenido` | Todos los tramos fiables | Es el hash del contenido del fichero |
| `escrito_con_relleno` | Hay tramos no fiables | Solo identifica el fichero escrito (con relleno); **no** es el hash del original |
| `no_calculado` | Fichero omitido, cancelado o con error | Sin valores |

Así ningún hash sobre relleno se puede tomar por el del original (ADR-019, criterio de #27). Las listas y
el informe nunca muestran una columna de hash sin la de ámbito (pregunta abierta 9).

**Productos derivados del diario** (generados al final o a petición, también sin recuperar: «Lista»):

| Producto | Formato |
|---|---|
| Lista TXT (RC-11) | Tabla de ancho fijo legible, UTF-8 con BOM, rutas completas |
| Lista CSV (RC-11) | RFC 4180, UTF-8 con BOM, separador «,», columnas fijas documentadas en `formatos/informe-recuperacion.md` (estado, ámbito de hash y recuentos de bytes no fiables incluidos) |
| Lista HTML (RC-12) | Un solo fichero autónomo, sin scripts ni recursos externos, todo el texto escapado; resumen, tabla ordenable sin JavaScript (orden de la tabla = orden de recuperación) y estados con texto, no solo color (como ADR-021) |
| Informe (RC-10) | TXT y HTML con: cabecera (origen, volumen, opciones, fecha, versión), resumen por estado y acción, tabla de ficheros con hashes y ámbito, sección «Tramos no fiables» por fichero, errores del destino, nota de rutas largas |
| Lista de sectores (RC-13) | CSV: ruta, flujo, desplazamiento en el flujo, LBA inicial y número de sectores del dispositivo (o bytes con opción), estado; filtro por rangos de LBA (D32) y modo inverso «ficheros en estos sectores» (D33) |

La lista de sectores no necesita leer datos: sale de los runs y del desplazamiento del volumen. Una
exportación en formato de mapa de dominio de ddrescue (G2) sería útil para clonar solo lo seleccionado
(pregunta abierta 10).

### 7.11 Interfaces

- **API Rust** (`mr_api`): `recover(vfs, selección, opciones, ctx) -> RecoverResult`,
  `start_recover_task`, `recover_list(...)`, `sector_list(...)`; opciones validadas antes de lanzar.
- **ABI**: tarea con progreso y **peticiones** (conflicto, error, sin espacio) que el llamador responde
  (`ARQUITECTURA.md` §4); estructuras con `struct_size` (como `mr_verify_options`).
- **CLI**: `recover <origen> --dest <dir> [--volume ...] [--path <ruta virtual>]... [--category ...]
  [--include/--exclude <máscara>]... [--size a..b] [--date a..b] [--on-conflict ...] [--on-error ...]
  [--split auto|never|<tamaño>] [--no-times] [--no-ads] [--report txt,html] [--list csv,txt,html]
  [--sector-list csv]`; códigos de salida: 0 todo `integro`, 3 si algún fichero escrito no es `integro`
  (como ADR-019), otro código si falla la operación. `list` y `sector-list` sin recuperar.

## 8. Correspondencia con la matriz de paridad

| Fila | Diseño (apartado) | Diferencias deliberadas respecto a lo observado |
|---|---|---|
| RC-01 Recuperación masiva | §7.2, §7.4 (memoria acotada, sin límite de número) | Diario por fichero y reanudación por `omitir_si_identico` |
| RC-02 Categorías | §7.2 (categorías y `fiab_datos`), §7.5 | Filtro adicional por fiabilidad; carpetas sintéticas con nombres propios |
| RC-03 Filtros | §7.5 | Exclusión con `!`; rangos abiertos explícitos; reglas por ruta |
| RC-04 Destino seguro | §7.1, §7.4 (`check_output_path`, contención de rutas) | Con imágenes: nunca el origen ni sus segmentos; disco físico en M9 |
| RC-05 Conflictos | §7.7 | `omitir_si_identico`; sustitución solo explícita; renombrado sin sustitución atómico |
| RC-06 Errores | §7.3, §7.8 | Estado `reasignado`; políticas por estado; informe por tramos con LBA |
| RC-07 Rutas largas y Unicode | §7.6 | Siempre `\\?\`; escape documentado (original en el informe) en lugar de transliteración; UTF-16 original |
| RC-08 División | §7.6 | Automática según el FS de destino; hash del conjunto y de cada parte |
| RC-09 Fechas, permisos | §7.9 | Permisos y propietario al informe, no a la salida |
| RC-10 Informe con hashes | §7.10 | Ámbito del hash obligatorio |
| RC-11 Lista TXT/CSV | §7.10 | Formato documentado y estable |
| RC-12 Lista HTML | §7.10 | Plantilla propia, autónoma y sin scripts |
| RC-13 Lista de sectores | §7.10 | Modo inverso y filtro por LBA en la misma orden |
| FS-05 EFS en bruto | §7.9 | Exportación de `$DATA` cifrado y `$EFS` en ficheros separados; sin formato de importación en M4 |

## 9. Pruebas de aceptación propuestas

Sobre el corpus NTFS de `PRUEBAS.md` §1.2 (imágenes del artefacto, manifiestos versionados, pruebas
`#[ignore]` de corpus en la CI), más volúmenes sintéticos de `mr-fs-ntfs` (`testvol.rs`) y el dispositivo
simulado con errores de `PRUEBAS.md` §3. Todas comprueban además que el SHA-256 de la imagen de origen no
cambia y que en el destino no queda ningún `.mrtmp`.

| Escenario | Prueba | Resultado esperado | Filas |
|---|---|---|---|
| `ntfs_basic` | Recuperar la raíz entera | Las 62 entradas vivas con su ruta del manifiesto; SHA-256 de cada fichero igual a `sha256`; estado `integro`, ámbito `contenido`; código de salida 0; tiempos de escritura iguales a los del registro (`fake_time` del manifiesto) | RC-01, RC-09, RC-10 |
| `ntfs_basic` | Nombre largo con espacios y acentos; `configuración.ini` | Nombres idénticos en UTF-16 en el destino | RC-07 |
| `ntfs_mbr`, `ntfs_gpt` | Ídem a través de la partición | Mismos hashes; lista de sectores con LBA = `start_lba` + LCN × 8 | RC-01, RC-13 |
| `ntfs_deleted` | Categoría `borrado` | Las 11 entradas borradas en su ruta (el directorio borrado con sus 5 ficheros), SHA-256 igual; estado `integro_no_verificable` (nunca `integro`) | RC-02 |
| `ntfs_deleted` | Categoría `vivo` con filtro de exclusión | Solo los 51 vivos menos los excluidos; directorios vacíos no creados | RC-02, RC-03 |
| `ntfs_overwritten` | Recuperar `/sobrescritura` | `objetivo_1..3`: estado `sobrescrito`, todos sus tramos `reasignado`, omitidos por defecto; con `--on-error write` se escriben con relleno, su hash tiene ámbito `escrito_con_relleno` y **nunca** coincide con `sha256` ni se informa como `recovered_sha256`; los 2 borrados intactos dan su `sha256`; código 3 | RC-06, RC-10 |
| `ntfs_overwritten` | `--include-reallocated-bytes` | El SHA-256 escrito coincide con `recovered_sha256`, ámbito `escrito_con_relleno`, estado `sobrescrito` | RC-06 |
| `ntfs_damaged_mft` | Recuperar todo | Los 4 registros dañados del manifiesto como `no_disponible`, sin fichero escrito ni contenido inventado; el resto igual que `ntfs_basic` | RC-06 |
| `ntfs_damaged_boot` | Recuperar todo | Igual que `ntfs_basic`; el informe registra el uso de la copia del arranque (`ValidWithFixes`) | RC-10 |
| `ntfs_damaged_mftmirr` | Recuperar todo | Igual que `ntfs_basic`; anomalías de `$MFTMirr` en el informe | RC-10 |
| `ntfs_fragmented` | Recuperar todo | SHA-256 iguales; lista de sectores con un rango por run del manifiesto | RC-01, RC-13 |
| `ntfs_sparse_ads` | Destino NTFS | Ficheros dispersos con su SHA-256; cada ADS como flujo del fichero de salida con su `sha256`, también el ADS del directorio | RC-01, RC-09 |
| `ntfs_sparse_ads` | Destino sin flujos (simulado) | ADS como ficheros hermanos escapados con su `sha256` | RC-07 |
| Sectores ilegibles | `ntfs_basic` con el dispositivo simulado marcando sectores dentro de los runs de un fichero | Fichero `parcial`; tramos `unreadable` con su LBA exacta en el informe y en el diario; relleno `BAD!` exactamente en esos bytes; las cuatro políticas (`escribir`, `escribir_y_ocultar`, `apartar`, `omitir`) dan su efecto; cada sector malo se lee una sola vez | RC-06 |
| Relleno que imita datos | Sector ilegible cuyo relleno configurado coincide con el contenido original | Sigue `parcial`: el estado manda, no el contenido | RC-06 |
| Conflictos | Destino con ficheros previos; volumen sintético con `A.txt` y `a.txt` (POSIX) y versión viva y borrada del mismo nombre | Cada política según §7.7; nunca se sustituye sin `--overwrite-existing`; umbral alcanzado → omitir y anotar | RC-05 |
| Carrera | Crear el nombre final entre la escritura y el renombrado (gancho de prueba) | El renombrado sin sustitución falla y se aplica la política; el fichero ajeno queda intacto | RC-05 |
| Nombres hostiles | Volumen sintético con nombres `..`, `a\..\..\x`, `CON`, `nul.txt`, `fin.`, `fin `, `a:b`, código 1, suplente suelto | Todo dentro del destino, nombres escapados según §7.6, originales en el informe | RC-07, RC-04 |
| Rutas largas | Destino con raíz profunda; rutas finales > 260 y > 1024 | Escritas con `\\?\`; marcadas en el informe | RC-07 |
| División | Opción `--split 1MiB` sobre ficheros del corpus; prueba unitaria del límite 4 GiB − 1 con un flujo disperso virtual y destino FAT32 simulado | Concatenar las partes da el `sha256`; el informe tiene el hash del conjunto y de cada parte | RC-08 |
| Destino prohibido | Destino = imagen de origen, un segmento, `\\.\PhysicalDrive0`, `\\?\C:` | Rechazo antes de leer, sin escribir nada | RC-04 |
| Cancelación | Cancelar en el fichero k | Ningún fichero parcial con nombre final; diario con estado `cancelada` y los k − 1 ficheros | RC-01 |
| Repetición | Ejecutar dos veces con `omitir_si_identico` | La segunda no escribe nada y su informe lo dice | RC-01, RC-05 |
| Listas | `list --format csv,txt,html` y `sector-list` sin recuperar | CSV con el número de filas y columnas documentado; HTML bien formado (validador) y sin `<script>`; sectores calculados sin leer datos (contador de lecturas a cero en la zona de datos) | RC-11, RC-12, RC-13 |
| Hashes | Las cuatro sumas de ficheros vivos | Coinciden con un cálculo independiente (Python `hashlib` y `zlib.crc32`) y SHA-256 con el manifiesto | RC-10 |
| EFS | Volumen sintético con `$DATA` cifrado y `$EFS` | `.efsdata` y `.efsmeta` byte a byte iguales a los atributos; estado `cifrado` | FS-05 |
| Escala | Árbol sintético de 100 000 ficheros pequeños | Memoria máxima acotada (medida) y tiempo lineal | RC-01 |
| Determinismo | Dos ejecuciones iguales | Mismo árbol de salida y mismo diario salvo fechas y rutas absolutas | Todas |

## 10. Preguntas abiertas para el propietario

1. **Política por defecto por estado**: ¿`parcial` → escribir con relleno, `sobrescrito` y `sin_datos` →
   omitir, `integro_no_verificable` → escribir? ¿O preguntar siempre en la UI la primera vez?
2. **Nombres de las carpetas sintéticas** en el árbol y en el destino (`[Raiz]`, `[Huerfanos]`,
   `[Metadatos]`, `[Sin nombre]`, `[No fiables]`): ¿en español sin tildes, en inglés, o según el idioma de
   la interfaz? Afecta a rutas de salida y a pruebas.
3. **Renombrado automático**: ¿`nombre (2).ext` para todo, o un sufijo que diga que es la versión borrada?
   ¿Umbral por defecto de 10?
4. **Marcar ficheros no íntegros en el propio destino**: además del informe, ¿fichero lateral `.mrbad`,
   sufijo en el nombre, carpeta aparte por defecto o nada?
5. **RC-09 permisos y propietario**: ¿basta con guardarlos en el informe (SDDL cuando se lea `$Secure`), o
   se quiere una opción para aplicarlos a la salida?
6. **Reanudación**: ¿es suficiente repetir con `omitir_si_identico`, o M4 debe reanudar con estado en la
   base de trabajo?
7. **Orden de copia**: ¿orden del árbol (por defecto) u orden por LBA para discos mecánicos (más rápido,
   peor para el usuario que espera una carpeta concreta)?
8. **FS-05**: ¿se acepta la exportación en ficheros separados como paridad de M4, dejando la importación a
   EFS (formato no estable según [MS-EFSR] §2.2.3) para una investigación posterior?
9. **Hashes de ficheros no íntegros**: ¿calcularlos con ámbito `escrito_con_relleno` (propuesta) u
   omitirlos del todo para que nunca aparezca un valor que alguien pueda copiar sin el ámbito?
10. **Lista de sectores**: ¿exportar también en formato de mapa de dominio de ddrescue?
11. **Ubicación del diario y de los informes**: ¿en la raíz del destino (propuesta) o en una carpeta
    elegida aparte, para no mezclar con los ficheros recuperados?
12. **Bytes reasignados**: ¿se mantiene `--include-reallocated-bytes` desactivado por defecto incluso en la
    UI de un perito?
13. **Nombres UTF-16 originales**: ¿se cambia `mr-fs-ntfs` para conservar el UTF-16 original (necesario
    para §7.6 y para distinguir nombres con suplentes sueltos), o se acepta la pérdida y se trata como
    conflicto?
14. **Decisión de arquitectura**: ¿se registra este diseño como ADR (estados `reasignado` y
    `integro_no_verificable`, ámbito de hash, renombrado sin sustitución) antes de implementar #25 a #27?
