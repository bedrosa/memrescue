# Imágenes divididas y vistas de rango

Este documento describe dos dispositivos de bloques virtuales de **solo lectura** de MemRescue:

- `mr_io::view::RangeView`: un rango de sectores de otro dispositivo expuesto como dispositivo
  independiente (partición, volumen dentro de una imagen…).
- `mr_image::split::SplitImage`: una imagen de disco repartida en varios ficheros consecutivos
  (`x.001`, `x.002`…), concatenados como un único disco.

Ambos son implementaciones originales; no se basan en código ni material de terceros.

## 1. Vista de rango (`RangeView`)

```rust
pub fn RangeView::new(inner: Arc<dyn BlockDevice>, start_lba: u64, sector_count: u64) -> Result<RangeView>
pub fn start_lba(&self) -> u64
pub fn sector_count(&self) -> u64
pub fn inner(&self) -> &Arc<dyn BlockDevice>
impl BlockDevice for RangeView
```

Validaciones al crearla (error `IoError::InvalidRange`):

- `sector_count > 0`;
- `start_lba + sector_count` sin desbordamiento (`checked_add`);
- el rango cabe en los **sectores completos** del interior (`≤ inner.sector_count()`): los bytes
  finales del interior que no completan un sector nunca se exponen;
- los productos por el tamaño de sector también se comprueban.

Comportamiento:

- `size = sector_count × sector_size`; el tamaño de sector es el del interior.
- `read_at` comprueba primero los límites **de la vista** y después traduce el desplazamiento con
  `checked_add`. Una lectura que cruza el final de la vista falla con `OutOfBounds` aunque el
  interior tenga datos ahí, y el búfer no recibe datos de fuera del rango.
- `allocation(lba, count)` valida el rango local, delega en el interior con `start_lba + lba` y
  devuelve los tramos con LBA locales. Si el interior devolviera un tramo fuera de la vista se
  informa como `Corrupt` en lugar de devolver LBA incoherentes.
- Las vistas se pueden anidar (vista sobre vista); los desplazamientos se acumulan y cada nivel
  limita al anterior.

## 2. Imágenes divididas (`SplitImage`)

```rust
pub const MAX_SEGMENTS: usize = 10_000;
pub fn discover(first: &Path) -> mr_io::Result<Vec<PathBuf>>
pub fn SplitImage::open(paths: &[PathBuf], sector_size: u32) -> mr_io::Result<SplitImage>
pub fn segments(&self) -> &[PathBuf]
pub fn anomalies(&self) -> &[String]
pub fn describe(&self) -> String
impl BlockDevice for SplitImage
```

### 2.1 Convenciones de nombres admitidas

| Regla | Ejemplos válidos | No válidos |
|---|---|---|
| Sufijo decimal de **3 o más dígitos** tras el **último** punto | `disco.001`, `disco.img.001`, `x.0001` | `x.01`, `x.1`, `x.e01`, `x.001.bak`, `x.003x` |
| Nombre base (antes del último punto) no vacío | `a.001` | `.001` |
| El primer segmento es `…001` o `…000` | `x.000`, `x.001` | `x.002` (no es el primero) |
| Los siguientes tienen el número siguiente con el **mismo ancho** que el primero | `x.0001` → `x.0002` | `x.001` → `x.0002` (otro conjunto) |
| Al superar el ancho, el número crece de forma natural | `x.999` → `x.1000` | |

El nombre base puede contener puntos, espacios y caracteres no ASCII (`disco con espacios y ñ.001`)
y debe ser Unicode válido. No se admiten otras convenciones (`.aa/.ab` de `split`, `.E01/.E02`,
`-s001.vmdk`, `.part1`…): el formato EWF y los VMDK divididos son contenedores distintos.

### 2.2 Descubrimiento (`discover`)

- Busca **solo en el directorio del primer segmento**, probando los nombres `base.(n+1)`,
  `base.(n+2)`… con el mismo ancho. No recorre otros directorios ni usa comodines: así no se
  mezclan conjuntos (p. ej. `a.001` con `a.img.001`, o `a.001` con `a.0001`, en la misma carpeta)
  ni se toman ficheros de ubicaciones inesperadas.
- Se **detiene en el primer número que falta**. Si hubiera segmentos posteriores a un hueco
  (`x.001`, `x.002`, `x.004`) no se incluyen; el tamaño resultante será menor que el del disco
  original y la interfaz debe mostrarlo.
- Si se pasa `x.001` y existe `x.000`, falla con `ExternalReference("x.000")`: empezar por `x.001`
  perdería el principio del disco.
- Cada segmento debe pasar `mr_io::path_policy::admit` (fichero normal; nunca directorio,
  dispositivo ni enlace a ellos). Un segmento que es un directorio produce `DevicePathRejected`.
- Más de 10 000 segmentos → `UnsupportedFeature` (no se trunca en silencio).
- **No abre el contenido** de ningún segmento (solo consulta metadatos y la ruta canónica). La
  interfaz muestra la lista y pide confirmación antes de llamar a `open`, igual que con otros
  ficheros externos (padres de VHD, extensiones de VMDK): el usuario decide qué se abre.

