# VMDK (monolithicSparse, streamOptimized y descriptor separado): lector de solo lectura

Implementación: `core/mr-image/src/vmdk.rs` (`mr_image::vmdk::VmdkImage`) y, para lo propio de
streamOptimized (marcadores, pie, descompresión y caché), `core/mr-image/src/vmdk_stream.rs`. Los
discos con descriptor de texto separado (monolithicFlat, twoGbMaxExtent*) están en
`core/mr-image/src/vmdk_extents.rs` (`VmdkExtentImage`, incidencia 11) y los ficheros que aprueba el
usuario, en `core/mr-image/src/external.rs` (`ApprovedFiles`); ver «Descriptor separado» más abajo.
Pruebas: unitarias en los mismos ficheros, cruzadas contra `qemu-img` en
`core/mr-image/tests/vmdk_qemu.rs` y de corrupción en `core/mr-image/tests/corruption.rs`.

Implementación original basada en la especificación pública de VMware
«Virtual Disk Format 1.1». No se ha usado material de terceros (ni de DMDE).

## Especificación (resumen de lo que se usa)

### Cabecera de extensión dispersa (sector 0, little-endian)

| Desplazamiento | Tamaño | Campo | Uso |
|---|---|---|---|
| 0 | 4 | `magicNumber` = `KDMV` | obligatorio |
| 4 | 4 | `version` | 1, 2 o 3 |
| 8 | 4 | `flags` | bit 0 prueba de fin de línea, bit 1 GT redundante, bit 2 GTE «zeroed-grain», bit 16 comprimido, bit 17 marcadores |
| 12 | 8 | `capacity` | sectores de 512 bytes → `size()` = capacity × 512 |
| 20 | 8 | `grainSize` | sectores por grano |
| 28 | 8 | `descriptorOffset` | sector del descriptor incrustado |
| 36 | 8 | `descriptorSize` | sectores del descriptor |
| 44 | 4 | `numGTEsPerGT` | debe ser 512 |
| 48 | 8 | `rgdOffset` | GD redundante (no se usa) |
| 56 | 8 | `gdOffset` | sector del GD principal; `0xFFFF…FFFF` (GD_AT_END) = GD en el pie (streamOptimized) |
| 64 | 8 | `overHead` | no se usa |
| 72 | 1 | `uncleanShutdown` | solo informativo (`describe()`) |
| 73 | 4 | `'\n' ' ' '\r' '\n'` | prueba de fin de línea |
| 77 | 2 | `compressAlgorithm` | 0 (monolithicSparse) o 1 = deflate (streamOptimized) |

### Directorio y tablas de granos

- GD en `gdOffset` (sectores): `ceil(ceil(capacity / grainSize) / 512)` entradas `u32`, cada
  una el sector de una tabla de granos (GT). Entrada 0 → GT ausente: sus 512 granos son dispersos.
- Cada GT: 512 entradas `u32` = sector del grano. 0 → no asignado (ceros). 1 con el bit 2 de
  `flags` → grano a cero (ceros).
- monolithicSparse: el dato del byte virtual `p` está en `GT[g % 512]·512 + p % (grainSize·512)`,
  con `g = p / (grainSize·512)`.

### streamOptimized (granos comprimidos con marcadores)

Indicadores 16 y 17 activos, `compressAlgorithm` = 1 y `createType="streamOptimized"`.

- **Grano**: la entrada de GT apunta al sector de su **marcador de grano**: `lba: u64` (primer sector
  virtual del grano), `size: u32` (bytes comprimidos) y, a continuación, `size` bytes de deflate en
  **formato zlib** (RFC 1950: cabecera de 2 bytes, datos y **Adler-32** de los datos descomprimidos),
  rellenado hasta el sector. Descomprimido mide un grano entero o, en el último grano de un disco
  cuya capacidad no es múltiplo del grano, solo la parte que cubre el disco (qemu-img lo escribe así).
- **Marcadores de metadatos** (un sector): `numSectors: u64`, `size: u32` = 0, `type: u32`:
  0 fin de flujo, 1 GT, 2 GD, 3 pie.
- **GD al final**: si la cabecera trae `gdOffset` = GD_AT_END, el GD real está en el **pie**: los
  últimos 1536 bytes del fichero son el marcador del pie (tipo 3), el pie (copia de la cabecera con
  el `gdOffset` real) y el marcador de fin de flujo. Es la disposición de VMware (GT y GD escritos
  después de los granos). qemu-img, en cambio, deja el GD y las GT al principio con `gdOffset` en la
  cabecera, y no siempre escribe el fin de flujo.

