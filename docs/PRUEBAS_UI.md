# Pruebas de la interfaz (UI Automation)

Proyecto: `app/tests/MemRescue.UiTests` (xUnit, `net10.0-windows`). Prueban la **aplicación real**:
lanzan el `MemRescue.exe` compilado como un proceso aparte, con `--open <imagen>`, y lo manejan desde
fuera mediante UI Automation (`System.Windows.Automation`, cliente UIA de Windows), igual que lo haría
un lector de pantalla o un usuario: escriben en cajas de texto, pulsan botones (`InvokePattern`), abren
menús (`ExpandCollapsePattern`) y leen los textos que muestra la ventana.

Complementan al autodiagnóstico `--selftest` (que se ejecuta dentro del proceso) y a las pruebas de
interop (`MemRescue.Core.Interop.Tests`), que no pasan por la interfaz.

## Arranque con imagen: `--open`

```
MemRescue.exe --open <ruta-de-imagen> [--sector-size 512|1024|2048|4096]
```

Abre la imagen en solo lectura en cuanto la ventana termina de mostrarse, con la misma lógica que
«Disco → Abrir imagen…» (`MainWindow.OpenImageAsync`, modo interactivo: si el fichero no existe o parece
un contenedor no soportado se muestra el mismo aviso que al abrirlo desde el menú). Sin `--sector-size`
se usa 512. Argumentos no válidos → mensaje de error y código de salida 2. Se ignora si se usa
`--selftest`, cuyo comportamiento no cambia.

## Qué cubren

Imágenes sintéticas deterministas de `tests/generators/gen_partition_images.py`, generadas una sola
vez por ejecución en un directorio temporal propio (`%TEMP%\memrescue-uitests-*`) que se borra al final.

| Prueba | Comprueba |
|---|---|
| `Abre_la_imagen_indicada_con_open` | Ventana «MemRescue»; `--open` carga `gpt_basic.img`; el visor muestra la LBA 0 con sectores de 512 B; la lista de orígenes contiene exactamente `gpt_basic.img`. |
| `Navegacion_con_caja_LBA_y_botones` | «1» en `LbaBox` + «Ir» → línea `0x200` con `45 46 49 20 50 41 52 54` y `EFI PART`; ▶ → `SectorInfoText` «LBA 2 de …»; \|◀ → LBA 0 con `55 AA` en `0x1FE` (MBR protector); ◀ en la LBA 0 no sale del rango; ▶\| → LBA 32767 con la cabecera GPT de respaldo (`EFI PART`) y `PositionText` «LBA 32767 / 32767». |
| `LBA_fuera_de_rango_no_cambia_la_vista` | `99999999` y `xyz` → `StatusText` «LBA no válida (0 – 32767)», y el visor y `SectorInfoText` no cambian; una LBA válida posterior (`0x2`) vuelve a funcionar y restablece «Listo». |
| `Menus_pendientes_deshabilitados_e_ir_a_sector_habilitado` | Disco → «Particiones…» deshabilitado; Editor → «Ir a sector…» habilitado con una imagen abierta e «Ir a cluster / registro…» deshabilitado; la aplicación se cierra limpiamente después. |
| `Sector_de_4096_bytes` | `gpt_4kn.img` con `--sector-size 4096`: «sector de 4096 B»; ▶ → LBA 1 en `0x1000` con `EFI PART`; la página tiene 256 líneas de volcado (4096 B). |
| `Particion_MBR_en_LBA_2048` | `mbr_basic.img`: la LBA 2048 empieza por el marcador `MEMRESCUE-TEST|mbr_basic|P1|LBA=…`. |
| `Contenedor_danado_pide_confirmacion_para_el_modo_tolerante` | VHD dinámico con una entrada de la BAT fuera del fichero: la apertura estricta falla y aparece el diálogo «Modo tolerante» (con «MODO TOLERANTE» y «1 bloque descartado»); «No» → ningún origen abierto; «Sí» → visor en la LBA 0, aviso `TolerantBannerText` visible, la LBA 8 muestra `BAD!` y el aviso de no fiables; el fichero no cambia. |
| `Las_imagenes_no_se_modifican` | SHA-256, fecha de modificación y tamaño de las tres imágenes idénticos antes y después de abrirlas y navegar por ellas en tres instancias; cada instancia termina con código 0. |

Para localizar controles se usan los `x:Name` existentes (`LbaBox`, `HexView`, `SectorInfoText`,
`PositionText`, `StatusText`, `SourcesList`) y `AutomationProperties.AutomationId` añadidos en
`MainWindow.xaml` a los botones de navegación (`FirstSectorButton`, `PrevSectorButton`, `GoLbaButton`,
`NextSectorButton`, `LastSectorButton`) y a algunos menús (`DiskMenu`, `PartitionsMenuItem`, `EditorMenu`,
`GoToSectorMenuItem`, `GoToClusterMenuItem`). Las comprobaciones numéricas no dependen del separador de
miles de la referencia cultural del sistema.

