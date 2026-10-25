# Progreso de MemRescue

Actualizar al final de cada etapa: qué se terminó, qué se verificó, siguiente tarea.

---

## Etapa 1 · Investigación de DMDE (fase 1): 2026-10-17, Terminada (parcial en algoritmos)

- Análisis REA/Ghidra de `dmde.exe` 4.4.4.842 (SHA-256 `57a4febb…47adb4`, sin modificar).
- 11 documentos + README + inventario de imports (ahora en `docs/investigacion/dmde-fase1/`).
- Verificado en código: NTFS (boot, fixups, atributos, runs, índices, LZNT1, escaneo), GPT, ExtFS,
  APFS, MD5, CRC-32, zlib, IOCTLs de Windows.
- No terminado: 4 de 5 áreas de análisis quedaron incompletas (RAID/RAW, otros FS, E/S, arranque/scripts).

## Etapa 2 · Migración y planificación: 2026-10-17, Terminada

**Hecho**
- Repositorio del proyecto poblado con la investigación de la fase 1.
- Migrados 18 ficheros de investigación (documentos, datos auxiliares y 1 evidencia decompilada).
  **Verificado**: SHA-256 idéntico origen/destino. Los datos extraídos del binario y la evidencia
  decompilada se conservan solo en local (no se versionan, ADR-001).
- Consultadas 13 páginas del manual/web oficial de DMDE → `investigacion/dmde-manual-oficial.md`.
- Creados: `ARQUITECTURA.md`, `ROADMAP.md`, `DECISIONES.md`, `PRUEBAS.md`, `PARIDAD_DMDE_PRO.md`
  (124 funcionalidades), `investigacion/README.md`, `README.md` raíz.

**Verificado**
- Integridad de la migración (hashes).
- Funciones exclusivas de Professional según la web oficial (consulta 2026-10-17).

**No hecho / pendiente**
- Ninguna línea de código (por instrucción).
- No se abrió Ghidra ni se ejecutó DMDE en esta etapa.
- Decisiones tecnológicas ADR-003/004/005/007/008/010 en estado **Propuesta**.

## Etapa 3 · M0 Fundaciones: 2026-10-17, Terminada

**Decisiones del propietario**: Rust (motor), **WPF** (en lugar de WinUI 3), CLI, SQLite, C ABI + P/Invoke,
.NET 10 LTS con SDK local en el proyecto y publicación autocontenida. Ver ADR-003, 005, 008, 010–013.

**Entorno comprobado** (sin instalar nada global): Git 2.52, Rust 1.99.1 (MSVC), VS Build Tools 2026
(MSVC 14.50), Python 3.14. No había SDK de .NET → SDK 10.0.402 instalado en `.tools\dotnet` con el script
oficial firmado por Microsoft (autorizado por el propietario). WSL, sqlite3 y qemu-img no están instalados.

**Hecho**
- Git inicializado (rama `main`, sin commits todavía).
- Motor Rust: `mr-common`, `mr-io`, `mr-store` (esquema SQLite v1), `mr-api` (tareas), `mr-ffi` (DLL, ABI 1).
- `memrescue-cli` (version, image-info, hash, db-init, jobs).
- Solución .NET `app/MemRescue.slnx`: `MemRescue` (WPF → `MemRescue.exe`), `MemRescue.Core.Interop`,
  pruebas xUnit. Icono propio generado por `tools/make_icon.py`.
- Generador determinista de imágenes MBR/GPT + validador independiente + pruebas negativas.
- Scripts `scripts/build.ps1` y `scripts/test.ps1`. Documentos nuevos: `API_FFI.md`, `PLAN_CORPUS_NTFS.md`.

**Verificado (ejecución real, 2026-10-17)**

| Comprobación | Resultado |
|---|---|
| `scripts\build.ps1` (Rust + .NET Debug) | Correcta, 0 advertencias, 0 errores |
| `cargo fmt --check`, `cargo clippy -D warnings` | OK |
| Pruebas Rust | 34/34 superadas (+2 contra corpus) |
| Pruebas .NET (xUnit, DLL real) | 17/17 superadas |
| Corpus: determinismo (2 generaciones) | 3/3 imágenes idénticas byte a byte |
| Corpus: validación independiente | 128 comprobaciones, 0 errores |
| Corpus: pruebas negativas | 9/9 corrupciones detectadas |
| `MemRescue.exe --selftest` (Debug) | Ventana abierta, motor 0.1.0 / ABI 1, SHA-256 de `gpt_basic.img` = manifiesto |
| `MemRescue.exe` publicado autocontenido, sin .NET en el sistema | Arranca; SHA-256 de `gpt_4kn.img` = manifiesto |
| Captura de la ventana (`--screenshot`) | Revisada: menús, propiedades, registro, barra de estado |
| `scripts\test.ps1` completo | **10/10 pasos OK** |

