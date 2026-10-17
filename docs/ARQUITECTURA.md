# Arquitectura técnica de MemRescue

Estado: **inicial (propuesta)**. Las elecciones tecnológicas dependen de ADR-003/004/005/007/008,
pendientes de confirmación.

## 1. Objetivos de diseño

1. **Fiabilidad sobre datos dañados**: ningún parser puede provocar un fallo del proceso ante datos
   corruptos; todo error es un valor, con nivel de confianza.
2. **Nunca empeorar la situación**: solo lectura por defecto; la escritura es explícita, diferida y
   reversible (ADR-006).
3. **Escala**: discos de decenas de TB, millones de ficheros, escaneos de días con reanudación.
4. **Paridad comprobable** con DMDE Professional (ADR-002).
5. **Familiaridad** para usuarios de DMDE con identidad propia.

## 2. Vista de capas

```
┌────────────────────────────────────────────────────────────────────────────┐
│ app (C#/WPF, .NET 10)   MemRescue.exe                                      │
│  Selector de dispositivo · Particiones · Árbol+Panel · Editor hex ·        │
│  Escaneo · Recuperación · RAID · Clonado · Informes · Ajustes              │
├──────────────────────────────── FFI C (memrescue_core.dll) ────────────────┤
│ mr-api        Tareas asíncronas, progreso, cancelación, eventos, handles   │
├────────────────────────────────────────────────────────────────────────────┤
│ mr-recover    Motor de recuperación (copiado, conflictos, filtros, splits) │
│ mr-report     Informes, listas TXT/CSV/HTML, hashes, listas de sectores    │
│ mr-scan       Orquestador de escaneo completo + persistencia (SQLite)      │
│ mr-raw        Escaneo RAW por firmas + calculadoras de tamaño por formato  │
│ mr-vfs        Sistema de archivos virtual (fusión FS + encontrados + RAW)  │
├────────────────────────────────────────────────────────────────────────────┤
│ mr-fs-*       ntfs · fat · exfat · ext · hfsplus · apfs · refs · btrfs ·   │
│               iso9660   (lector + escáner de fragmentos por FS)            │
│ mr-part       MBR/EBR · GPT · APM · búsqueda de particiones perdidas       │
├────────────────────────────────────────────────────────────────────────────┤
│ mr-raid       Dispositivos virtuales: RAID 0/1/4/5/6, rotación custom,     │
│               JBOD, NULL, cálculo automático                               │
│ mr-image      RAW/IMG, imágenes divididas, VHD, VHDX, VMDK, E01 (lectura)  │
│ mr-changes    Capa de cambios diferidos (overlay), deshacer, rollback      │
├────────────────────────────────────────────────────────────────────────────┤
│ mr-io         BlockDevice trait · lector tolerante a errores (reintentos,  │
│               saltos, rellenos, mapa de sectores malos) · caché            │
│ mr-io-win     \\.\PhysicalDriveN, volúmenes, IOCTL, SCSI/ATA pass-through, │
│               SMART/NVMe, bloqueo de volúmenes                             │
├────────────────────────────────────────────────────────────────────────────┤
│ mr-common     Tipos (LBA, rangos, confianza), hashes (CRC-32, MD5, SHA-1,  │
│               SHA-256), descompresores (LZNT1, zlib, LZFSE…), logging      │
└────────────────────────────────────────────────────────────────────────────┘
       mr-clone (copia de sectores) usa mr-io + mr-raid + mr-image
       mr-hex   (modelo del editor + motor de plantillas) usa mr-io + mr-changes
       memrescue-cli usa mr-api
```

Regla de dependencias: cada capa solo depende de las inferiores. Los FS no conocen la UI ni el
escaneo global; exponen dos interfaces: **leer** un volumen y **reconocer fragmentos** en un bloque.

## 3. Abstracciones centrales

### 3.1 `BlockDevice`
Lectura (y escritura opcional) por LBA con tamaño de sector, tamaño total, identidad y
capacidades. Implementaciones: disco físico, volumen, fichero de imagen, contenedores
(VHD/VHDX/VMDK/E01), RAID virtual, partición (vista con offset), overlay de cambios, dispositivo
NULL, dispositivo simulado de pruebas con mapa de errores.

### 3.2 Lector tolerante (`ResilientReader`)
Envuelve un `BlockDevice` y aplica la política de E/S: búfer máximo, reintentos (CRC/seek),
lectura sector a sector tras error, salto de N sectores, lectura hacia atrás tras salto, patrones
de relleno para sectores malos/saltados, omisión de errores por código, reconexión. Mantiene un
**mapa de sectores** (leído / malo / saltado / no intentado) compartido con clonado e informes.

### 3.3 Resultado con confianza
Todos los validadores devuelven `Valid`, `ValidWithFixes(lista)`, `Suspicious(motivos)` o
`Invalid(motivo)`, y pueden operar en modo estricto o tolerante. (Patrón observado en DMDE, doc
`investigacion/dmde-fase1/05-algoritmos-verificados.md`, implementado de forma original.)

### 3.4 Volumen y FS virtual
Un `Volume` expone un árbol de nodos (`Node`: id, nombre(s), flujos, extents, tiempos, atributos,
estado normal/borrado/encontrado/RAW, origen del dato, confianza). `mr-vfs` fusiona: estructura
original + fragmentos encontrados + resultados RAW, con carpetas sintéticas propias
(equivalentes funcionales a "raíz", "huérfanos", "sin nombre", "RAW").

