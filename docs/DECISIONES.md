# Registro de decisiones (ADR)

Formato: contexto → decisión → alternativas → consecuencias. Estados: **Propuesta** (pendiente de
confirmación del propietario), **Aceptada**, **Sustituida**.

---

## ADR-001 · Implementación original en sala limpia (Aceptada)

**Contexto.** La investigación de DMDE incluye pseudocódigo decompilado. El objetivo es una
alternativa independiente.

**Decisión.**
- La investigación se usa para escribir **especificaciones de comportamiento** (qué hace, con qué
  tolerancias), nunca como fuente de código.
- Los formatos se implementan a partir de especificaciones públicas (Microsoft, UEFI, ext4, Apple
  File System Reference, Btrfs, etc.) y se validan con imágenes generadas por herramientas independientes.
- Los ficheros de `docs/investigacion/dmde-fase1/evidencias-decompiladas/` no se consultan al implementar.
- No se copian textos, iconos, plantillas, firmas RAW ni ficheros auxiliares de DMDE.
- No se analiza ni reproduce el sistema de licencias de DMDE.

**Consecuencias.** Algunas heurísticas propias de DMDE habrá que redescubrirlas con pruebas.

---

## ADR-002 · Paridad = comportamiento comprobable (Aceptada)

**Decisión.** Una funcionalidad solo se marca "en paridad" en `PARIDAD_DMDE_PRO.md` cuando existe
una prueba automatizada reproducible sobre imágenes sintéticas que demuestra el comportamiento
descrito en la referencia (manual oficial y/o evidencia del binario). Sin prueba, el estado máximo
es "implementado".

---

## ADR-003 · Lenguaje del motor: Rust (Aceptada, 2026-10-17)

**Contexto.** El motor parsea datos binarios no confiables y dañados (entrada hostil por definición),
necesita rendimiento de E/S y paralelismo, y debe ser mantenible durante años.

**Decisión.** Motor en **Rust** (edición 2021, toolchain estable MSVC), organizado en un *workspace* de
crates. Aprobada por el propietario el 2026-10-17. `unsafe` prohibido (`forbid`) en todos los crates
salvo la frontera FFI (`mr-ffi`) y, en el futuro, el acceso a dispositivos Windows.

**Alternativas evaluadas.**

| Opción | A favor | En contra |
|---|---|---|
| **Rust** | Seguridad de memoria al parsear estructuras corruptas; rendimiento nativo; `cargo fuzz`/`proptest`; crate `windows` oficial para Win32/IOCTL | Curva de aprendizaje; menos librerías de UI nativas |
| C++20 | Ecosistema maduro, Qt | Riesgo alto de desbordamientos con datos corruptos, que es exactamente nuestro caso |
| C# (.NET 8) | Productividad, buena integración Windows | Parsing masivo de sectores y GC en escaneos de TB; menos control de memoria |

---

## ADR-004 · Interfaz: WinUI 3 (.NET 8) sobre el motor Rust (Sustituida por ADR-012)

> El propietario eligió WPF en lugar de WinUI 3 (2026-10-17). Se conserva el análisis original.

**Contexto.** Se busca una app de Windows 11 profesional y familiar para usuarios de DMDE: densidad
alta de información, árboles y listas de millones de elementos, editor hex.

**Decisión propuesta.** UI en **C# + WinUI 3 (Windows App SDK)**; el motor Rust se expone como
biblioteca con **API C estable** (`memrescue_core.dll`, generada con `cbindgen`) consumida mediante
P/Invoke. Listas/árboles virtualizados (`ItemsRepeater`), editor hex como control propio dibujado con Win2D.

**Alternativas evaluadas.**

| Opción | A favor | En contra |
|---|---|---|
| **WinUI 3 + C#** | Aspecto nativo de Windows 11, accesibilidad, alto DPI, tema oscuro, rapidez de desarrollo de UI | Dos lenguajes; frontera FFI que mantener |
| Qt 6 (C++/QML) vía cxx-qt | UI densa muy capaz, multiplataforma | Licencia LGPL/comercial; integración Rust más frágil; aspecto menos Windows 11 |
| egui / Slint (Rust puro) | Un solo lenguaje | Menos maduro para listas enormes, accesibilidad y apariencia nativa |
| Tauri (web) | UI rápida de prototipar | Rendimiento de árboles/hex con millones de filas; dependencia WebView2 |

**Consecuencias.** La frontera FFI obliga a diseñar una API de **tareas asíncronas con progreso y
cancelación** (ver ARQUITECTURA §4) que además sirve para la CLI de pruebas.

---

## ADR-005 · CLI headless para pruebas y automatización (Aceptada, 2026-10-17)

**Decisión.** Además de la GUI, `memrescue-cli.exe` expone las operaciones del motor (abrir imagen,
listar particiones, escanear, recuperar, clonar). Las pruebas de extremo a extremo usan la CLI.
La CLI no es la interfaz del usuario final, pero permite pruebas reproducibles y diagnóstico.

---

## ADR-006 · Solo lectura por defecto y escritura con doble barrera (Aceptada)

**Decisión.**
- Todo dispositivo se abre en solo lectura. Las fuentes de recuperación nunca se escriben.
- Las operaciones de escritura (particiones, editor hex, herramientas NTFS, clonado a disco) pasan por
  una **capa de cambios diferidos** (deshacer/rehacer, volcado de rollback) y solo se aplican con:
  (1) modo global de escritura activado, (2) confirmación explícita por operación con resumen de sectores.
- Durante las fases iniciales la escritura sobre **dispositivos físicos** queda deshabilitada en
  compilación (feature `physical-write` desactivada); solo se escribe en ficheros de imagen.
- El destino de recuperación no puede estar en el mismo disco físico que el origen (salvo
  anulación explícita, igual que la referencia).

---

## ADR-007 · Ejecución con privilegios (Propuesta)

**Decisión propuesta.** Un único proceso con manifiesto `requireAdministrator` solo cuando se abren
dispositivos físicos; para imágenes no se requiere elevación. Alternativa a evaluar en el hito de
dispositivos: proceso *broker* elevado que solo hace E/S de bloques.

---

## ADR-008 · Formatos de persistencia propios (Aceptada, SQLite, 2026-10-17)

**Decisión.**
- Logs de escaneo, trabajos y proyectos: **SQLite** (crate `rusqlite`, SQLite enlazado estáticamente)
  con esquema versionado (`PRAGMA user_version`), WAL y `synchronous=NORMAL`.
- SQLite guarda **metadatos, progreso y estado de reanudación**; el contenido binario recuperado o
  volcado va en ficheros independientes (requisito del propietario).
- Reanudación: al abrir la base, los trabajos que quedaron en `running` pasan a `interrupted` y pueden
  continuar desde su último punto de control (`job_checkpoints`). La máquina de estados se valida en código.
- Logs de clonado: formato propio + **importación/exportación de mapfile de GNU ddrescue** (formato
  abierto y documentado) para interoperabilidad.
- Configuración: TOML.
- No se busca compatibilidad con los logs/INI propietarios de DMDE.

---

## ADR-009 · Biblioteca de firmas RAW propia (Aceptada)

**Decisión.** Las firmas RAW se definen en ficheros declarativos propios (TOML/JSON) escritos a
partir de especificaciones públicas de cada formato. No se copia la tabla de firmas de DMDE.