### 2.3 Apertura (`open`) y validaciones

- Tamaño de sector válido (potencia de 2 entre 512 y 65 536); lista no vacía; ≤ 10 000 rutas.
- Cada ruta pasa `admit` + `open_regular_file` en **solo lectura**; se guarda la ruta canónica
  (es lo que devuelve `segments()`).
- Ruta repetida (misma ruta canónica) → `Corrupt`.
- Segmento **intermedio** de tamaño 0 → `Corrupt`. Imagen totalmente vacía → `Corrupt`.
- Tamaño total = suma comprobada (`checked_add`); desbordamiento → `Corrupt`.
- Se permiten tamaños distintos; se registran como anomalías (visibles en `describe()` y
  `anomalies()`):
  - segmento intermedio con tamaño distinto del primero (p. ej. menor que los anteriores);
  - último segmento mayor que el primero;
  - último segmento vacío;
  - tamaño total no múltiplo del sector (los bytes finales no forman un sector completo).
- No se detecta el formato del contenido: el conjunto se trata como RAW concatenado.

### 2.4 Lectura

- Límites comprobados (`OutOfBounds`), incluido `u64::MAX`.
- El segmento inicial se localiza por búsqueda binaria sobre los desplazamientos de inicio; la
  lectura continúa por los segmentos siguientes si cruza fronteras.
- Lectura posicional (`seek_read`/`read_at`), sin cursor compartido: segura entre hilos.
- Si un segmento se acorta después de abrirlo, se devuelve `IoError::Read` con el desplazamiento
  **global** y la longitud del tramo de ese segmento que falló; nunca datos inventados.

### 2.5 Descripción

`describe()` devuelve, por ejemplo:

```
Imagen dividida en 4 segmentos (x.001–x.004), 3,5 MiB (3670016 bytes)
Imagen dividida en 1 segmento (v.001), 1,5 KiB (1536 bytes)
```

seguido de `; anomalías: …` si las hay.

## 3. Límites conocidos

- Solo la convención numérica descrita; sin `.aa/.ab` ni `.part1`.
- No se comprueba que los segmentos pertenezcan realmente al mismo volcado (RAW no tiene cabecera
  común); la coherencia queda en manos del usuario y de los avisos de anomalías.
- Un hueco en la numeración trunca el conjunto en ese punto.
- En Windows la búsqueda de nombres es insensible a mayúsculas (lo determina el sistema).
- `admit` resuelve la ruta canónica (el sistema puede crear internamente un identificador sin
  acceso a datos), pero `discover` no lee contenido.

## 4. Pruebas

Todas con ficheros temporales sintéticos (`tempfile`), sin herramientas externas, discos físicos
ni privilegios.

`core/mr-io/src/view.rs` (8 pruebas unitarias): límites exactos (primer/último sector, uno más
allá), sector parcial final del interior no expuesto, lectura que cruza el final con datos en el
interior, desbordamientos con `u64::MAX` (incluido un interior de tamaño `u64::MAX`), anidamiento,
asignación desplazada con un dispositivo de prueba con tramos `Sparse` (simple y anidada), interior
incoherente → `Corrupt`, interior sin dispersión → `Data`.

`core/mr-image/src/split.rs` (2 unitarias): análisis de nombres y formato de tamaños.

`core/mr-image/tests/split.rs` (15 de integración):

- RAW determinista (xorshift) dividido en: segmentos iguales (4 × 256 KiB, sector 512); último
  menor (sector 4096); tamaños no alineados a sector (100 003 bytes); un único segmento; 64
  segmentos de 513 bytes. En cada caso: SHA-256 de la lectura completa (bloques de un sector,
  4 KiB, 64 KiB, 7 777 bytes y de una vez) igual al SHA-256 del RAW; lecturas que cruzan cada
  frontera; límites; sectores; e **inmutabilidad** (SHA-256, fecha de modificación y tamaño de
  cada segmento antes y después).
- `describe` y anomalías (intermedio menor, último vacío, último mayor, total no múltiplo del
  sector).
- Rechazos de `open`: intermedio vacío, todo vacío, lista vacía, repetido, sector inválido,
  directorio, ruta de dispositivo, inexistente.
- Segmento acortado tras abrir → `Read` con desplazamiento global.
- `discover`: hueco en la numeración, varios conjuntos y nombres raros en el mismo directorio,
  conjuntos desde `.000`, `.001` con `.000` presente, segmentos que son directorios, inmutabilidad,
  límite de 10 000 segmentos y crecimiento del ancho (`k.999` → `k.1000`).

Resultado real (Windows 11, `cargo test -p mr-io -p mr-image`): mr-io 34 pruebas correctas (8 de
vistas), mr-image 2 unitarias + 15 de integración correctas, 0 fallos (≈ 8 s, dominados por la
prueba que crea 10 001 ficheros). `cargo clippy -p mr-io -p mr-image --all-targets -- -D warnings`
y `cargo fmt --all --check` sin avisos.
