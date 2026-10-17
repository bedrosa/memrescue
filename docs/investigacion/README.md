# Investigación de referencia (DMDE)

Esta carpeta conserva la investigación de ingeniería inversa usada como **referencia de
comportamiento** para MemRescue. El contenido técnico de `dmde-fase1/` no se reinterpreta: si una
conclusión se corrige o amplía, se hace en un documento nuevo de fase posterior que lo indique. Para su
publicación solo se han eliminado rutas locales del equipo de análisis y notas sobre la organización del
trabajo.

## Material que no se versiona (sala limpia, ADR-001)

Se conservan **solo en local** y están excluidos de Git: la evidencia decompilada
(`dmde-fase1/evidencias-decompiladas/`) y los datos extraídos del binario (`dmde-fase1/datos/`: tablas de
imports y procedimientos, cadenas y mensajes de la interfaz). Los documentos citan esos datos por
dirección e identificador de evidencia.

Las direcciones (`0x…`), nombres `FUN_…` e identificadores de evidencia `ev_…` dentro de estos documentos
son **evidencias técnicas** y se mantienen sin cambios.

## Contenido

| Documento | Tema |
|---|---|
| [dmde-fase1/README.md](dmde-fase1/README.md) | Índice y resumen de la fase 1 |
| [01](dmde-fase1/01-identificacion-y-arquitectura.md) | Identificación, PE, toolchain, capas |
| [02](dmde-fase1/02-dependencias-y-tecnologias.md) | Dependencias e imports |
| [03](dmde-fase1/03-inventario-funciones-y-modulos.md) | Inventario de funciones y mapa de módulos |
| [04](dmde-fase1/04-mapa-funcional-interfaz.md) | Mapa funcional desde la tabla de UI |
| [05](dmde-fase1/05-algoritmos-verificados.md) | Algoritmos NTFS, GPT, ExtFS, APFS, hashes |
| [06](dmde-fase1/06-io-dispositivos.md) | E/S de dispositivos e IOCTLs |
| [07](dmde-fase1/07-particiones-y-sistemas-de-archivos.md) | Matriz de particiones y FS |
| [08](dmde-fase1/08-raid-raw-recuperacion-imagen.md) | RAID, RAW, recuperación, clonado |
| [09](dmde-fase1/09-configuracion-scripts-plantillas.md) | Configuración, scripts, plantillas |
| [10](dmde-fase1/10-funcionalidades-verificables.md) | Niveles de verificación |
| [11](dmde-fase1/11-pendientes-y-fase-2.md) | Pendientes de la fase 1 |
| [dmde-manual-oficial.md](dmde-manual-oficial.md) | Resumen de la documentación oficial consultada (fase 2) |

### Datos brutos (`dmde-fase1/datos/`)

| Fichero | Contenido | Líneas |
|---|---|---|
| `imports_por_dll.txt` | Imports agrupados por DLL | 12 |
| `procs_named.txt` | Imports y funciones con nombre | 421 |
| `strings.tsv` | Las 6 675 cadenas definidas (dirección ⇥ valor) | 6 675 |
| `ui_msgs.tsv` | Los 1 549 mensajes `ID=texto` de la interfaz | 1 549 |
| `other.tsv` | Cadenas que no son mensajes de UI | 5 126 |

**Control de versiones**: `evidencias-decompiladas/` y los volcados de textos de DMDE (`strings.tsv`,
`ui_msgs.tsv`, `other.tsv`) existen en disco pero **están excluidos de git** (`.gitignore`), por ser
material propietario de terceros. El resto de la investigación sí está versionado.

## Reglas de uso (sala limpia)

1. Estos materiales describen **qué hace** DMDE; MemRescue se implementa a partir de
   **especificaciones públicas** y pruebas propias, no traduciendo pseudocódigo.
2. `evidencias-decompiladas/` contiene salida de Ghidra de código propietario: se conserva como
   prueba de la investigación y **no debe abrirse durante la implementación**.
3. `ui_msgs.tsv` y `strings.tsv` contienen textos de DMDE: sirven para inventariar funciones;
   **no se copian** a la interfaz de MemRescue (los textos de MemRescue se redactan desde cero).
4. No se investiga ni se reproduce el sistema de licencias/activación.