## Cómo ejecutarlas

Con el SDK local del proyecto (PowerShell, desde la raíz del repositorio):

```powershell
$env:DOTNET_ROOT = "$PWD\.tools\dotnet"; $env:DOTNET_CLI_TELEMETRY_OPTOUT = '1'; $env:DOTNET_NOLOGO = '1'
.\.tools\dotnet\dotnet.exe build app\MemRescue.slnx -c Debug
.\.tools\dotnet\dotnet.exe test app\tests\MemRescue.UiTests\MemRescue.UiTests.csproj -c Debug --no-build
```

`dotnet test app\MemRescue.slnx` ejecuta también estas pruebas junto con las de interop (estas
últimas necesitan antes el corpus en `tests\corpus\generated`, p. ej. con `scripts\test.ps1`).

- Se lanza el `MemRescue.exe` de la misma configuración con la que se compilaron las pruebas
  (`app\src\MemRescue\bin\<Config>\net10.0-windows\win-x64\`), localizado subiendo desde la carpeta de
  las pruebas hasta encontrar `app\MemRescue.slnx`. El proyecto de pruebas referencia a `MemRescue.csproj`
  (sin usar su ensamblado) para que la compilación lo genere antes.
- Variables opcionales: `MEMRESCUE_EXE` (otro ejecutable) y `MEMRESCUE_PYTHON` (intérprete de Python;
  por defecto `python` del PATH).

## Requisitos

- **Sesión de escritorio interactiva y desbloqueada** (la ventana tiene que crearse y renderizarse).
  No funcionan en un servicio, en una sesión sin escritorio ni con la pantalla bloqueada.
- Windows con .NET 10 (SDK local) y Python 3 para el generador de imágenes.
- No requieren administrador ni acceden a discos: solo ficheros sintéticos en `%TEMP%`.
- No hace falta el foco del teclado: todo se hace con patrones de UI Automation, sin simular
  pulsaciones ni ratón, así que se puede seguir usando el equipo (salvo bloquear la sesión).

## Robustez

- Todas las esperas son sondeos cada 50 ms con tiempo límite (20 s para las interacciones; 90 s para el
  arranque, porque el primer lanzamiento de un ejecutable recién compilado puede tardar por el análisis
  antivirus). Al vencer, el error indica qué se esperaba y qué mostraba el visor.
- Las pruebas no se ejecutan en paralelo (una ventana cada vez).
- Cada prueba cierra su instancia en `Dispose` aunque falle: `WM_CLOSE` (como el botón «Cerrar»),
  espera de hasta 10 s y, si no ha terminado, `Kill` del árbol de procesos. No quedan procesos huérfanos
  salvo que se aborte el propio proceso de pruebas (p. ej. matando `testhost`).

## Limitaciones conocidas

- `WindowPattern.Close` de UIA se ignora si antes se ha abierto un menú por UI Automation; por eso el
  cierre usa `WM_CLOSE` (`Process.CloseMainWindow`) y deja `WindowPattern.Close` como alternativa.
- No se prueban los cuadros de diálogo modales («Abrir imagen…», «Ir a sector…», avisos): bloquearían la
  prueba si algo fallara y su lógica ya está cubierta por otras vías. Tampoco hashes ni legibilidad desde
  la pestaña «Comprobaciones» (cubiertos por `--selftest` y las pruebas de interop).
- El aspecto visual (colores, disposición) no se verifica; para eso está la captura `--screenshot`.
- Si se cuelga `testhost` a mitad de una prueba, puede quedar abierta una ventana de MemRescue (no hay
  objeto de trabajo de Windows que la ate al proceso de pruebas).

## Resultados reales (2026-10-17, Windows 11 Pro 26200, sesión interactiva)

- `dotnet build app\MemRescue.slnx -c Debug`: 0 advertencias, 0 errores.
- `MemRescue.UiTests`: **7 de 7 superadas**, ~12–16 s en total (≈1 s por prueba; 3 s la de
  integridad, que lanza tres instancias).
- `dotnet test app\MemRescue.slnx`: 47/47 de interop + 7/7 de UI, repetido varias veces seguidas sin fallos
  y sin procesos `MemRescue` residuales. Durante el desarrollo hubo un fallo aislado (1 de 7, con
  ≈20 s de más, compatible con agotar el límite de espera que entonces era de 20 s también para el
  arranque) justo después de regenerar el corpus y recompilar; no se pudo reproducir en ejecuciones
  repetidas ni se capturó su mensaje. Por precaución el límite de arranque se elevó a 90 s.