**Errores encontrados y corregidos durante M0**
- Documentación XML incompleta (CS1573) y espera bloqueante en una prueba (xUnit1031): corregidos (las advertencias son errores).
- Fichero Rust sin formato canónico detectado por `test.ps1`: corregido con `cargo fmt`.
- Una mutación de la prueba negativa no alteraba nada (escribía `0xFF` sobre `0xFF`): corregida la prueba.
- Acentos mal mostrados por PowerShell 5.1 en scripts sin BOM y en procesos hijos: scripts en UTF-8 con BOM y `-X utf8`.

**Limitaciones conocidas**
- `MemRescue.exe` de **depuración** necesita `DOTNET_ROOT` apuntando a `.tools\dotnet` (lo fija `scripts\env.ps1`);
  el publicado autocontenido no.
- El autodiagnóstico abre la imagen siempre con sectores de 512 B.
- El validador del corpus es independiente del generador pero del mismo equipo; falta oráculo de terceros.
- Ninguna funcionalidad de recuperación existe todavía (correcto para M0); ninguna fila de la matriz en "Paridad".

**Criterios de finalización de M0**: compila sin errores ✔ · MemRescue.exe se inicia ✔ · la ventana WPF abre ✔ ·
WPF ↔ Rust funciona ✔ · la CLI invoca el motor ✔ · pruebas automatizadas ✔ · imágenes validadas ✔ ·
documentación actualizada ✔ · sin funciones ficticias ✔ (todo lo no implementado está deshabilitado con su hito).

## Etapa 4 · Commit inicial, revisión técnica, INV-05 y M1 (iteración 1): 2026-10-17, Entregada para revisión

**Git**: commit inicial `7fdc385` (79 ficheros; sin binarios, SDK, temporales, bases ni imágenes; material
propietario de DMDE excluido por `.gitignore`; sin remotos). Los cambios de esta etapa **no** están
confirmados todavía.

**Revisión técnica** (`REVISION_TECNICA_M1.md`): 25 hallazgos corregidos con prueba de regresión. Los más
graves: alias `\\?\C:` que abría el volumen (R1), enlaces a dispositivos (R2), comportamiento indefinido por
longitudes FFI desmesuradas (R13) y el marcado de trabajos ajenos como interrumpidos en SQLite (S1).

**INV-05** (`investigacion/dmde-fase2/INV-05-mbr-y-busqueda-de-particiones.md`): 13 consultas REA acotadas.
Verificado: tabla de nombres de tipo, esquemas (MBR/GPT/híbrido/Apple), reglas de aceptación de una tabla
MBR/EBR y sondeo de una posición candidata con copias de respaldo (sector 6, sector 12, último sector,
64–384 MiB, −1024 B). Sin verificar: el bucle que genera candidatos.

**M1 – iteración 1** (motor 0.2.0, ABI 2):
- Lectura RAW endurecida, sectores 512/1024/2048/4096, rangos estrictos, límite de memoria por petición.
- Lector tolerante a errores con progreso/cancelación y simulador de fallos.
- Detección de formatos de contenedor (sin interpretarlos aún).
- CLI: `probe`, `sectors`, `verify`.
- WPF: visor hexadecimal por sectores (1–8 por página), ir a sector/desplazamiento, anterior/siguiente,
  primero/último, copiar volcado, cerrar origen, comprobación de legibilidad, confirmación explícita para
  abrir contenedores como RAW; autodiagnóstico con `--sector-size`/`--read-lba`.

**Verificado (ejecución real)**

| Comprobación | Resultado |
|---|---|
| `scripts\build.ps1` (Debug) y `-Configuration Release -Publish` | Correctos, 0 advertencias, 0 errores |
| Pruebas Rust | **77/77** (mr-io 26, mr-store 11, mr-api 11, mr-ffi 13, mr-common 6, CLI 7, corpus 3) |
| Pruebas .NET (DLL real) | **47/47** |
| Corpus | determinista, 128 comprobaciones, 9/9 corrupciones detectadas |
| GUI `--selftest` 512 B (`gpt_basic`) y 4096 B (`gpt_4kn`) | LBA 1 = `EFI PART`, sectores y SHA-256 = manifiesto, 0 ilegibles |
| Exe publicado autocontenido sin .NET | Arranca; LBA 2048 de `mbr_basic` = marcador de P1 |
| Orígenes tras todas las pruebas | 3/3 imágenes con SHA-256, tamaño y fecha intactos |
| `scripts\test.ps1` | **12/12 pasos OK** |