### Descriptor incrustado

Texto `clave=valor` con líneas de extensión `ACCESO SECTORES TIPO ["FICHERO" [DESPL]]`.
`parse_descriptor(&str)` devuelve `VmdkDescriptor` con `version`, `cid`, `parent_cid`,
`create_type`, `parent_file_name_hint` y las extensiones (`access`, `sectors`, `kind`,
`file_name`, `offset`). Admite BOM UTF-8, CRLF, comentarios `#`, comillas y nombres con espacios;
ignora claves informativas (`ddb.*`, `encoding`…).

## Variantes

| Variante | Resultado |
|---|---|
| `monolithicSparse` (cabecera v1, v2, v3; con o sin «zeroed-grain») | **soportada** |
| `streamOptimized` (bits 16 y 17, deflate; GD en la cabecera o en el pie) | **soportada** (incidencia 9) |
| Granos comprimidos sin marcadores, marcadores sin compresión, `compressAlgorithm` distinto de 1 con compresión, o GD al final / algoritmo sin los indicadores de compresión | `UnsupportedFeature` |
| streamOptimized con granos de más de 2¹⁵ sectores (16 MiB; VMware y qemu usan 64 KiB) | `UnsupportedFeature` |
| Cabecera de streamOptimized con `createType` monolithicSparse, o al revés | `Corrupt` (incoherente) |
| Descriptor de texto separado abierto con la detección automática (`open_image`) | `ExternalReference` con los nombres (como texto) de los ficheros de extensión, que no se abren; si el descriptor no se puede interpretar, `UnsupportedFeature` |
| Descriptor separado `monolithicFlat`, `twoGbMaxExtentFlat`, `twoGbMaxExtentSparse` o `monolithicSparse` con extensiones FLAT, SPARSE y ZERO, abierto con `open_vmdk_extents` y los ficheros aprobados uno a uno | **soportado** (incidencia 11; ver «Descriptor separado») |
| Descriptor separado `vmfs*`, `streamOptimized`, `fullDevice`, `custom`…, o extensiones VMFS, VMFSSPARSE, VMFSRDM, VMFSRAW, SESPARSE | `UnsupportedFeature` |
| Extensión dispersa sin descriptor incrustado (`descriptorOffset` = 0, p. ej. `-s001.vmdk`) abierta sola | `UnsupportedFeature` (se lee como extensión de su descriptor) |
| Firma `COWD` (ESX 2.x) | `UnsupportedFeature` |
| Versión fuera de 1–3, indicadores desconocidos | `UnsupportedFeature` |
| Otro `createType` con cabecera `KDMV` | `UnsupportedFeature` |
| Disco hijo (`parentCID` ≠ `ffffffff` o `parentFileNameHint`) | `ExternalReference` |
| Varias extensiones, extensión no SPARSE con fichero, o nombre de extensión con ruta (`/`, `\`, `:`, `..`, UNC, espacios en los extremos) | `ExternalReference` |
| Cualquier otra incoherencia | `Corrupt { format: "VMDK" }` |

El nombre de la extensión única SPARSE no se compara con el nombre real del fichero (el lector
no lo conoce y renombrar un `.vmdk` es habitual): por definición, en monolithicSparse y
streamOptimized esa extensión es el propio fichero. Si el nombre parece una ruta se trata como
referencia externa.

## Descriptor separado con extensiones (incidencia 11)

Un descriptor de texto (`# Disk DescriptorFile`, ≤ 1 MiB) describe el disco virtual como la
concatenación, en orden, de sus líneas de extensión `ACCESO SECTORES TIPO ["FICHERO" [DESPL]]`. qemu-img
genera `monolithicFlat` (`x.vmdk` + `x-flat.vmdk`), `twoGbMaxExtentFlat` (`x-f001.vmdk`, `x-f002.vmdk`…)
y `twoGbMaxExtentSparse` (`x-s001.vmdk`…) en trozos de **exactamente 2 GiB** (4 194 304 sectores; no hay
opción para cambiarlo); cada extensión SPARSE es una cabecera `KDMV` con `capacity` = sectores de la
extensión y un descriptor incrustado vacío (a ceros, `descriptorOffset` = 1).

**Apertura explícita (ADR-018, addendum).** El motor nunca abre un fichero que nombra el descriptor:

1. `list_vmdk_extents(descriptor)` / `mr_api::vmdk_required_extents` / `mr_vmdk_extents_list` /
   `memrescue-cli vmdk-extents` interpretan el descriptor y devuelven cada extensión (acceso, tipo,
   sectores, desplazamiento, primer sector virtual, nombre **exacto** declarado, si necesita fichero y su
   problema, si lo hay) **sin abrir ninguna**.
2. El usuario asigna a cada nombre necesario un fichero (en la interfaz se sugiere, sin abrirlo, el del
   mismo directorio con el nombre exacto, y se confirma uno a uno; en la CLI, `--extent NOMBRE=RUTA` por
   cada extensión).
3. `open_vmdk_extents(descriptor, &ApprovedFiles, opts)` abre **solo** esos ficheros. `ApprovedFiles`
   admite cada ruta con la política de rutas (ADR-014) y la abre en solo lectura; la búsqueda por nombre
   es por coincidencia exacta y nunca toca el sistema de archivos.

No existe una opción «todas las extensiones del directorio» (`--extents-from-dir` se rechaza a
propósito): un descriptor puede nombrar ficheros que el usuario no ha revisado (otro disco, un fichero
de datos) y el motor los leería como parte del disco; cada fichero debe nombrarse o confirmarse.

**Validaciones** (antes de abrir nada salvo el descriptor):

- `createType` ∈ {monolithicFlat, twoGbMaxExtentFlat, twoGbMaxExtentSparse, monolithicSparse} (sin
  distinguir mayúsculas); otro → `UnsupportedFeature`. Disco con padre (`parentFileNameHint` o
  `parentCID` ≠ `ffffffff`) → `ExternalReference` (padres fuera del alcance).
- Al menos una extensión y como mucho 32 768; ninguna de 0 sectores; la suma de sectores (= tamaño
  virtual; el descriptor no declara otro) sin desbordar `u64::MAX / 512` → si no, `Corrupt`.
- Tipos: FLAT, SPARSE (sin desplazamiento distinto de 0) y ZERO (sin fichero). VMFS, VMFSSPARSE,
  VMFSRDM, VMFSRAW y SESPARSE → `UnsupportedFeature`. FLAT con `desplazamiento + sectores` que desborda →
  `Corrupt`.
- Nombre de fichero: solo **nombres simples**. Se rechazan (`UnsupportedFeature`, aunque el usuario los
  apruebe) los vacíos, con `..`, con `/` o `\` (rutas absolutas o con directorios, UNC, `\\?\…`,
  `\\.\…`), con `:` (unidad, flujo alternativo), con `<>"|?*` o caracteres de control, con espacios
  en los extremos o punto final, los nombres de dispositivo DOS (`NUL`, `COM1`, `con.vmdk`…) y los que no
  son UTF-8 válido.
- Nombres repetidos (sin distinguir mayúsculas, el mismo fichero en NTFS) → `Corrupt`.
- Ficheros aprobados: cada extensión necesaria (FLAT o SPARSE con acceso RW o RDONLY) debe tener uno
  (si faltan → `ExternalReference` con los nombres que faltan); un fichero aprobado para un nombre que no
  es una extensión necesaria, el propio descriptor, dos nombres con el mismo fichero o un nombre
  repetido son un error del llamador (`InvalidInput`; `InvalidArgument` en la API y la ABI).
- FLAT: el fichero debe medir al menos `(desplazamiento + sectores) × 512` → si no, `Corrupt`.
- SPARSE: se abre con el lector de monolithicSparse / streamOptimized como extensión
  (`VmdkImage::open_extent`): misma validación de cabecera, GD, GT y granos (y modo tolerante); el
  descriptor incrustado puede faltar o estar vacío (si tiene texto y declara padre → `ExternalReference`)
  y `capacity` debe ser exactamente los sectores declarados → si no, `Corrupt`. La especificación solo
  define streamOptimized como disco monolítico; se acepta también como extensión porque el lector es el
  mismo (probado con un descriptor escrito a mano y una extensión streamOptimized de qemu-img).

**Lectura.** FLAT: bytes del fichero desde `desplazamiento × 512` (`Data` en `allocation`). SPARSE: el
lector de la extensión, con sus dispersos y granos descartados. ZERO: ceros (`Sparse`). NOACCESS: el
fichero no se abre ni se exige; `read_at` devuelve `IoError::Read` (`PermissionDenied`) con el rango
virtual, `allocation` lo informa como `Data` y el lector tolerante lo marca `Unreadable` con el patrón de
relleno: nunca datos. Un fichero FLAT acortado tras abrirlo da error de lectura. `describe()`: p. ej.
«VMDK twoGbMaxExtentSparse (descriptor separado), 3 extensiones (SPARSE ×3)». Los ficheros de extensión
abiertos se guardan como `segments` del origen: nunca se aceptan como destino de una extracción.

**Lo que no se comprueba.** El descriptor no guarda ninguna suma de las extensiones ni su identidad
(salvo el `CID`, que no se compara con nada): si el usuario aprueba un fichero equivocado del tamaño
adecuado, se lee como si fuera la extensión. Por eso la interfaz muestra el nombre declarado junto al
fichero elegido y exige confirmar cada uno.

## Validaciones

- Fichero ≥ 512 bytes, firma `KDMV`; si no, se distingue descriptor de texto, `COWD` o `Corrupt`.
- Caracteres de prueba de fin de línea exactos cuando el bit 0 está activo (detecta transferencias
  en modo texto) → `Corrupt`.
- `grainSize` potencia de 2 entre 8 y 2²⁰ sectores (2¹⁵ en streamOptimized); `numGTEsPerGT` = 512;
  `capacity` ≤ `u64::MAX / 512`.
- Descriptor: `descriptorSize` > 0, ≤ 1 MiB y dentro del fichero; texto hasta el primer NUL con
  conversión UTF-8 tolerante; líneas ≤ 4096 bytes; sin caracteres de control (salvo tabulador);
  claves principales no repetidas; extensiones bien formadas; tamaño de la extensión = `capacity`;
  `createType` coherente con la cabecera.
- Pie (solo con GD_AT_END): fichero con sitio para cabecera + 1536 bytes; marcador del pie con
  `size` = 0 y tipo 3; marcador de fin de flujo (`numSectors`, `size` y `type` a 0) tras el pie; pie
  con firma `KDMV` que supera las mismas validaciones que la cabecera; capacidad, grano,
  `numGTEsPerGT`, ubicación y tamaño del descriptor, algoritmo e indicadores 2, 16 y 17 iguales a los
  de la cabecera; su `gdOffset` no puede ser otra vez GD_AT_END ni 0. Estructura crítica: `Corrupt`
  en ambos modos. El valor `numSectors` del marcador del pie no se comprueba (qemu tampoco).
- GD: tamaño ≤ `MAX_TABLE_BYTES` (256 MiB) **antes** de reservar memoria, dentro del fichero y sin
  solapar el descriptor; cada GT referenciada (2 KiB) dentro del fichero y sin solapar la cabecera,
  el descriptor, el GD, el pie ni otra GT.
- Granos: se validan **al abrir** (todas las GT presentes se cargan): la zona del grano debe estar
  dentro del fichero y no solapar la cabecera, el descriptor, el GD, el pie, una GT ni otro grano
  (entradas duplicadas incluidas); la entrada 1 sin el bit «zeroed-grain» es inválida. La zona es,
  en monolithicSparse, la parte del grano que cubre el disco virtual y, en streamOptimized, el
  marcador más los datos comprimidos (12 + `size` bytes).
- streamOptimized, además, al abrir se lee el **marcador de cada grano** (12 bytes, con una ventana
  de 4 KiB que aprovecha que los granos suelen ir seguidos): su `lba` debe ser exactamente
  `g × grainSize` (la posición del grano en la GT), `size` entre 1 y 2 × grano (el límite de qemu;
  deflate en el peor caso apenas supera el grano) y los datos dentro del fichero.
- En modo estricto cualquier entrada inválida da `Corrupt` en la apertura; en modo tolerante se
  descarta (ver «Modo tolerante»).
- streamOptimized, al **leer** un grano (fuera de la caché): se vuelven a leer y comprobar su
  marcador (el fichero pudo cambiar tras abrirlo), se descomprime con un límite de salida de un grano
  (+ 1 byte) y el flujo zlib debe terminar con su Adler-32 correcto y medir exactamente un grano o,
  en el último grano, la parte que cubre el disco. Si no, `read_at` devuelve `IoError::Read`
  (`InvalidData`, «grano comprimido N dañado: …») con el rango **virtual** pedido: nunca datos. El
  lector tolerante marca esos sectores `Unreadable` con el patrón de relleno (ADR-019).
- No se exige el marcador de fin de flujo con el GD en la cabecera: qemu-img no siempre lo escribe
  (una imagen de 1 GiB terminaba con el último grano). Un truncado deja fuera algún grano o tabla, y
  eso ya lo detectan las comprobaciones anteriores.
- Toda la aritmética de desplazamientos es comprobada (`checked_*`); errores de E/S →
  `IoError::Read` (un fichero acortado tras abrirse da `UnexpectedEof`, nunca datos inventados).

### Lo que no se puede detectar

VMDK no guarda ninguna suma de los datos aparte del **Adler-32** que el propio flujo zlib lleva de
los datos descomprimidos. Un grano comprimido alterado se detecta si deja de ser un flujo deflate
válido, si cambia su tamaño descomprimido o si no cuadra su Adler-32 (32 bits, débil pero útil:
detecta casi todos los cambios de pocos bytes). Una alteración que siga inflando al tamaño correcto
con un Adler-32 que coincida (colisión, o un flujo zlib válido entero sustituido por otro) **no se
puede detectar** y se leería como datos. Tampoco se puede detectar una entrada de GD o de GT que
apunte a otra estructura coherente (el problema es el mismo en monolithicSparse); el marcador de
grano con su LBA reduce mucho ese riesgo en streamOptimized.

La apertura estricta no descomprime los granos (habría que leer el fichero entero): una imagen
streamOptimized que el modo estricto abre puede tener granos que fallen al leerlos. Las lecturas
estrictas y los hashes fallan en ese caso, como ante cualquier error de lectura.

## Modo tolerante (ADR-020)

`VmdkImage::open_with(file, true)` (o `OpenOptions::tolerant`). Correspondencia inequívoca: una GT
(entrada de GD ≠ 0) cabe en el fichero y no solapa la cabecera, el descriptor, el GD, el pie ni otra
GT; un grano (entrada de GT ≠ 0) cabe en el fichero (su zona), no solapa esas estructuras, ninguna GT
ni otro grano, y la entrada 1 solo es válida con el indicador «zeroed-grain»; en streamOptimized, su
marcador debe además ser válido (LBA y tamaño). Una entrada de GD inválida descarta los granos de su
GT; una de GT, solo su grano; si un grano solapa una GT se descartan los dos (y con la GT, todos sus
granos); en cualquier solape se descartan **todos** los implicados. Un grano descartado se lee con
`IoError::Read` y `allocation` lo informa como `Data`. `describe()` añade «; modo tolerante: N granos
descartados (no fiables)». Cabecera, descriptor, ubicación del GD y pie siguen siendo críticos
(`Corrupt` en ambos modos).

Un grano streamOptimized cuyo marcador es válido pero cuyos datos no se descomprimen se comporta
igual en ambos modos (error de lectura, `Unreadable` en el lector tolerante) y no se cuenta en
`discarded_blocks()`, porque solo se descubre al leerlo.

Pruebas (`vmdk.rs`, 2): grano fuera del fichero, sobre el descriptor y el GD (que también cubren las
GT), duplicado, solapado, entrada 1 sin indicador, GT fuera del fichero, sobre el descriptor,
duplicada y grano sobre una GT, con comparación sector a sector e inmutabilidad; estructuras críticas
en ambos modos. streamOptimized (`vmdk_stream.rs`): LBA del marcador distinto, tamaño comprimido 0 o
enorme, grano fuera del fichero, grano duplicado, grano que invade otro grano y una GT, GT sobre un
grano y entrada que apunta a una GT. Con qemu-img: `monolithic_sparse_with_duplicated_grain_in_tolerant_mode`
y `stream_optimized_with_bad_marker_or_truncated_in_tolerant_mode`.

## GD redundante

Se usa solo el GD principal (`gdOffset`, o el del pie). El redundante (`rgdOffset`, bit 1) no se lee
ni se compara. Posible mejora futura: ofrecerlo como alternativa de recuperación cuando el principal
esté dañado.

## Referencias externas y seguridad

`VmdkImage::open` recibe un `File` ya admitido y **nunca** abre otros ficheros. Los nombres de
padres y extensiones (incluidas rutas maliciosas como `..\..\Windows\…`, absolutas, UNC o
`\\?\GLOBALROOT\…`) solo aparecen como texto en `ExternalReference.reference` o en el detalle del
error, con caracteres de control escapados y truncados a 260 caracteres.

## Memoria y rendimiento

- El GD se carga entero al abrir (≤ 256 MiB y ≤ tamaño del fichero), leído por trozos de 1 MiB; tras
  validarlo se reutiliza como índice de las GT, sin copia.
- Las GT presentes se cargan y validan al abrir (2 KiB cada una, en total ≤ `MAX_TABLE_BYTES`; si no,
  `UnsupportedFeature`): un disco de 2 TiB con granos de 64 KiB ocupa 128 MiB.
- streamOptimized guarda además el tamaño comprimido de cada entrada de GT (`u32`, tanto como las GT
  cargadas). Se declara al presupuesto como una tabla más leída del fichero (los marcadores).
- Memoria de trabajo de la apertura ≤ `open_memory_budget(t)` por tabla (17/8 × `t` + 64 KiB, ADR-020,
  addendum), con `t` = bytes del GD y de las GT presentes (y, en streamOptimized, de los tamaños
  comprimidos): como mucho unos 1088 MiB con GD y GT al límite (unos 1632 MiB en streamOptimized).
  La detección de solapes usa una lista de índices `u32` (entradas del GD; para los granos, su
  posición en las GT cargadas) y el modo tolerante un bit por entrada. Reservas con
  `try_reserve_exact`; un fallo o un exceso es `UnsupportedFeature`, nunca un aborto. Picos medidos
  con entradas duplicadas: GD al límite (todas las entradas en la misma GT) 512 / 520 MiB
  (estricto / tolerante; antes 2,5 GiB); GT al límite (131 072 GT, todos los granos en el mismo
  sector) 521 / 529 MiB (antes 2,5 GiB). En modo estricto la apertura falla en la primera entrada
  inválida o en el primer solape encontrado.
- streamOptimized al leer: búfer del grano comprimido ≤ 2 × grano + 12 bytes y salida de la
  descompresión ≤ grano + 1 byte (con el grano máximo de 16 MiB: unos 48 MiB por lectura en curso).
  Caché LRU de granos descomprimidos de **4 MiB** como máximo (al menos un grano; 64 granos de 64 KiB),
  protegida por un `Mutex`; la descompresión se hace fuera del cerrojo. Un grano que falla no se
  guarda.
- Medido (Release, Windows 11): streamOptimized de qemu-img de 1 GiB (un tercio aleatorio, un tercio
  ceros, un tercio repetitivo; 350 MiB en disco) se abre en 0,3 s (lee unos 11 000 marcadores) y se
  calcula su SHA-256 completo en 1,6 s (unos 665 MB/s), igual al del RAW original.
- `allocation()` salta de golpe los tramos de 512 granos cuya GT no existe, así que un disco
  enorme casi vacío se describe en pocas iteraciones.

## Dependencia: `miniz_oxide`

La descompresión usa el crate `miniz_oxide` **0.8.9** (fijado con `=0.8.9` en el `Cargo.toml` del
espacio de trabajo, sin funciones por defecto salvo `with-alloc`; depende solo de `adler2` 2.0.1).
Rust puro con `#![forbid(unsafe_code)]` (también `adler2`), licencias MIT / Zlib / Apache-2.0
(`adler2`: 0BSD / MIT / Apache-2.0). Se usa `inflate::decompress_to_vec_zlib_with_limit`, que
comprueba el Adler-32 y acota la salida. Justificación en ADR-017 (addendum de la incidencia 9).

## API

- `VmdkImage::open(File) -> mr_io::Result<VmdkImage>` (estricto) y `VmdkImage::open_with(File, tolerant)`;
  `is_tolerant()`, `discarded_blocks()` (granos descartados).
- `BlockDevice`: `size()` = capacity × 512, `sector_size()` = 512, `read_at` con límites
  comprobados (`OutOfBounds`), `allocation(lba, count)` con tramos ordenados y fusionados
  (`Data` para granos asignados, comprimidos o no; `Sparse` para entradas 0, GT ausentes o granos a
  cero), validando el rango como la implementación por defecto (`InvalidRange`).
- `describe()`: p. ej. «VMDK monolithicSparse (cabecera v1), granos de 64 KiB» o «VMDK streamOptimized
  (cabecera v3), granos de 64 KiB comprimidos con deflate[, directorio de granos en el pie]».
- `descriptor()`, `grain_size_bytes()`, `parse_descriptor(&str)`.

## Pruebas y resultados reales

Comandos (desde la raíz del repositorio):

```
cargo fmt --all -- --check
cargo clippy -p mr-image --all-targets -- -D warnings
cargo test -p mr-image                                           # unitarias y corrupción
cargo test --release -p mr-image --test vmdk_qemu -- --ignored   # 8 cruzadas con qemu-img
```

Resultado (qemu-img 11.1.2, Windows 11): fmt y clippy sin avisos; 18 unitarias en `vmdk.rs`, 12 en
`vmdk_stream.rs`, 8/8 de integración contra qemu-img y 6 de corrupción de VMDK superadas.

Unitarias de monolithicSparse (imágenes sintéticas construidas en memoria): parser del descriptor
(estilo qemu, CRLF, BOM, FLAT/ZERO, nombres con espacios y Unicode, bytes no UTF-8), descriptores
maliciosos (rutas `..\`, absolutas, UNC, `\\?\`, `/etc/passwd` como extensión y como padre), líneas y
descriptores enormes, caracteres de control, cabeceras con firma/versión/indicadores/grano/numGTEsPerGT
no válidos, caracteres de fin de línea alterados, cabeceras de streamOptimized incompletas o
incoherentes con el descriptor, descriptor fuera del fichero, GD/GT/granos fuera del fichero, fichero
truncado, capacidad enorme rechazada sin reservar memoria y disco de 64 TiB casi vacío abierto con un
GD diminuto, lectura cruzando granos asignados, no asignados y a cero, GT ausentes, capacidad no
múltiplo del grano y capacidad 0.

Unitarias de streamOptimized (`vmdk_stream.rs`, generador propio al estilo de VMware con granos
comprimidos con `miniz_oxide`): marcadores y pie (tipo, tamaño, fin de flujo), límites del marcador de
grano (LBA, tamaño 0, 2 × grano, fuera del fichero, desbordamientos), descompresión (tamaño del grano o
de la cola, salida mayor que el grano, Adler-32 alterado, flujo truncado, vacío, basura, deflate sin
cabecera zlib), caché LRU acotada, lectura completa y desalineada con el GD en el pie y en la
cabecera y con el último grano corto o entero, `allocation`, variantes no soportadas, pie y
descriptor críticos en ambos modos (9 casos), fin de flujo opcional con el GD en la cabecera,
marcadores inválidos (estricto `Corrupt`, tolerante con comparación sector a sector), datos
comprimidos dañados (byte alterado, Adler-32, truncado, medio grano, basura: ilegibles en ambos modos,
el resto idéntico), fichero acortado tras abrir y caché limitada a 512 granos de 8 KiB tras leer
1100.

Integración contra `qemu-img` (`MR_QEMU_IMG` o
`.tools\qemu\bin\qemu-img.exe` (relativa a la raíz del repositorio); solo `create`, `convert`,
`info`, `map`):

| Caso | Comprobaciones |
|---|---|
| RAW de 16 MiB (zonas pseudoaleatorias, ceros y bytes sueltos) → `convert -O vmdk -o subformat=monolithicSparse` | tamaño = `info` y = referencia; SHA-256 de la lectura completa = `convert -O raw`; 300 rangos aleatorios deterministas; `allocation()` = `map --output=json` (tramos de datos/no datos fusionados); tramos dispersos leen ceros; fichero intacto (SHA-256, tamaño, fecha) |
| RAW de 5 MiB + 37 sectores (no múltiplo del grano) | ídem |
| RAW de 3 000 000 bytes (no múltiplo del sector; qemu lo redondea) | ídem |
| RAW de 16 MiB con `zeroed_grain=on` (cabecera v2) | ídem |
| `subformat=streamOptimized`: 16 MiB, 5 MiB + 37 sectores, 3 000 000 bytes y 4 MiB + 512 bytes aleatorios (incompresibles) con un hueco | ídem (mapa: qemu informa los granos comprimidos como `data: true`) |
| streamOptimized de qemu reescrito con la disposición de VMware (GD al final, pie, marcadores de GT y GD, fin de flujo): 16 MiB y 5 MiB + 37 sectores | qemu-img lo lee igual que el original; ídem contra su conversión a RAW y su mapa; `describe` cita el pie |
| streamOptimized de 8 MiB con 1 a 3 bytes alterados en los datos comprimidos de 1 a 3 granos (120 iteraciones, ambos modos) | se abre sin descartes; cada grano se lee idéntico al original o da `IoError::Read`, y solo los alterados (resultado: todos los alterados ilegibles); lector tolerante sector a sector con un grano dañado; fichero intacto |
| streamOptimized con el LBA de un marcador alterado, truncado a mitad del último grano y cortado justo tras el último grano | estricto `Corrupt` y tolerante con 1 grano descartado (sector a sector contra la referencia); el último caso abre en ambos modos y se lee entero |
| `subformat=monolithicFlat` | el descriptor da `ExternalReference` citando `…-flat.vmdk`; el fichero plano da `Corrupt` (sin `KDMV`); no se sigue la referencia |
| Descriptor separado (`vmdk_extents_qemu.rs`, incidencia 11): RAW disperso (`fsutil sparse setflag`) de 16 MiB + 37 sectores → `monolithicFlat`, `twoGbMaxExtentFlat` y `twoGbMaxExtentSparse` (1 extensión); 2 GiB + 3 MiB + 7 sectores → `twoGbMaxExtentFlat` (2 extensiones, unos 2 GiB en disco mientras dura la prueba); 4 GiB + 3 MiB + 7 sectores → `twoGbMaxExtentSparse` (3 extensiones) | `qemu-img compare` RAW ↔ VMDK; lista sin abrir nada con tamaño y nombres; sin aprobar (o sin la última) `ExternalReference` y la detección automática tampoco la sigue; con cada fichero aprobado: SHA-256 completo = RAW, 200 rangos aleatorios más los bordes de cada extensión, `allocation` = `qemu-img map` (SPARSE) o todo datos (FLAT); descriptor, extensiones y RAW intactos (SHA-256, tamaño, fecha) |
| Descriptor escrito a mano con una extensión SPARSE: un monolithicSparse y un streamOptimized de qemu-img | SHA-256 = RAW; tamaño declarado distinto de la capacidad → `Corrupt`; descriptor con `parentFileNameHint` → `ExternalReference`; ficheros intactos |
| `create -f vmdk -b base.vmdk -F vmdk hijo.vmdk` | `ExternalReference` con `base.vmdk` |

Extremo a extremo (`core/mr-api/tests/containers_qemu.rs`): el corpus GPT/MBR (512 y 4096 bytes por
sector) convertido a streamOptimized se abre con la detección automática y da el mismo SHA-256 que el
RAW.

Corrupción (`corruption.rs`, imagen streamOptimized sintética con el GD en la cabecera y 9 granos
comprimidos en orden no monótono): 400 iteraciones de 1 a 4 bytes alterados en estructuras
(cabecera, descriptor, GD, entradas de GT, marcadores; a veces en cualquier sitio), con las
garantías comunes salvo «lo que el estricto abre se lee entero» (los granos se descomprimen al
leerlos); 300 iteraciones con alteraciones solo en los granos comprimidos (marcador o datos),
comprobando que ningún grano legible difiere del original (977 lecturas de grano ilegibles y 85
aperturas estrictas rechazadas); truncado en cada frontera hasta el último grano (80 cortes; el
estricto no abre ninguno y el tolerante abre 62, siempre con descartes y el resto idéntico); fichero
acortado tras abrir (cada grano cuyo marcador o datos quedan fuera da error).

## Limitaciones

- Discos multiextensión y FLAT solo con descriptor separado y ficheros aprobados uno a uno (incidencia
  11); VMFS*, SESparse y cadenas de instantáneas (padres) no se interpretan.
- streamOptimized: solo deflate con marcadores; no se comprueban los marcadores de GT y GD (qemu-img
  no los escribe cuando deja las tablas al principio) ni el `numSectors` del marcador del pie.
- No se usa el GD redundante para recuperar un GD principal dañado, y sus GT no se consideran al
  buscar solapes (no se leen).
- `qemu-img convert` no genera entradas GTE «zeroed-grain» (las zonas de ceros quedan sin
  asignar) ni streamOptimized con el GD en el pie, por lo que esos casos se prueban con imágenes
  sintéticas o reescritas a partir de las de qemu-img (que qemu-img sí lee).
- `uncleanShutdown` solo se informa; no se intenta reparar nada (solo lectura).
