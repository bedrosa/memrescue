# MemRescue

Herramienta de recuperación de datos, reconstrucción de sistemas de archivos y edición de disco
para Windows 11. Objetivo: la máxima paridad funcional con DMDE Professional mediante una
implementación **original** basada en especificaciones públicas y pruebas propias.

Ejecutable final: `MemRescue.exe`. Estado: **M1 en curso** (iteración 2 entregada para revisión). Aún no
recupera datos: abre en solo lectura imágenes RAW, VHD y VHDX (fijos y dinámicos), VMDK monolithicSparse e
imágenes divididas (sectores de 512 a 4096 B), presenta un rango de sectores como dispositivo, muestra y
navega sectores en hexadecimal con el estado de validez de cada uno, calcula hashes y comprueba la
legibilidad (con salto y lectura hacia atrás); el resto aparece deshabilitado con su hito.

## Compilar y probar

Requisitos ya presentes en el equipo: Rust (MSVC), Visual Studio Build Tools, Python 3, Git.
El SDK de .NET 10 está en `.tools\dotnet` (local al proyecto, no versionado).

```powershell
powershell -ExecutionPolicy Bypass -File scripts\build.ps1     # compila motor, CLI, DLL y WPF
powershell -ExecutionPolicy Bypass -File scripts\test.ps1      # 10 pasos de verificación
powershell -ExecutionPolicy Bypass -File scripts\build.ps1 -Configuration Release -Publish
#   → artifacts\MemRescue\MemRescue.exe (autocontenido, no necesita .NET instalado)
```

CLI: `target\debug\memrescue-cli.exe help`.

## Estructura

| Carpeta | Contenido |
|---|---|
| `core/` | Motor Rust: `mr-common`, `mr-io`, `mr-store` (SQLite), `mr-api`, `mr-ffi` (`memrescue_core.dll`) |
| `cli/` | `memrescue-cli` |
| `app/` | Solución .NET: `MemRescue` (WPF), `MemRescue.Core.Interop`, pruebas xUnit |
| `tests/` | Generadores, manifiestos y corpus de imágenes sintéticas |
| `scripts/` | Compilación y pruebas |
| `tools/` | Utilidades (generador del icono) |
| `docs/` | Documentación del proyecto e investigación |

## Documentación

| Documento | Para qué |
|---|---|
| [docs/PROGRESO.md](docs/PROGRESO.md) | Dónde estamos y qué sigue: **leer primero en cada sesión** |
| [docs/ROADMAP.md](docs/ROADMAP.md) | Hitos M0–M12 e investigaciones pendientes (INV) |
| [docs/PARIDAD_DMDE_PRO.md](docs/PARIDAD_DMDE_PRO.md) | Matriz de 124 funcionalidades con evidencia y estado |
| [docs/ARQUITECTURA.md](docs/ARQUITECTURA.md) | Capas, módulos, estado implementado |
| [docs/API_FFI.md](docs/API_FFI.md) | Contrato Rust ↔ C# (memoria, errores, asincronía) |
| [docs/REVISION_TECNICA_M1.md](docs/REVISION_TECNICA_M1.md) | Hallazgos de seguridad/robustez corregidos y sus pruebas |
| [docs/DECISIONES.md](docs/DECISIONES.md) | ADR (tecnología, seguridad, sala limpia) |
| [docs/PRUEBAS.md](docs/PRUEBAS.md) | Corpus, simulador, criterios de paridad, cómo ejecutar |
| [docs/PRUEBAS_UI.md](docs/PRUEBAS_UI.md) | Pruebas automáticas de la interfaz (UI Automation) |
| [docs/formatos/](docs/formatos/) | VHD, VHDX, VMDK, imágenes divididas y rangos, mapa de validez |
| [docs/HERRAMIENTAS_TERCEROS.md](docs/HERRAMIENTAS_TERCEROS.md) | qemu-img portable como referencia de pruebas (procedencia y licencias) |
| [docs/PLAN_CORPUS_NTFS.md](docs/PLAN_CORPUS_NTFS.md) | Plan de imágenes NTFS con ficheros borrados |
| [docs/investigacion/](docs/investigacion/README.md) | Ingeniería inversa de DMDE y manual oficial |

## Reglas del proyecto

- No se modifica ni se ejecuta el ejecutable de referencia `dmde.exe`, que se conserva fuera del repositorio (fases iniciales).
- No se accede a discos físicos reales (el motor rechaza las rutas de dispositivo); solo imágenes sintéticas.
- No se copia código, textos, iconos ni recursos de DMDE; no se estudian sus licencias.
- Ninguna funcionalidad se declara en paridad sin prueba reproducible.