**Funcionalidades completadas** (ver `PARIDAD_DMDE_PRO.md`): IM-01 probada; IO-06, IO-08, HX-01, HX-04, HX-07,
UX-02, UX-05 parciales; UX-09 implementada. **Pendientes en M1**: ver `ROADMAP.md` (PartitionView, IO-07/09,
IM-02, VHD/VHDX/VMDK, prueba de UI).

**Limitaciones**
- VHD/VHDX/VMDK solo se **detectan**: no hay forma de generar imágenes de referencia independientes sin
  instalar herramientas o usar administrador (WSL pospuesto por el propietario).
- La prueba de enlace simbólico → dispositivo no se ejecuta en este equipo (crear enlaces requiere privilegios);
  se cubre con pruebas léxicas y de uniones.
- La navegación de la interfaz se verifica por autodiagnóstico y captura, no por automatización de UI.

## Etapa 5 · M1 (iteración 2): contenedores, mapa de validez, rangos y prueba de UI, 2026-10-17, Entregada para revisión

**Herramienta de referencia**: qemu-img 11.1.2 portable (MSYS2 ucrt64) en `.tools/qemu`, con base de datos
y paquetes firmados verificados (firmante certificado por ≥ 2 llaves maestras de MSYS2), SHA-256 por
paquete, licencias y `PROVENANCE.json`. Fuera de Git y del producto; sin instalación, sin PATH, sin
administrador (`HERRAMIENTAS_TERCEROS.md`). Solo se usa con `convert`/`info`/`map` sobre imágenes temporales.

**Entregado** (motor 0.2.0, ABI 3):
- `mr-image`: VHD fijo/dinámico, VHDX fijo/dinámico (+ CRC-32C propio), VMDK monolithicSparse, imágenes
  divididas, apertura unificada por formato y adaptador de tamaño de sector (ADR-017).
- Referencias externas (diferenciales, padres, extensiones, descriptores separados) → `ExternalReference`
  sin abrir nada (ADR-018).
- Mapa de validez por sector con 5 estados en motor, ABI, CLI y visor; salto y lectura hacia atrás;
  rellenos `BAD!`/`SKIP` distintos; hashes estrictos (ADR-019).
- `RangeView`: un rango (p. ej. una partición) como dispositivo, anidable.
- CLI: `split-list`, `--split`, `--range`, `--force-raw`, `verify --jump/--read-back`, `sectors` con estados.
- WPF: tamaño de sector «Automático», apertura de contenedores, confirmación de imágenes divididas,
  «Abrir rango de sectores como dispositivo…» (Ctrl+Mayús+R), visor con estado por sector y aviso de no
  fiables, barra de posición para imágenes grandes, verificación con salto/lectura hacia atrás y recuento
  por estado, mensajes específicos para dañado/referencia externa/variante no soportada.
- Pruebas de UI Automation (`--open`), 9 pruebas.

**Errores corregidos durante la integración**: copias duplicadas de la lectura posicional en 4 lectores
(ahora `mr_io::read_exact_at` público); rutas absolutas de qemu-img en las pruebas (ahora relativas al
repositorio); descriptor VMDK separado devuelto como «no soportado» en vez de referencia externa; códigos
FFI provisionales (dañado → E/S) sustituidos por 12/13/14.

**Verificado (ejecución real)**

| Comprobación | Resultado |
|---|---|
| `scripts\test.ps1` | **13/13 pasos OK** |
| Pruebas Rust | **176** por defecto + **12** ignoradas que ejecuta `test.ps1` (3 corpus, 9 qemu-img): 188/188 |
| Contra qemu-img | VHD 4 conversiones, VHDX 14, VMDK 4 (+ rechazos), corpus × 5 variantes = 15: hash, rangos y mapa idénticos |
| Pruebas .NET | **52/52** interop + **9/9** interfaz (UI Automation) |
| `build.ps1` Debug y `-Configuration Release -Publish` | Correctos, 0 advertencias, 0 errores |
| Exe publicado sin SDK | Abre un VHDX dinámico de qemu-img con `gpt_4kn`: ABI 3, SHA-256 = manifiesto, 2048 sectores leídos, 0 ilegibles, contenedor intacto |
| Orígenes | Corpus 3/3 intacto; cada prueba de formato comprueba hash/tamaño/fecha de RAW y contenedores |