### 3.5 Escaneo completo
Recorre el dispositivo con estrategia "a saltos" (gruesa → fina), entrega cada bloque a los
reconocedores de cada FS y de RAW, agrupa fragmentos en **candidatos de volumen** con
parámetros y puntuación, y persiste todo en SQLite para pausar/reanudar y reconstruir sin
re-escanear.

## 4. Gestión de tareas

- Toda operación larga es una `Task` con id, estado, progreso (fracción + texto + contadores),
  cancelación cooperativa, pausa cuando aplique, y registro de eventos.
- Pool de hilos de CPU (`rayon`) para análisis; E/S secuencial por dispositivo para no castigar
  discos dañados (paralelismo de E/S configurable solo en imágenes/SSD sanos).
- Eventos que requieren decisión del usuario (conflicto de nombre, error de lectura con política
  "preguntar") se publican como **peticiones** que la UI o la CLI responden; con política
  automática no bloquean.
- La API FFI expone: crear tarea, consultar progreso, suscribirse a eventos, responder peticiones,
  cancelar.

## 5. Interfaz (organización funcional)

Inspirada en el flujo de trabajo de DMDE (manual oficial), con diseño y textos propios:

1. **Selector de origen**: discos físicos, volúmenes, imágenes, RAID guardados, logs de escaneo;
   botón de parámetros de E/S; opción de búsqueda rápida de particiones al abrir.
2. **Particiones**: lista con indicadores de estructuras (tabla, entrada, boot, copia, FS básico,
   espejo), encontradas/eliminadas, modo avanzado para modificar, cambios virtuales hasta aplicar.
3. **Ventana principal**: árbol de directorios + panel de ficheros + panel de búsqueda + editor,
   con disposiciones intercambiables; marcadores de estado (borrado / encontrado / RAW / confianza).
4. **Escaneo completo**: parámetros, progreso, lista de volúmenes candidatos con calidad, versiones.
5. **Reconstrucción**: modos rápido / por defecto / completo / personalizado y control Más/Menos.
6. **Recuperación**: categorías, filtros, destino, conflictos, errores, informe y hashes.
7. **RAID**: constructor, rotación personalizada, cálculo automático con restricciones.
8. **Copia de sectores**: origen/destino, rangos, inversa, log/reanudación, mapfile ddrescue.
9. **Editor hex**: vistas por plantilla, navegación por estructuras, modo edición con colores de estado.
10. Menús organizados por **Disco / Herramientas / Ventanas / Editor / Modo / Edición** para que el
    usuario de DMDE encuentre las funciones en el mismo lugar lógico.

Todo control sin implementar se muestra deshabilitado con indicación "pendiente" (no se ocultan
ni se simulan).

## 6. Seguridad y calidad

- `#![forbid(unsafe_code)]` en todos los crates salvo `mr-io-win` y la frontera FFI, que aíslan el `unsafe`.
- Límites explícitos de memoria/tiempo en parsers (tamaños máximos, profundidad de árboles, ciclos).
- Fuzzing de cada parser; pruebas con imágenes corrompidas de forma determinista.
- Escritura física deshabilitada en compilación hasta el hito correspondiente (ADR-006).

## 7. Estado implementado (M0, 2026-10-17)

| Componente | Ubicación | Qué hace hoy | Pruebas |
|---|---|---|---|
| `mr-common` | `core/mr-common` | `Lba`, `ByteRange` con aritmética comprobada; `Confidence`; hashes CRC-32/MD5/SHA-1/SHA-256 incrementales | 6 unitarias (vectores conocidos) |
| `mr-io` | `core/mr-io` | Trait `BlockDevice`; `ImageFile` (solo lectura, lectura posicional, rechaza rutas de dispositivo); `MemDevice` | 5 unitarias |
| `mr-store` | `core/mr-store` | SQLite v1: fuentes, trabajos (máquina de estados), checkpoints, eventos; WAL; recuperación tras cierre abrupto | 6 unitarias |
| `mr-api` | `core/mr-api` | `Task<T>` (hilo, progreso atómico, cancelación cooperativa); `image_info`; tarea de hash con registro opcional en SQLite | 6 unitarias |
| `mr-ffi` | `core/mr-ffi` | `memrescue_core.dll`, ABI 1 ([API_FFI.md](API_FFI.md)) | 7 unitarias |
| `memrescue-cli` | `cli/` | `version`, `image-info`, `hash`, `db-init`, `jobs` | 4 extremo a extremo + 2 contra corpus |
| `MemRescue.Core.Interop` | `app/src/MemRescue.Core.Interop` | Cliente .NET seguro de la DLL (`Engine`, `HashTask`) | 17 xUnit (14 + 3 corpus) |
| `MemRescue` (WPF) | `app/src/MemRescue` | Ventana principal con menús del flujo de referencia; abrir imagen, propiedades, hashes con progreso/cancelación, registro; modo `--selftest` y `--screenshot` | Autodiagnóstico en `scripts/test.ps1` |
| Generador y validador | `tests/generators` | Imágenes MBR/GPT deterministas + validador independiente + pruebas negativas | 128 comprobaciones + 9 corrupciones |

Fuera de M0 (no existe todavía): lectura de particiones, sistemas de archivos, escaneo, recuperación,
RAID, clonado, editor hex, acceso a dispositivos.

## 8. Estructura del repositorio

```
memrescue/
  core/            workspace Rust (crates mr-*)
  cli/             memrescue-cli
  app/             MemRescue (WPF) + MemRescue.Core.Interop + pruebas xUnit
  tests/
    generators/    scripts para crear imágenes sintéticas
    corpus/        imágenes pequeñas versionadas + manifiestos con hashes esperados
  docs/            esta documentación + investigacion/
```