---

## ADR-010 · Repositorio y estructura (Aceptada, 2026-10-17)

**Decisión.** Repositorio git (rama `main`) con:
`core/` (crates Rust del motor), `cli/` (memrescue-cli), `app/` (solución .NET: WPF + interop + pruebas),
`tests/` (generadores, manifiestos y corpus), `scripts/` (compilación y pruebas), `tools/`, `docs/`.
`Cargo.toml` raíz define el workspace. El ejecutable final se llama `MemRescue.exe`.

---

## ADR-011 · .NET 10 LTS con SDK local y publicación autocontenida (Aceptada, 2026-10-17)

**Contexto.** El equipo no tenía ningún SDK de .NET (solo runtimes 8 y 10 base). .NET 8 pierde soporte
el 10-nov-2026; .NET 10 es LTS hasta nov-2028.

**Decisión.** (Elegida por el propietario.)
- Destino `net10.0-windows`. SDK 10.0.402 instalado **dentro del proyecto** (`.tools\dotnet`, excluido de
  git) con el script oficial firmado `dotnet-install.ps1`, sin administrador ni cambios en PATH.
  `global.json` fija la versión; `scripts\env.ps1` lo usa automáticamente.
- MemRescue se distribuye **autocontenido** (incluye el runtime): funciona sin instalar .NET, como
  herramienta portable (`scripts\build.ps1 -Configuration Release -Publish`).

---

## ADR-012 · Interfaz gráfica en WPF (Aceptada, 2026-10-17)

**Contexto.** Prioridad del propietario: máxima fidelidad funcional y de experiencia con DMDE
Professional; interfaz técnica y densa (tablas complejas, árboles, paneles redimensionables, menús,
diálogos avanzados, editor hexadecimal) antes que efectos visuales modernos.

**Decisión.** UI en **C# + WPF** (.NET 10). Motivos: madurez para aplicaciones de escritorio densas,
virtualización de listas/árboles (`VirtualizingStackPanel`), `GridSplitter`, menús y atajos clásicos,
`RenderTargetBitmap` para pruebas visuales, control propio para el editor hex.
Identidad visual propia (icono y colores generados por `tools/make_icon.py`); sin recursos de DMDE.
Regla de UI: cada control funciona de verdad o aparece deshabilitado indicando el hito que lo implementa.

---

## ADR-013 · Diseño de la frontera Rust ↔ C# (Aceptada, 2026-10-17)

**Decisión.** API C estable (`memrescue_core.dll`, ABI versionada) consumida con `[LibraryImport]`.
Detalles en [API_FFI.md](API_FFI.md). Puntos clave:
- El motor nunca devuelve memoria propia; las salidas van a búferes del llamador.
- Tareas identificadas por **handles numéricos en un registro**, no punteros (un handle inválido es un
  error, no una corrupción de memoria).
- Asincronía por **sondeo y cancelación cooperativa**, sin callbacks hacia .NET.
- Errores como códigos + mensaje por hilo; pánicos capturados en cada función exportada.
- Cabecera C escrita a mano (no se usa `cbindgen`, para no añadir herramientas); su coherencia se
  verifica con pruebas de tamaño de estructuras en Rust y en C#.

**Alternativas descartadas.** COM/WinRT (complejidad), proceso separado con IPC (latencia y complejidad
innecesarias en M0; se reconsiderará con ADR-007 para el acceso elevado a dispositivos), `csbindgen`
(herramienta extra).

**Revisión M1.** ABI 2: orígenes como handles, lectura de sectores, verificación, detección de formato,
códigos 10–11, cadenas ≤ 64 KiB. En C#, `EngineHandle` con finalizador de seguridad.

---

## ADR-014 · Política de admisión de rutas por lista blanca (Aceptada, 2026-10-17)

**Contexto.** La revisión técnica encontró que `\\?\C:` abría el volumen como dispositivo pese a la lista
negra de prefijos (REVISION_TECNICA_M1, R1).

**Decisión.** Mientras el acceso a dispositivos esté deshabilitado, solo se admiten **ficheros normales**:
lista blanca léxica de formas de ruta, resolución de enlaces sin abrir el destino, ruta canónica con letra
de unidad o UNC y apertura con `FILE_FLAG_OPEN_REPARSE_POINT` verificando los atributos del objeto abierto.
El acceso a discos (M9) tendrá su propia vía explícita, nunca a través de `ImageFile`.

---

## ADR-015 · Contenedores: detectar antes que interpretar (Aceptada, 2026-10-17)

**Decisión.** Un fichero cuyo formato se reconoce como contenedor (VHD, VHDX, VMDK, EWF, QCOW, VDI, DMG) no
se abre como RAW salvo petición explícita del usuario. Cada formato se implementará solo cuando exista un
corpus de referencia generado por una herramienta independiente.

---

## ADR-016 · Propietario único de la base de trabajo (Aceptada, 2026-10-17)

**Decisión.** Una base de trabajo SQLite pertenece a un solo `Store` a la vez (bloqueo exclusivo de
`<base>.lock`). Con `synchronous=FULL` y `quick_check` al abrir. Así es seguro marcar como interrumpidos los
trabajos huérfanos al abrir (REVISION_TECNICA_M1, S1–S5).

---

## ADR-017 · Formatos de contenedor: soporte por variante verificada (Aceptada, 2026-10-17)

**Contexto.** Reconocer una cabecera no demuestra que se sepa leer el contenido (requisito del propietario).

**Decisión.** Una variante de contenedor solo se declara soportada cuando su lectura completa coincide
byte a byte (SHA-256 y rangos aleatorios) con la conversión a RAW hecha por una **referencia independiente**
(qemu-img portable, `HERRAMIENTAS_TERCEROS.md`) y su mapa de regiones dispersas es coherente con
`qemu-img map`. Hoy: VHD fijo/dinámico, VHDX fijo/dinámico, VMDK monolithicSparse. Todas las estructuras se
validan contra el tamaño real del fichero con aritmética comprobada y memoria acotada (tablas ≤ 256 MiB);
la estructura incoherente da `Corrupt`, nunca un pánico ni datos inventados. Si se pide un tamaño de sector
distinto del nativo, se presentan los mismos bytes con otra división (`Reblocked`).

**Addendum (incidencia 9): VMDK streamOptimized y dependencia `miniz_oxide`.** Se declara soportada la
variante streamOptimized (granos deflate en formato zlib con marcadores; GD en la cabecera, como la escribe
qemu-img, o en el pie, como la escribe VMware) tras compararla con qemu-img (SHA-256, rangos aleatorios y
`qemu-img map`, también con el corpus). Decisiones:

- **Validación al abrir sin descomprimir**: se lee el marcador de cada grano (LBA igual a su posición en
  la GT, tamaño comprimido entre 1 y 2 × grano, dentro del fichero) y su zona (marcador + datos) entra en
  la detección de solapes de ADR-020; el pie es estructura crítica. Descomprimir todo al abrir exigiría
  leer el fichero entero, así que la apertura estricta **no** garantiza que todos los granos se
  descompriman: un grano cuyo flujo zlib no es válido, cuyo Adler-32 no cuadra o que no mide un grano (o
  la cola del disco) da `IoError::Read` al leerlo, en ambos modos, y el lector tolerante lo marca
  `Unreadable`. Nunca se entregan datos de un grano que falla. Una alteración que infle al tamaño
  correcto con el mismo Adler-32 no se puede detectar (VMDK no guarda otra suma).