**Limitaciones conocidas**
- Diferenciales, VHDX con registro pendiente, VMDK streamOptimized/flat/multiextensión: se rechazan con
  error específico (no se declaran soportados).
- Un contenedor con una sola entrada de tabla fuera del fichero se rechaza entero (conservador).
- qemu-img no genera VHDX con sector lógico de 4096 ni VMDK con granos a cero: esos casos solo se prueban con
  imágenes sintéticas construidas según la especificación.
- La prueba de enlace simbólico → dispositivo sigue sin ejecutarse aquí (requiere privilegios); protección
  cubierta por pruebas léxicas, de uniones y por la verificación de atributos tras abrir.
- Las pruebas de UI necesitan una sesión de escritorio desbloqueada; no cubren diálogos modales.

## Etapa 6 · Repositorio público, auditoría y seguimiento por issues (2026-10-17)

**Auditoría de publicación** (ficheros versionados, historial, ramas, issues, milestones, labels y
descripción del repositorio):

| Comprobación | Resultado |
|---|---|
| Rutas personales, nombre de usuario, correos privados | 0 en los 4 commits de `main` y en GitHub |
| Credenciales y tokens | 0 |
| Material de la herramienta de referencia (decompilado, volcados del ejecutable) | 0 ficheros versionados; se conservan solo en local |
| Carácter U+2014 | 0 en ficheros, commits, issues, comentarios, milestones, labels y descripción (issue #2) |
| Identidad de los commits | Dirección `noreply` de GitHub, sin trailers de coautoría |
| Binarios versionados | Solo el icono propio, reproducible con `tools/make_icon.py` |

La limpieza del carácter U+2014 sustituyó 224 apariciones en 28 ficheros (dos puntos, coma o, en celdas
vacías de tablas, `-`); el resto del contenido no cambió. Verificado con `scripts\test.ps1`: 13/13 pasos.

**GitHub**: repositorio público `bedrosa/memrescue`; `main` protegida (PR obligatoria sin aprobación de
terceros, sin push directo, sin force push ni borrado, historial lineal, conversaciones resueltas, sin
excepciones para administradores); fusión solo por squash o rebase; auto-merge desactivado; token de
Actions de solo lectura; escaneo de secretos con bloqueo en el push. 30 issues (#1 a #30) con labels de
tipo, prioridad y área y milestones M1 a M12. Decisión del propietario sobre contenedores con tablas de
asignación dañadas (issue #4): modo estricto por defecto y modo tolerante solo por activación explícita.

**Integración continua** (issue #1, PR #31): workflow de GitHub Actions con 4 jobs (higiene del
repositorio, Rust, .NET e interop, lectores contra qemu-img), verificados en GitHub Actions y exigidos como
checks obligatorios de `main`; pruebas de interfaz a demanda; `CONTRIBUTING.md` con el flujo de trabajo.

**M1 por issues** (cada uno con su rama, PR y checks obligatorios en verde):

| Issue | PR | Resultado |
|---|---|---|
| #3 | #33 | Claves de MSYS2 fijadas a un commit y con SHA-256 comprobado en `tools/fetch_qemu_img.py` |
| #6 | #34 | `NullDevice` (todo disperso) y `PatternDevice` (patrón constante) |
| #5 | #35 | Omisión de errores por código y modo «ignorar todo» (IO-09); `mr_source_verify_start_ex` sin cambiar la ABI 3 |
| #4 | #36 | Tablas de asignación dañadas en VHD/VHDX/VMDK (ADR-020): estricto por defecto (ahora también rechaza bloques solapados o duplicados y valida todas las GT de VMDK al abrir) y modo tolerante solo explícito (`OpenOptions.tolerant`, `MR_OPEN_TOLERANT`, `--tolerant`, confirmación en la interfaz); un bloque solo se usa si su zona del fichero es inequívoca; los descartados se leen como ilegibles, nunca como datos |
| #7 | #37 | Patrones de relleno configurables (ADR-021): `mr_source_read_mapped_ex` con `mr_read_options` (ABI 3 sin cambios en lo existente), validación común (ni ceros, ni cuatro bytes iguales, ni ambos iguales), ajustes por usuario en `%APPDATA%\MemRescue\settings.json` con escritura atómica, colores por estado con texto, barra lateral y leyenda (contraste mínimo 7:1, alto contraste del sistema), diálogo de opciones y CLI `sectors --bad-fill/--skip-fill/--jump/--read-back` |

Verificación de #4: 213 pruebas Rust, 11 contra qemu-img 11.1.2 (2 nuevas en modo tolerante), 67 de interop
C#, 11 de interfaz y auditoría de higiene sin problemas. Verificación de #7: 219 pruebas Rust, 94 de
interop C#, 13 de interfaz; las pruebas nunca escriben en el perfil real (`MEMRESCUE_SETTINGS_DIR`).

## Etapa 7 · Cierre de M1 (2026-10-18)

**Revisión de robustez** (solo lectura, límites de memoria y ficheros corruptos). Encontró dos fallos
reales, corregidos con pruebas de regresión, y varios puntos de endurecimiento:

| Issue | PR | Resultado |
|---|---|---|
| #38 | #43 | Abrir una tabla de 256 MiB con entradas duplicadas llegaba a 2,8 GiB (y una falta de memoria abortaba el proceso). Ahora el pico es de 385 a 529 MiB según el formato, con presupuesto por apertura (`open_memory_budget`) y `try_reserve_exact`: superarlo devuelve un error |
| #39 | #41 | Un mapa de bits de VHD ilegible abortaba toda la verificación; ahora ese bloque se lee como datos y, si falla, queda ilegible con relleno, nunca disperso. Con `Fail` (hashes) el error sigue abortando |
| #40 | #42 | Enumeración O(1) de tramos no fiables en la FFI, reintentos acotados a 100 también en Rust, repetidos de imágenes divididas con `HashSet`, `overflow-checks` en Release y pruebas de corrupción: 900 inversiones aleatorias de bytes (VHDX, VMDK, divididas), 159 truncados en fronteras de estructura y ficheros acortados tras abrir, sin pánicos, bloqueos ni datos de estructuras rechazadas |

**Validación de cierre** sobre `main` (a30a4ae):

| Comprobación | Resultado |
|---|---|
| `scripts\test.ps1` (Debug) | 14/14 pasos |
| Pruebas Rust | 244 en Debug y 244 en Release (con comprobación de desbordamiento) |
| Comparación independiente con qemu-img 11.1.2 | 11/11 |
| Interop C# / UI Automation | 94/94 y 13/13 |
| `MemRescue.exe` Release publicado (`scripts\build.ps1 -Configuration Release -Publish`) | Autodiagnóstico correcto con GPT 512 B y GPT 4Kn: SHA-256 igual al manifiesto, 0 sectores malos, imagen sin modificar |
| Higiene (rutas, secretos, U+2014) | 0 problemas |

**Decisión del propietario**: M1 cerrado; las variantes adicionales de contenedor (#8 a #11) pasan al
milestone M1.5 y no bloquean M2.

**Limitaciones que quedan**: el modo tolerante de VHDX solo se prueba con imágenes sintéticas; no se lee el
GD redundante de VMDK; los sectores saltados solo aparecen en lecturas con salto (CLI y API); los colores y
el alto contraste no tienen prueba automática; sin hardware real hasta M9.

## Etapa 8 · M2 Particiones (2026-10-18)

| Issue | PR | Resultado |
|---|---|---|
| #16 | #46 | INV-05b en sala limpia (sin ejecutar ni analizar DMDE): algoritmo propio de candidatos por niveles con presupuesto acotado |
| #14 | #48 | Corpus de 38 escenarios de tablas dañadas, desplazadas y borradas (esquema de manifiesto 2), validador independiente y 57 pruebas negativas que no dependen del hash |
| #12 (base) | #47 | Crate `mr-part`: tipos comunes y `sector::read_trusted` (un sector ilegible nunca se interpreta) |
| #13 | #49 | GPT con copia principal y de respaldo, CRC-32, 512 y 4096 B, imagen desplazada o recortada (informada, nunca aplicada en silencio) |
| #12 | #50 | MBR/EBR con confianza y anomalías tipadas (ADR-022), GPT integrada, particiones en API, ABI 3 ampliada, CLI (`partitions`, `--partition`, `--apply-shift`) y pestaña «Particiones» |
| #15 | #51 | Búsqueda exprés de particiones perdidas (ADR-023): NTFS, FAT12/16/32 y exFAT con sus copias, indicadores E/P/C/S/U/A, progreso y cancelación, solo bajo petición |

**Verificación** sobre `main` (3e062b8): 331 pruebas Rust; `mr-part` contra el corpus 11/11; CLI contra el
corpus 13/13; interop C# 249/249; UI Automation 17/17; higiene sin problemas. Los 38 manifiestos coinciden
con exactitud (particiones, anomalías y 18 volúmenes encontrados). Un relleno que imita un arranque NTFS
perfecto en un sector ilegible nunca produce un volumen. La búsqueda hace las mismas lecturas en un
dispositivo virtual de 1 GiB que en uno de 2 TiB.

**Decisiones del propietario**: M2 cerrado; #17 (APM) y #18 (copia del particionado) pasan a M2.5; el
corpus NTFS de M3 se genera en un runner Linux desechable de GitHub Actions (mkntfs, ntfs-3g) y se valida con
The Sleuth Kit.

**Limitaciones que quedan**: la búsqueda exprés no encuentra volúmenes fuera de toda posición típica y sin
copia localizable (escaneo completo, M6); los volúmenes sintéticos del corpus no tienen MFT; comparación
manual con DMDE pendiente (opcional, ADR-002).

## Etapa 9 · M3 NTFS, M1.5, M2.5 e investigaciones de M4 a M7 (2026-10-25)

Trabajo en paralelo con varios agentes, cada uno en su rama y worktree, e integración centralizada por PR con
los checks obligatorios en verde (en `main` se añadió como obligatorio el job del corpus NTFS).

| Issue | PR | Resultado |
|---|---|---|
| #19 | #54 | Corpus NTFS reproducible byte a byte en un runner Linux desechable (mkntfs, ntfs-3g) con The Sleuth Kit como oráculo (ADR-024) |
| #20 | #55 | `mr-fs-ntfs`: arranque y copia, MFT, registros con fixups estrictos y estado por registro (ADR-025) |
| #21 | #56 | Atributos, runs, flujos con estado por tramo, LZNT1 contra vectores de la API de Windows, ADS; salida atómica (ADR-026) |
| #58 | #59 | La salida nunca sustituye un fichero creado a la vez (`hard_link` atómico) |
| #22 | #67 | Índices `$I30`, árbol, rutas y pestaña «Sistema de archivos» (ADR-028) |
| #23 | #62, #71 | Ficheros borrados desde la MFT y desde entradas residuales de índice, recuperabilidad por clúster y recuperación (ADR-027) |
| #18 | #60 | Copia del particionado con los bytes de las tablas, integridad y comparación |
| #17 | #64 | Apple Partition Map |
| #10 | #61 | Registro pendiente de VHDX reproducido en memoria |
| #9 | #63 | VMDK streamOptimized (`miniz_oxide`, ADR-017 addendum) |
| #11 | #69 | VMDK multiextensión y flat con confirmación explícita (`ApprovedFiles`) |
| #8 | #70 | VHD/VHDX diferenciales con cadena de padres explícita |
| #24, #28, #29, #30 | #57, #68, #65, #66 | Investigaciones INV-01 (recuperación), INV-03b (RAID), INV-03 (escaneo completo) e INV-02 (RAW), solo con fuentes públicas |

**Validación de cierre de M3** sobre `main` (9ef6918):

| Comprobación | Resultado |
|---|---|
| `scripts\test.ps1` (Debug, con corpus NTFS) | Todos los pasos correctos |
| Interop C# / UI Automation | 282/282 y 22/22 |
| `MemRescue.exe` Release publicado | Autodiagnóstico correcto con GPT 512 B y 4Kn, imágenes intactas |
| Recuperación real con la CLI Release | 3 ficheros borrados de `ntfs_deleted` con el SHA-256 del manifiesto; imagen intacta |
| Corpus NTFS | 503 rutas vivas, 419 ficheros con su SHA-256, 16 borrados con la recuperabilidad del manifiesto |

**Limitaciones que quedan**: corpus NTFS de ntfs-3g (sin volúmenes de Windows, compresión real ni
`$ATTRIBUTE_LIST`, cubiertos con vectores de Windows y volúmenes sintéticos); diferenciales de Hyper-V sin
probar; restaurar particiones en disco llega con M12.

## Siguiente tarea

1. M4 · VFS y motor de recuperación: #25 (`mr-vfs`, en integración), #26 (motor de recuperación) y #27
   (informes), sobre el diseño de INV-01 y los valores por defecto de ADR-029.
