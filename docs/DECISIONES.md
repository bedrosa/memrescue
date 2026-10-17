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