- **Memoria**: los tamaños comprimidos se cargan como una tabla más (presupuesto del addendum de
  ADR-020); por lectura, como mucho 2 × grano + grano; caché LRU de 4 MiB; granos de streamOptimized
  limitados a 16 MiB (`UnsupportedFeature` si no).
- **Dependencia**: `miniz_oxide` **=0.8.9** (con `adler2` 2.0.1), fijada en `[workspace.dependencies]`
  con solo la función `with-alloc`. Motivos: Rust puro con `#![forbid(unsafe_code)]` (la regla de
  `unsafe` solo en `mr-ffi` se mantiene también en las dependencias de lectura), sin código C ni
  compilación nativa, comprueba el Adler-32 y permite acotar la salida
  (`decompress_to_vec_zlib_with_limit`), ampliamente usada (motor por defecto de `flate2`), licencias
  MIT / Zlib / Apache-2.0 (`adler2`: 0BSD / MIT / Apache-2.0). Alternativas descartadas: `flate2` (capa
  más sobre el mismo motor), `zlib`/`libz-sys` (C), inflado propio (riesgo sin beneficio). Cambiar de
  versión exige revisar el diff del crate y repetir las pruebas contra qemu-img.

---

## ADR-018 · Referencias externas: nunca se siguen automáticamente (Aceptada, 2026-10-17)

**Decisión.** Discos diferenciales (VHD/VHDX), padres y extensiones VMDK y descriptores VMDK separados
devuelven `ExternalReference` con el nombre referenciado **como texto** (saneado y truncado). El motor nunca
abre un fichero que no haya indicado el usuario: un descriptor malicioso con `..\..\Windows\…`, rutas
absolutas o UNC no provoca ningún acceso. Las imágenes divididas tampoco se abren al descubrirlas: el motor
lista los segmentos del **mismo directorio** y la apertura exige una petición explícita (`MR_OPEN_SPLIT`,
confirmación en la interfaz). Abrir padres/extensiones será una operación explícita futura con la misma
política de rutas.

---

## ADR-019 · Mapa de validez por sector obligatorio (Aceptada, 2026-10-17)

**Contexto.** Requisito crítico del propietario: el relleno de sectores ilegibles no puede confundirse con
datos.

**Decisión.** Toda lectura tolerante entrega, junto a los datos, el estado de **cada** sector: `Read`,
`Recovered`, `Unreadable`, `Skipped` o `Sparse`. Ilegibles (`BAD!`) y saltados (`SKIP`) llevan rellenos
distintos y se marcan como no fiables en el motor, la ABI (`mr_source_read_mapped`, informe de
verificación con los 5 recuentos), la CLI (código de salida 3) y el visor («NO FIABLE: patrón de relleno»).
Las lecturas estrictas (`mr_source_read`) y los hashes fallan ante cualquier error: nunca se calcula un hash
sobre bytes sustituidos. Los ceros de regiones dispersas son legítimos (`Sparse`, fiables). Detalle en
`formatos/mapa-de-validez.md`.

---

## ADR-020 · Tablas de asignación dañadas: estricto por defecto, tolerante por petición explícita (Aceptada, 2026-10-17)

**Contexto.** Issue #4. Un VHD, VHDX o VMDK con alguna entrada de su tabla de asignación dañada (fuera del
fichero, solapada, duplicada o con un estado inválido) se rechazaba entero con `Corrupt`. Para recuperar
datos interesa aprovechar todo lo legible sin presentar datos inventados.

**Decisión del propietario.** Modo **estricto por defecto** (comportamiento anterior: la imagen se rechaza
con `Corrupt`) y modo **tolerante solo por activación explícita**: `mr_image::OpenOptions::tolerant`,
`MR_OPEN_TOLERANT` (bit 4 de `flags` en `mr_source_open`, ampliación compatible de la ABI 3),
`Engine.OpenSource(..., tolerant: true)`, `--tolerant` en la CLI y, en la interfaz, una confirmación
explícita con aviso visible. El origen nunca se modifica.

**Correspondencia inequívoca.** Una entrada solo se usa si su zona del fichero (la parte del bloque o grano
que cubre el disco virtual) está **entera dentro del fichero**, **no solapa** ninguna estructura del
contenedor **ni la zona de otra entrada** y su **estado es válido**. Cuando dos o más zonas se solapan se
invalidan **todas** las implicadas (también las duplicadas idénticas): nunca se elige una interpretación.

| Formato | Entradas | Estructuras que no se pueden solapar | Estados inválidos |
|---|---|---|---|
| VHD dinámico | BAT: mapa de bits + parte útil del bloque | copia del pie (byte 0), cabecera dinámica, BAT completa, pie final (fin de la zona de datos) | no hay campo de estado; 0 y cualquier sector cuyo bloque no quepa son inválidos |
| VHDX | BAT: bloques FULLY_PRESENT (parte útil) | zona de cabeceras (1 MiB inicial), registro, región de metadatos, región BAT, otras regiones | 4, 5 y PARTIALLY_PRESENT (sin padre); desplazamiento desbordado o < 1 MiB |
| VMDK monolithicSparse | GD: GT de 2 KiB; GT: granos (parte útil) | cabecera, descriptor incrustado, GD, las GT (un grano que solapa una GT invalida ambos) | entrada de GT 1 sin el indicador «zeroed-grain» |

En modo tolerante cada entrada inválida invalida **solo** su bloque o grano (una entrada de GD de VMDK
inválida invalida los granos de su GT). Esos bloques se entregan como **no fiables**: `read_at` devuelve
`IoError::Read` con el rango virtual y `allocation` los informa como `Data`, de modo que el lector tolerante
(`mr_io::resilient`) los marca `Unreadable` con el patrón de relleno (ADR-019) y las lecturas estrictas, los
hashes y la verificación fallan o los cuentan como no fiables. No se añade un estado «desconocido»: el
contenido de un bloque descartado no se lee, así que «ilegible» describe con exactitud lo que se entrega.
Las entradas de mapa de bits de sectores de VHDX con estado inválido no afectan a la lectura de un disco
sin padre: en modo tolerante se cuentan en la descripción y se ignoran.

**Siguen siendo críticos en ambos modos** (`Corrupt`): pies y cabeceras con firma, suma o versión
incorrectas, cabecera dinámica, ubicación y tamaño de la BAT/GD, tablas de regiones, metadatos y
descriptor, y estructuras fijas que se solapan entre sí.

**Consecuencias.** La descripción del origen termina en «modo tolerante: N bloques descartados (no
fiables)» («granos» en VMDK). El modo estricto ahora detecta también solapes entre bloques y duplicados
(antes no se comprobaban) y, en VMDK, valida todas las GT y sus granos al abrir (antes los granos se
validaban al leerlos): una imagen con esos daños se rechaza en la apertura. Las imágenes válidas no
cambian (pruebas contra qemu-img). Memoria: VMDK guarda las GT presentes (≤ `MAX_TABLE_BYTES`); la
memoria de trabajo de la apertura está acotada por el presupuesto del addendum siguiente. Detalle por
formato en `formatos/vhd.md`, `formatos/vhdx.md` y `formatos/vmdk.md`.

