# Guía de contribución y desarrollo

MemRescue se desarrolla con un flujo controlado: **Issue → rama → commits → pruebas → PR manual → fusión
manual en `main`**. El mantenedor del repositorio crea, revisa y fusiona todas las pull requests.

## 1. Issues

- GitHub Issues es el seguimiento operativo. `docs/ROADMAP.md` describe los hitos, `docs/PROGRESO.md`
  recoge los resultados verificados y `docs/PARIDAD_DMDE_PRO.md` mide la paridad funcional. Los cuatro
  deben ser coherentes.
- Antes de empezar una tarea se busca su issue; si no existe y está bien definida, se crea (sin duplicados).
- Cada issue incluye descripción, contexto, criterios de aceptación, pruebas, dependencias y referencias,
  con un label de cada grupo y su milestone (M1 a M12):
  - Tipo: `type:feature`, `type:bug`, `type:enhancement`, `type:testing`, `type:documentation`, `type:research`.
  - Prioridad: `priority:critical`, `priority:high`, `priority:medium`, `priority:low`.
  - Área: `area:core`, `area:filesystem`, `area:recovery`, `area:ui`, `area:images`, `area:raid`,
    `area:testing`, `area:security`.
- Un issue solo se cierra cuando su solución está verificada y fusionada en `main` (la PR puede incluir
  `Fixes #N`). Los errores encontrados durante el desarrollo se registran como issues nuevos con pasos
  reproducibles, severidad e impacto.

## 2. Ramas

- `main` está protegida: sin push directo, sin force push, sin borrado, historial lineal, conversaciones
  resueltas y PR obligatoria. No se desarrolla nunca sobre `main`.
- Prefijos permitidos, en inglés, minúsculas y kebab-case, con el número del issue:
  - `feat/<issue>-<descripcion>` para funcionalidades y mejoras (p. ej. `feat/12-mbr-parser`).
  - `fix/<issue>-<descripcion>` para errores, regresiones, seguridad y ajustes (p. ej. `fix/3-pin-keyring`).
- Cada rama sale de la última versión de `main`, corresponde a un único issue y no mezcla cambios no
  relacionados. No se hacen rebases destructivos, force push ni borrado de ramas sin autorización.

## 3. Commits

- Mensajes técnicos en inglés, en imperativo y con una primera línea breve (p. ej. «Add GPT backup header
  validation»), seguida de un cuerpo que explique el qué y el porqué cuando haga falta.
- Identidad Git del mantenedor con su dirección `noreply` de GitHub.
- Sin trailers de coautoría automáticos ni menciones a herramientas de asistencia.

## 4. Pruebas antes de subir una rama

```powershell
powershell -ExecutionPolicy Bypass -File scripts\test.ps1     # 13 pasos; -SkipGui, -SkipQemu
python -I -X utf8 tools/check_repo_hygiene.py                 # auditoría de higiene
```

- Toda funcionalidad nueva lleva pruebas; ninguna funcionalidad se declara terminada sin verificarla.
- Los orígenes se abren siempre en solo lectura y las pruebas usan imágenes sintéticas desechables.
- Los sectores ilegibles nunca se presentan como datos (ver `docs/formatos/mapa-de-validez.md`).

## 5. Integración continua

`.github/workflows/ci.yml` se ejecuta en cada push a `feat/**` y `fix/**` y en cada PR hacia `main`:

| Job | Ejecutor | Comprueba |
|---|---|---|
| Repository hygiene | Ubuntu | Rutas personales, correos, credenciales, material excluido, carácter U+2014 y binarios reproducibles |
| Rust | Windows | `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, corpus sintético, validación independiente, pruebas negativas y pruebas cruzadas |
| .NET | Windows | Compilación con advertencias como errores y pruebas xUnit de interop contra la DLL real |
| Container readers | Windows | Lectores VHD/VHDX/VMDK comparados byte a byte con qemu-img (obtenido con verificación de firmas) |

Las pruebas de interfaz (UI Automation) necesitan escritorio y se lanzan a demanda con el workflow
`UI tests`. El token de Actions tiene permisos de solo lectura y no se usan secretos.

## 6. Pull requests

- El colaborador sube la rama y entrega: issue, rama, resumen técnico, commits, pruebas superadas,
  limitaciones y el enlace de comparación (`https://github.com/bedrosa/memrescue/compare/main...<rama>`).
- El mantenedor crea la PR, la revisa y la fusiona (squash o rebase). No se usa auto-merge.

## 7. Contenido del repositorio

- Implementación original basada en especificaciones públicas (ADR-001). No se incluye código, pseudocódigo
  decompilado, volcados ni recursos gráficos de DMDE.
- Sin rutas personales, correos privados, credenciales, registros de sesiones ni configuraciones privadas
  de herramientas (estas se excluyen en `.git/info/exclude`).
- Se conservan las atribuciones y licencias de terceros exigidas (`docs/HERRAMIENTAS_TERCEROS.md`).
- No se usa el carácter U+2014: se sustituye por dos puntos, coma, punto, paréntesis o guion.