**Addendum (issue #38): presupuesto de memoria por apertura.** Con la BAT de un VHD al límite
(256 MiB, bloques de 512 B, 67 108 864 entradas) y todas las entradas en el mismo bloque, la apertura
llegaba a unos 2,8 GiB en ambos modos (VHDX 1,4 GiB, VMDK 2,5 GiB), porque la detección de solapes y el
registro de daños guardaban varias copias por entrada; un fallo de asignación abortaba el proceso (no
lo capturan los guardas de pánicos de la FFI ni de las tareas). Se decide:

- **Presupuesto**: la memoria de trabajo proporcional a las tablas que reserva una apertura no pasa de
  `mr_image::open_memory_budget(t)` = 17/8 × `t` + 64 KiB (`OPEN_MEMORY_FIXED_BYTES`) por tabla, con
  `t` = bytes de tablas de asignación leídos del fichero (BAT; en VMDK, GD y GT presentes). Desglose:
  tabla decodificada que conserva la imagen (≤ 1 × `t`), lista de candidatos que se ordena para la
  detección de solapes (índices `u32`, ≤ 1 × `t`), registro de daños (1 bit por entrada, ≤ `t`/32) y
  estructuras menores (zonas e índice de las GT en VMDK). Con `MAX_TABLE_BYTES` = 256 MiB: como mucho
  unos 544 MiB en VHD y VHDX y unos 1088 MiB en VMDK (GD y GT al límite a la vez).
- **Cumplimiento**: cada una de esas reservas pasa por un contador (`budget.rs`) que rechaza superar el
  presupuesto y reserva con `try_reserve_exact`; tanto el exceso como un fallo de asignación se
  informan con `UnsupportedFeature` y un mensaje claro, nunca abortan.
- **Sin copias**: las tablas en bruto se leen por trozos de 1 MiB y se decodifican sobre la marcha;
  la detección de solapes procesa los grupos en flujo (solo guarda el primer elemento de cada grupo
  hasta que llega el segundo y entrega cada elemento una vez); `Damage` es un mapa de bits con
  recuento en modo tolerante y no guarda nada en modo estricto. En VMDK el GD se reutiliza como índice
  de las GT y los granos se identifican por su posición en las GT cargadas.
- **Estricto falla pronto**: la apertura estricta termina en la primera entrada inválida (fuera del
  fichero o estado inválido, en orden de índice) o en el primer solape que encuentra el recorrido (en
  orden de desplazamiento en el fichero), sin seguir validando. El tipo de error (`Corrupt`) no
  cambia; si hay varios daños, el que se nombra en el mensaje puede ser otro que antes.

Pico medido (prueba `core/mr-ffi/tests/table_memory.rs`, tablas al límite y entradas duplicadas,
estricto / tolerante): VHD 512 / 520 MiB (antes 2816), VHDX 385 / 389 MiB (antes 1409), VMDK con GD
al límite 512 / 520 MiB (antes 2560), VMDK con GT al límite 521 / 529 MiB (antes 2575).

---

## ADR-021 · Patrones de relleno configurables y colores por estado en el visor (Aceptada, 2026-10-17)

**Contexto.** Issue #7 (IO-08, HX-01). Los rellenos de sectores ilegibles (`BAD!`) y saltados (`SKIP`)
eran fijos y el visor solo distinguía el estado por texto. Se pide configurarlos desde la interfaz, de
forma persistente, y colorear cada sector según su estado sin que el relleno pueda pasar por datos.

**Decisión.**
1. **Motor y ABI.** Nueva función `mr_source_read_mapped_ex` con `mr_read_options` (32 bytes,
   `struct_size` como `mr_verify_options`): reintentos, salto, lectura hacia atrás (`MR_READ_READ_BACK`),
   `bad_fill[4]` y `skip_fill[4]`. Ampliación compatible de la ABI 3: `mr_source_read_mapped` conserva su
   firma y su comportamiento (`BAD!`, sin salto). En Rust: `mr_api::read_sectors_mapped_with` y
   `MappedReadOptions`; en C#: parámetros opcionales de `Source.ReadSectorsMapped`.
2. **Regla de admisión** (motor, CLI e interfaz, misma regla): ningún patrón puede tener sus cuatro bytes
   iguales y ilegibles y saltados deben usar patrones distintos. `00 00 00 00` se confundiría con los
   sectores dispersos (ceros legítimos) y con datos a cero; `FF FF FF FF`, `E5 E5 E5 E5`, `F6 F6 F6 F6` y
   similares son rellenos habituales en discos reales (flash borrada, formateo). El motor rechaza el par
   con `MR_ERR_INVALID_ARGUMENT` antes de leer. La regla solo evita volcados engañosos: la autoridad
   sigue siendo el estado de cada sector (ADR-019) y un patrón que aparezca en los datos del origen sigue
   siendo `Read`.
3. **Verificación sin rellenos configurables.** La verificación no entrega datos (solo recuentos y mapa de
   tramos), así que el patrón no cambia su resultado; `mr_verify_options` no se amplía. Los hashes siguen
   siendo estrictos y nunca incluyen relleno, sea cual sea el patrón (pruebas en mr-api, mr-ffi y C#).
4. **Preferencias por usuario.** `%APPDATA%\MemRescue\settings.json` (JSON con los patrones en 8 dígitos
   hexadecimales, sin ambigüedad), sustituible con la variable de entorno `MEMRESCUE_SETTINGS_DIR` (las
   pruebas la usan siempre: nunca escriben en el perfil real). Escritura atómica (temporal + sustitución);
   un fichero ausente, dañado, desmesurado (> 64 KiB) o con patrones no admitidos se ignora con un aviso en
   el registro y se usan los valores por defecto, sin sobrescribirlo hasta que el usuario guarde. Nunca se
   escribe junto a las imágenes.
5. **Visor.** Un bloque por sector con fondo y texto según su estado (`SectorStatePalette`, contraste
   ≥ 7:1 comprobado por prueba), barra lateral gruesa en los no fiables, cabecera con el estado en texto,
   línea «!! SECTOR NO FIABLE … PATRÓN DE RELLENO <patrón> … NO datos del origen», fila de estados por
   sector (`SectorState<LBA>`) y leyenda (`Legend<Estado>`, `FillPatternsText`). El color nunca es la
   única señal. Con un tema de alto contraste se usan solo colores del sistema (resaltado para los no
   fiables) y el texto distingue los estados.

**Consecuencias.** El visor lee siempre con `mr_source_read_mapped_ex` y los patrones guardados. Sin salto
en el visor, los sectores `Skipped` solo aparecen en lecturas con salto (CLI `sectors --jump`, API); la
paleta y los textos ya los cubren. El volcado pasa de `TextBox` a `RichTextBox` con el mismo
`AutomationId` (`HexView`); UI Automation lo lee con `TextPattern`. Detalle en
`formatos/mapa-de-validez.md` y `API_FFI.md`.

---

## ADR-022 · MBR/EBR: confianza en vez de aceptación y tabla de particiones por handle (Aceptada, 2026-10-18)

**Contexto.** Issue #12 (PT-01). INV-05 §2.3 describe cómo la referencia acepta o rechaza un sector como
tabla; §5 propone sustituir el booleano por la confianza de `mr_common::Confidence`. Hay que decidir qué
se lista, cómo se gradúa y cómo cruza la frontera C sin romper la ABI 3.

**Decisión.**
1. **Sin tabla frente a tabla dudosa.** En la LBA 0 un sector se interpreta como MBR si tiene firma o, sin
   firma, si es plausible (alguna entrada empieza dentro del disco, indicadores válidos en las entradas
   usadas y como mucho una extendida). Si no, el esquema es `None` con `MbrAnomaly::NoTable` y su motivo
   (en blanco, ninguna entrada dentro, indicador, extendidas, cabecera GPT). Un sector de arranque NTFS,
   exFAT o FAT en la LBA 0 es `Superfloppy`, no una tabla vacía. La ausencia de tabla es **informativa**
   (`Severity::Notice`): un disco sin particionar no es un disco dañado.
2. **Gravedad de cada anomalía** (`Severity`): `Invalid` para indicador no válido en una entrada usada,
   varias extendidas y entrada que empieza fuera del disco; `Suspicious` para firma ausente, varias
   activas, tipo desconocido, entrada que termina fuera del disco, solape, problemas de la cadena (bucle,
   enlace fuera, EBR inutilizable, límite) y sector de tabla ilegible; `Notice` para el resto (híbrido,
   indicador en entrada libre). La confianza de una partición es la peor de sus anomalías, de las de la
   tabla que la mencionan y de la falta de firma de su propia tabla; la del conjunto, la peor de todas.
3. **Qué se lista.** Todas las entradas de datos con tamaño mayor que 0, aunque sean inválidas (con su
   confianza), en orden de entrada y de cadena; la extendida es un contenedor y no se lista. Solo se
   recorre la primera extendida. Las entradas 0x05, 0x0F y 0x85 son extendidas y enlaces (mejora frente a
   la referencia, INV-05 §4). Datos relativos al EBR; enlaces relativos al inicio de la extendida; un
   enlace que sale del disco o de la extendida, o que vuelve a un EBR visitado, termina la cadena con su
   anomalía; máximo 1024 EBR. Los solapes se informan con un barrido ordenado (como mucho una anomalía
   por partición), para que la salida quede acotada.
4. **GPT.** Con una única entrada 0xEE el esquema es `Gpt` (con comprobación de inicio en la LBA 1 y de
   tamaño, que admite 0xFFFFFFFF); con 0xEE y otras entradas, `Hybrid`, que lista las entradas MBR
   distintas de 0xEE. Las particiones GPT las añadirá #13 en `read_partition_table`.
5. **ABI.** Ampliación compatible de la ABI 3 (`API_FFI.md`): la tabla se lee una vez y queda en el
   registro de handles junto a su origen (se pueden abrir particiones aunque el origen se cierre); se
   consulta por índice con estructuras de salida versionadas por `struct_size`; cada anomalía lleva código
   numérico estable, gravedad, LBA, destino y valor, y un texto con el código textual y la descripción
   separados por un salto de línea. Abrir una partición que termina fuera del origen abre la parte
   existente y lo dice en la descripción; nunca se inventan sectores.

**Alternativas.** Rechazar la tabla como la referencia (se perdería la información de tablas dañadas que
el usuario necesita ver); devolver toda la tabla serializada en una cadena JSON por la ABI (obligaría a
la interfaz a analizar texto y no permitiría abrir particiones por índice).

**Consecuencias.** La interfaz y la CLI muestran siempre lo que hay, con su grado de confianza, en lugar
de ocultar tablas dañadas. La búsqueda de particiones perdidas (#15) podrá reutilizar `read_mbr_at` en
posiciones candidatas con sus propias reglas estrictas. Los nombres de tipo son una lista propia de
identificadores públicos; un tipo que no figura se informa como desconocido.

### Adenda (2026-10-18): integración con GPT (#13)

1. **Esquema.** `read_partition_table` (`core/mr-part/src/table.rs`) lee el MBR y, si es protector o
   híbrido, o si la LBA 0 no tiene una tabla MBR utilizable (en blanco, ilegible, no plausible o con una
   cabecera GPT) pero aparece alguna firma `EFI PART`, incorpora la GPT (`gpt::read_gpt`): esquema `Gpt`
   (también para imágenes desplazadas sin MBR) o `Hybrid`. Con un MBR clásico o un disco sin
   particionar la GPT no se mira (una GPT obsoleta no cambia lo que dice el MBR).
2. **Unión.** Se listan las particiones MBR (en un híbrido, sus entradas distintas de 0xEE) y después
   las GPT (`Origin::Gpt`, con nombre; GUID único y atributos en `PartitionTable::gpt_entry`). Las
   anomalías GPT se añaden a las de la tabla sin duplicados. En un híbrido, cada entrada MBR que no
   coincide en inicio y tamaño con una GPT recibe `hybrid_mbr_mismatch`. La lectura GPT completa queda
   en `PartitionTable::gpt` (copia en uso, desplazamiento, sectores finales ausentes, GUID de disco).
3. **Gravedad GPT.** `Severity::Compensated` (nueva) para el daño de una copia o de su posición: la
   tabla queda «válida con correcciones», como en el lector GPT; sospechosas las que afectan a la imagen
   o a la copia elegida (desplazamiento, recorte, rango fuera de la imagen, copias en desacuerdo,
   tamaño de sector distinto, solapes y entradas fuera de rango); inválida la entrada invertida. La
   confianza del conjunto es la peor entre la de las anomalías y la que da el lector GPT (inválida si
   ninguna copia es utilizable).
4. **Desplazamiento: opción segura.** Las particiones GPT conservan las LBA **del disco**. Abrir una
   partición GPT de una imagen desplazada se **rechaza** por defecto (`InvalidArgument` con el
   desplazamiento en el mensaje); solo con una petición explícita (`apply_shift` en `mr_api`,
   `MR_PARTITION_OPEN_APPLY_SHIFT` en la ABI, `--apply-shift` en la CLI, confirmación en la interfaz) se
   abre en `LBA disco - shift`, calculado con el desplazamiento informado, y la descripción del nuevo
   origen lo dice («desplazamiento aplicado»). Si la partición empieza antes del inicio de la imagen,
   se rechaza siempre. Una tabla interpretada con otro tamaño de sector que el del origen también se
   rechaza: hay que reabrir el origen con ese tamaño (las LBA no se convierten en silencio).
5. **ABI.** Sigue siendo una ampliación compatible de la ABI 3: `mr_partition_table_info` crece a 80 B
   (copia GPT, desplazamiento con signo, sectores finales ausentes, tamaño de sector GPT y GUID de
   disco) y `mr_partition_info` a 96 B (GUID único y atributos) antes de publicarse; funciones nuevas
   `mr_partition_name` y `mr_partition_open_ex`; códigos GPT 201-241 en el bloque reservado y
   `MR_ANOMALY_VALUE_SIGNED` para el valor de `image_shifted`.


---

## ADR-023 · Búsqueda exprés de particiones perdidas: candidatos por niveles con presupuesto acotado (Aceptada, 2026-10-18)

**Contexto.** Issue #15 (PT-05, PT-06). INV-05b §8 propone un algoritmo propio (sala limpia) para
generar candidatos, sondearlos y graduar la evidencia; §10 dejaba preguntas abiertas que el
propietario ha respondido.

**Decisiones del propietario.**
1. La búsqueda se ejecuta **solo a petición** en M2 (no automáticamente al abrir un origen).
2. Reconocedores en M2: NTFS, FAT12/16/32 y exFAT, más tablas MBR/EBR (modo estricto) y cabeceras GPT
   encontradas en posiciones candidatas. ext, XFS, HFS+, APFS, Btrfs y ReFS quedan para hitos
   posteriores; el rasgo `Recognizer` permite añadirlos sin cambiar la búsqueda. Sin copias lejanas
   de otros sistemas de archivos por ahora.
3. Copia NTFS en el último sector del volumen y variante `+1` (INV-05b §4); la de NT 3.51 no.
4. Un volumen que empieza antes del inicio de la imagen se informa como **parcial** y no se puede abrir.
5. Presupuestos por defecto de INV-05b (4096 lecturas, 32 MiB, rejillas de 64 y 16 MiB, 64 cilindros,
   8 candidatos por hueco, 256 volúmenes, ventana de 8 KiB), configurables en API, ABI, CLI
   (`--max-reads`) y C#.

**Decisión de diseño** (`formatos/busqueda-particiones.md`):
- Candidatos P0 (tabla y byte 0), P1 (derivados: encadenado, tablas encontradas, desplazamiento),
  P2 (rejilla inicial), P3 (huecos) y P4 (rejilla final), en una cola determinista deduplicada; el
  trabajo no crece con el tamaño del disco. Reservas: P2 a P4 como mucho el 40 % cada uno y nunca la
  reserva del 25 % para P0 y P1. Estado `BudgetExhausted` con los pendientes por nivel.
- Cada estructura encontrada propone las hipótesis «principal» y «copia»; se elige la de mejor
  evidencia. Un sector ilegible nunca es evidencia ni llega a un reconocedor: indicador «?».
- Indicadores propios E, P, C, S, U, A con cinco estados (`+ x ? o -`) y confianza por reglas
  explicables (válida: principal + copia coherente, principal + estructuras sin copia en el formato, o
  principal + estructuras + entrada; el resto, sospechosa con sus motivos).
- Los volúmenes que coinciden con una partición de la tabla se marcan con ella (`tabla #N`), no se
  duplican.
- Desplazamiento de la imagen: hipótesis aceptada solo con **dos evidencias independientes** que
  coinciden; se informa (`image_shifted`, `mbr_displaced`) y abrir un volumen que depende de ella
  exige la misma petición explícita que ADR-022 (`apply_shift`).
- La tarea de `mr-api` comprueba la cancelación antes de cada lectura física; cancelada, termina en
  estado cancelado **con** resultado parcial (`TaskContext::finish_cancelled_with_result`).
- ABI: ampliación compatible de la ABI 3 (opciones de 64 B, resumen de 96 B, volumen de 136 B, tabla de
  32 B, todas versionadas por `struct_size`; códigos 301-314 y 320-321; `MR_ANOMALY_LBA_SIGNED`).

**Alternativas.** Búsqueda automática al abrir con presupuesto reducido (INV-05b §10.8; descartada
por el propietario para M2); barrido completo (es el escaneo de M6); aceptar un desplazamiento con
una sola evidencia (los sectores ocultos de las lógicas pueden ser relativos al EBR).

**Consecuencias.** Volúmenes en posiciones fuera de rejillas, sin tabla ni vecinos confirmados ni copia
al final de un MiB no se encuentran (lo cubrirá el escaneo completo). Las tablas encontradas se
interpretan con el sector del dispositivo. La comparación manual con DMDE sigue pendiente (ADR-002,
opcional).

---

## ADR-024 · Corpus NTFS generado en un ejecutor desechable de GitHub Actions con TSK como oráculo (Aceptada, 2026-10-18)

**Contexto.** Issue #19. Ninguna funcionalidad NTFS (M3, M4) puede declararse sin un corpus con verdad
conocida (ADR-002). `PLAN_CORPUS_NTFS.md` recomendaba generarlo con `mkntfs`/`ntfs-3g` y verificarlo con
The Sleuth Kit en WSL2, lo que exigía instalar WSL2 con administrador en el equipo del desarrollador.

**Decisión del propietario (2026-10-18).**
1. El corpus NTFS se genera en un **ejecutor Ubuntu desechable de GitHub Actions** (workflow
   `ntfs-corpus.yml`): `mkntfs` y `ntfs-3g` (paquetes de Ubuntu, versiones registradas) crean y pueblan
   las imágenes; **The Sleuth Kit** (`fls`, `istat`, `icat`, `ffind`, además de `mmls`, `fsstat` e `ifind`)
   es el oráculo independiente que rellena los manifiestos semánticos (rutas, tamaños, SHA-256 vía
   `icat`, registros MFT, estado borrado, runs, ADS, rangos dispersos).
2. Nada se instala en el equipo del desarrollador y nada requiere administrador local. Las imágenes se
   publican como artefacto del workflow con su SHA-256 y un script local (`tools/fetch_ntfs_corpus.py`)
   las descarga y las verifica contra los manifiestos versionados.
3. La compresión LZNT1 creada por Windows queda fuera de alcance por ahora (pendiente de una VM Windows
   desechable).

**Decisión de diseño.**
- **Reproducibilidad byte a byte** (conseguida y comprobada en cada ejecución): `mkntfs --zero-time`
  (marcas de tiempo y número de serie fijos), ntfs-3g bajo libfaketime con la hora congelada, montaje
  directo del fichero en primer plano, contenido derivado de SHAKE-256 con semilla, orden de operaciones
  fijo e identificadores MBR/GPT fijos. El workflow genera dos veces desde cero y compara; además, el
  `image_sha256` de los manifiestos versionados obliga a que cada ejecución reproduzca exactamente las
  imágenes publicadas. La reproducibilidad semántica (manifiestos idénticos) es el criterio de respaldo
  si una actualización de Ubuntu cambiara los bytes.
- **Verdad desde TSK, no desde MemRescue ni desde el generador**: `build_manifests.py` recorre todos los
  registros MFT con `istat`, resuelve las rutas con la cadena de padres de `$FILE_NAME` y obtiene el
  contenido con `icat`. La intención del generador solo sirve para exigir que TSK vea exactamente lo que
  se hizo (rutas, inodos de ntfs-3g, tamaños, hashes, marcas de tiempo) y para clasificar los
  sobrescritos cluster a cluster (`full`, `partial`, `none`, con los ficheros que ocupan ahora los
  clusters).
- **Tres validaciones sin código común**: `build_manifests.py` (TSK), `verify_tsk.py` (TSK por otras
  vías) y `validate_ntfs_images.py` (Python puro: tablas, arranque, `$MFT`, `$MFTMirr`, registros con su
  secuencia de actualización, mapping pairs y SHA-256 de los datos leídos siguiendo los runs), con 20
  pruebas negativas. Un segundo job las repite sobre el artefacto descargado en un ejecutor nuevo.
- Manifiestos versionados en `tests/corpus/ntfs/manifests/` (esquema en `PRUEBAS.md` §1.2); imágenes
  fuera de git, en `tests/corpus/ntfs/generated/` (ignorado). Acciones fijadas por SHA de commit
  (`checkout` v7.0.1, `upload-artifact` v7.0.1, `download-artifact` v8.0.1), `permissions: contents:
  read`, artefacto con retención explícita de 90 días.
- La CI obligatoria no descarga el corpus hasta que las pruebas del lector NTFS (#20 en adelante) lo
  usen; entonces lo obtendrán con `fetch_ntfs_corpus.py` o en un job que dependa del workflow.

**Alternativas.** WSL2 en el equipo del desarrollador (requiere administrador y cambia el sistema); VM
Windows (mejor fidelidad, pendiente para LZNT1); versionar las imágenes en git (≈330 MiB, contrario a la
política de binarios de la auditoría de higiene); un generador NTFS propio (descartado por ADR-001 y
por el principio de independencia).

**Consecuencias.** Los NTFS del corpus son de ntfs-3g, no de Windows: sin nombres DOS 8.3, sin huecos
perforados (ntfs-3g no implementa `fallocate`), sin `$UsnJrnl` y con el mismo número de serie en todas las
imágenes. Para usar el corpus en local hace falta `gh` autenticado y una ejecución del workflow con
artefacto vigente (si caduca, se relanza y produce los mismos bytes). Una actualización de ntfs-3g, TSK o
del núcleo del ejecutor puede obligar a revisar y versionar manifiestos nuevos.

---

## ADR-025 · Lector NTFS: registros por estado, fixups sin tolerancia y copia del arranque situada por su propio total (Aceptada, 2026-10-18)

**Contexto.** Issue #20 (FS-01, M3): primer sistema de archivos. Hay que decidir qué se hace con
registros dañados o ilegibles, cuándo se acepta la copia del arranque, cómo se comprueba `$MFTMirr`
y dónde queda la frontera con #21-#23. INV F1-05 §5.3 observa en la referencia una tolerancia: un
tramo con la secuencia de actualización incorrecta fuera del área usada no invalida el registro.

**Decisión** (detalle en `formatos/ntfs.md`):
1. **Crate `mr-fs-ntfs`** en la capa de sistemas de archivos, sobre `mr-io` y `mr-part` (usa
   `sector::read_trusted` y la escala `Severity` de ADR-022; no conoce la API ni la interfaz).
   `mr-api::ntfs_open` y la CLI `ntfs-info` lo exponen; FFI e interfaz llegan con el árbol (#22).
2. **Cada registro tiene un estado** (`in_use`, `not_in_use`, `empty`, `uninitialized`,
   `unmapped`, `outside_source`, `unreadable`, `damaged`) en lugar de un error: un registro malo
   nunca detiene la lectura de los demás y nunca se usa como válido. Un sector no fiable dentro de
   un registro lo deja `unreadable` sin interpretarlo (ADR-019). Los daños de registros no cambian la
   confianza del volumen.
3. **Secuencia de actualización sin tolerancia**: cualquier tramo que no termine en el USN marca el
   registro dañado, también fuera de los bytes en uso (diferencia deliberada con la referencia: un
   registro a medio escribir no se acepta en silencio; #23 podrá ofrecer una recuperación explícita
   de esos registros con su propia confianza).
4. **Copia del arranque**: con el principal no válido o ilegible solo se acepta una copia del último
   (o penúltimo) sector del origen si su propio total de sectores la sitúa ahí; usarla es una
   corrección informada (`ValidWithFixes`). Con el principal válido, la copia se comprueba y su
   estado se informa.
5. **`$MFTMirr`**: se compara con la MFT registro a registro, sin la matriz de la secuencia de
   actualización; solo el registro 0 se toma de la copia (para localizar la MFT) si el de `$MFT` no
   es utilizable. Las discrepancias son anomalías compensadas.
6. **Frontera**: `runs` decodifica *mapping pairs* con aritmética comprobada solo para la MFT; #21 lo
   ampliará (datos, dispersos, compresión) sin cambiar `decode_runs`. `apply_fixups` es común a
   `FILE` e `INDX` (#22). La enumeración (`records`) es la base de #22 y #23.

**Alternativas.** Aceptar tramos con fixup incorrecto fuera del área usada (como la referencia);
tratar un registro dañado como error que aborta la enumeración; aceptar cualquier copia del arranque
que sea un BPB válido (podría ser la de otro volumen dentro del origen); reparar en memoria todos los
registros que `$MFTMirr` contiene.

**Consecuencias.** La CLI y las pruebas de corpus informan los registros dañados exactamente
(`ntfs_damaged_mft`) y la copia usada (`ntfs_damaged_boot`). Una MFT con extensiones
(`$ATTRIBUTE_LIST`) se lee solo en la parte que cubre el registro 0 (`mft_incomplete`); seguirlas
queda para #21. Si el registro 0 falla en las dos copias, el volumen no se abre (lo cubrirá el
escaneo de M6).

---

## ADR-026 · Flujos NTFS: estado por tramo, extensiones estrictas y LZNT1 con oráculo de Windows (Aceptada, 2026-10-18)

**Contexto.** Issue #21 (FS-01, FS-03, FS-04): leer el contenido de los ficheros NTFS
(residentes, no residentes, dispersos, comprimidos con LZNT1, flujos alternativos) y seguir
`$ATTRIBUTE_LIST`. Hay que decidir cómo se entregan los bytes que no son datos del origen, qué
hacer con extensiones que no cuadran y cómo validar LZNT1 sin ficheros comprimidos en el corpus
(ntfs-3g no los crea).

**Decisión** (detalle en `formatos/ntfs.md`):
1. **Estado por tramo de bytes** en lugar de por sector: `data`, `sparse`, `uninitialized`
   (fiables: ceros definidos por el formato; lo que hay más allá del tamaño inicializado **nunca**
   se lee) y `unreadable`, `unmapped`, `outside_source`, `corrupt` (no fiables: patrón `BAD!`
   alineado al desplazamiento del flujo, configurable). Es la extensión de ADR-019 a ficheros: un
   fichero tiene clústeres en disco, huecos y unidades comprimidas que no son sectores. La CLI
   escribe el patrón solo en esos bytes, los lista en la salida de errores y termina con código 3.
2. **Errores frente a avisos en un flujo**: lo que impide saber qué hay en el flujo (cifrado,
   tamaños incoherentes, unidad de compresión distinta de 16 clústeres, extensiones solapadas,
   repetidas o más allá del tamaño asignado, sin primera extensión) es un error tipado; lo que solo
   deja partes sin describir (runs no válidos en una extensión, hueco entre extensiones, runs más
   cortos que los datos inicializados) es un aviso y esas partes se leen `unmapped`.
3. **Extensiones estrictas**: un registro de extensión solo se usa si su referencia al base es
   exactamente el base (número y secuencia), su secuencia es la de la entrada de la lista y no tiene
   lista propia (no se sigue: no puede haber ciclos); cada registro se lee una vez, con límites de
   4096 extensiones y 1 MiB de lista. Única holgura: en un registro no en uso se admite la secuencia
   incrementada una vez al liberarlo (ficheros borrados). Las mismas reglas completan `$MFT`
   (`mft_extended`).
4. **LZNT1** propio de [MS-XCA] con límites (4096 por bloque, referencias dentro del bloque,
   límite de salida por unidad de compresión; ante un error solo cuentan los bloques completos).
   **Oráculo independiente**: vectores generados en Windows con `RtlCompressBuffer` /
   `RtlDecompressBuffer` (`tests/generators/gen_lznt1_vectors.py`, ctypes, sin administrador),
   versionados como texto hexadecimal con el SHA-256 de los datos sin comprimir; los casos límite
   (bloque corto seguido de otro, firma, terminación, errores) fijan el comportamiento de Windows.
5. **Unidad comprimida con sectores ilegibles**: se descomprime hasta el primer bloque afectado;
   el resto de la unidad es `unreadable` (los bloques LZNT1 son independientes).
6. **Salida de `ntfs-cat`** (`mr_api::create_output`, reutilizable en M4): nunca el propio origen
   ni uno de sus segmentos (rutas canónicas; los enlaces duros no se detectan), ni una ruta de
   dispositivo (lista blanca de ADR-014), ni un directorio; se permite junto al origen. Una salida
   existente solo se sustituye con `--overwrite`; se escribe en un temporal del mismo directorio
   que se renombra al terminar y se borra si no se termina.

**Alternativas.** Implementar `BlockDevice` para el flujo con `SectorState` (no distingue
disperso de sin inicializar ni comprimido dañado, y fuerza tamaños de sector); rellenar con ceros
lo ilegible (se confundiría con datos); aceptar extensiones con cualquier secuencia (mezclaría
registros reutilizados de otros ficheros); comprobar la firma de la cabecera LZNT1 (Windows no lo
hace y rechazaríamos datos que Windows lee).

**Consecuencias.** 439 flujos del corpus coinciden con el SHA-256 del manifiesto (vivos, borrados
íntegros, sobrescritos con lo que devuelve TSK y ADS). La lectura de un fichero borrado sigue sus
runs tal como están; su fiabilidad la dará #23. FFI e interfaz llegan con el árbol (#22). Los
flujos cifrados no se leen (FS-05, M4).

**Addendum (issue #58): publicación sin sustituir.** El punto 6 comprobaba que el destino no
existía y después llamaba a `std::fs::rename`, que en Windows sustituye un fichero existente: un
fichero creado por otro proceso entre la comprobación y el renombrado se perdía sin
`--overwrite`. Se decide:

- **Sin `overwrite`**, `OutputFile::commit` publica con `std::fs::hard_link(temporal, destino)`,
  que falla de forma atómica con `AlreadyExists` si el destino existe (`OutputExists`, el destino
  no se toca), y después borra el temporal. El temporal está en el mismo directorio, luego en el
  mismo volumen. No se usa `MoveFileExW` sin `MOVEFILE_REPLACE_EXISTING` porque exigiría `unsafe`
  fuera de `mr-ffi`.
- **Alternativa sin enlaces duros** (FAT, exFAT: `Unsupported`, `ERROR_INVALID_FUNCTION`,
  `ERROR_NOT_SUPPORTED`; en Unix `EPERM`, `EOPNOTSUPP`): se abre el destino con `create_new` y se
  copia el temporal. Tampoco sustituye nunca un fichero ajeno, pero no es atómica respecto al
  contenido: durante la copia el destino existe incompleto. Si la copia falla, se borra ese
  destino, que solo puede ser el creado por la propia llamada. Otros errores del enlace se
  devuelven tal cual (no se cae a la copia a ciegas).
- **Con `overwrite`**, se mantiene `std::fs::rename` (sustitución atómica).
- El temporal se borra en cualquier error de `commit` y al descartar el `OutputFile`. La CLI
  devuelve el código 2 también cuando el destino aparece durante la escritura.

## ADR-027 · Borrados NTFS: recuperabilidad conservadora por clúster y estado `possibly_overwritten` (Aceptada, 2026-10-24)

**Contexto.** Issue #23 (FS-16): decir, para cada fichero borrado de un volumen NTFS, qué datos
siguen disponibles y con qué fiabilidad, y recuperarlo diciendo qué bytes pueden no ser los
originales. Un registro no en uso conserva sus runs, pero sus clústeres pueden haberse reutilizado;
NTFS no guarda quién los reutilizó. Esta ADR cubre la parte de la MFT; las entradas `INDX`
residuales llegan tras el árbol de directorios (#22).

**Decisión** (detalle en `formatos/ntfs.md`, «Ficheros borrados»):
1. **Borrado** = registro **base** no en uso que sigue siendo un registro FILE válido con algún
   `$FILE_NAME` (los demás no en uso son libres). Las extensiones no son entradas: se unen por la
   `$ATTRIBUTE_LIST` con las reglas de ADR-026 y, si la lista falta o es ilegible, por su
   referencia al base (secuencia igual o una más). Dañados, ilegibles y fuera del origen se informan
   aparte y nunca se recuperan.
2. **Padre**: vivo (directorio en uso con la misma secuencia), borrado (directorio no en uso con la
   misma secuencia o una más) o huérfano (fuera de la MFT, inutilizable, reutilizado o no
   directorio). La ruta sale de la cadena de padres de la MFT solo si llega a la raíz con
   secuencias coherentes.
3. **Estado de cada clúster** con contenido (dentro del tamaño inicializado; en comprimidos,
   unidades enteras), por orden: en los runs de un fichero vivo (`owned`, aunque `$Bitmap` diga
   libre), asignado en `$Bitmap` sin dueño conocido, asignación desconocida (bit no fiable, mapa
   corto o `$Bitmap` inutilizable), libre pero reclamado también por otro borrado, libre. Solo lo
   libre cuenta como original.
4. **Recuperabilidad**: `full` solo si todos los clústeres con contenido son libres y no hay tramos
   sin mapa (o residente con el registro válido, o vacío); `none` si no queda ningún clúster libre
   ni disperso; `partial` en lo demás; disposición inutilizable = `none`.
5. **Memoria acotada**: dos pasadas por la MFT; los dueños se guardan solo como intersecciones con
   los clústeres de los borrados (`ClaimIndex`, 4 M intersecciones por defecto; si se llena, se
   informa y la asignación sigue saliendo de `$Bitmap`); `$Bitmap` por páginas de 64 KiB (64 en
   memoria); límites de entradas, directorios y problemas listados.
6. **Recuperación**: nuevo estado de tramo `possibly_overwritten` (no fiable, pero **sin** patrón:
   son los bytes del disco, que pueden ser de otro fichero), junto a los de ADR-026. Una unidad
   comprimida con un clúster sospechoso es entera `possibly_overwritten`. La salida usa
   `create_output`; la CLI termina con código 3 si algún byte no es fiable.

**Alternativas.** Comparar contenido (lo hace el oráculo del corpus, pero en un caso real no hay
original con el que comparar); fiarse solo de `$Bitmap` (un vivo cuyo bit está a 0 por un mapa
dañado dejaría pasar clústeres ajenos como originales); cargar todos los runs vivos (memoria
proporcional al volumen); rellenar con patrón lo posiblemente sobrescrito (se perderían datos que
pueden ser los originales: escrituras sin asignar o asignaciones sin escribir); decidir cuál de dos
borrados escribió el último por sus fechas (no son fiables).

**Consecuencias.** Los 16 borrados y sobrescritos del corpus coinciden con el manifiesto
(recuperabilidad, SHA-256 o `recovered_sha256`, clústeres sobrescritos y dueño del `reason`). Una
escritura en un clúster libre sin asignarlo no se detecta (el contenido se da por original). La
evaluación no lee los datos: los sectores ilegibles aparecen al recuperar. Numeración provisional:
puede cambiar al integrar las ramas paralelas.

