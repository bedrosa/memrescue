# VMDK (monolithicSparse): lector de solo lectura

Implementación: `core/mr-image/src/vmdk.rs` (`mr_image::vmdk::VmdkImage`).
Pruebas: unitarias en el mismo fichero y cruzadas contra `qemu-img` en
`core/mr-image/tests/vmdk_qemu.rs`.

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
| 56 | 8 | `gdOffset` | sector del GD principal |
| 64 | 8 | `overHead` | no se usa |
| 72 | 1 | `uncleanShutdown` | solo informativo (`describe()`) |
| 73 | 4 | `'\n' ' ' '\r' '\n'` | prueba de fin de línea |
| 77 | 2 | `compressAlgorithm` | debe ser 0 |

### Directorio y tablas de granos

- GD en `gdOffset` (sectores): `ceil(ceil(capacity / grainSize) / 512)` entradas `u32`, cada
  una el sector de una tabla de granos (GT). Entrada 0 → GT ausente: sus 512 granos son dispersos.
- Cada GT: 512 entradas `u32` = sector del grano. 0 → no asignado (ceros). 1 con el bit 2 de
  `flags` → grano a cero (ceros).
- El dato del byte virtual `p` está en `GT[g % 512]·512 + p % (grainSize·512)`, con
  `g = p / (grainSize·512)`.

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
| `streamOptimized` (bits 16/17, `gdOffset` = `0xFFFF…FFFF`, `compressAlgorithm` ≠ 0, o `createType="streamOptimized"`) | `UnsupportedFeature` |
| Descriptor de texto separado (`monolithicFlat`, `twoGbMaxExtent*`, `vmfs*`…) | `ExternalReference` con los nombres (como texto) de los ficheros de extensión, que no se abren; si el descriptor no se puede interpretar, `UnsupportedFeature`. (Cambio del integrador: antes era siempre `UnsupportedFeature`; la interfaz necesita saber qué fichero falta.) |
| Extensión dispersa sin descriptor incrustado (`descriptorOffset` = 0, p. ej. `-s001.vmdk`) | `UnsupportedFeature` |
| Firma `COWD` (ESX 2.x) | `UnsupportedFeature` |
| Versión fuera de 1–3, indicadores desconocidos | `UnsupportedFeature` |
| Otro `createType` con cabecera `KDMV` | `UnsupportedFeature` |
| Disco hijo (`parentCID` ≠ `ffffffff` o `parentFileNameHint`) | `ExternalReference` |
| Varias extensiones, extensión no SPARSE con fichero, o nombre de extensión con ruta (`/`, `\`, `:`, `..`, UNC, espacios en los extremos) | `ExternalReference` |
| Cualquier otra incoherencia | `Corrupt { format: "VMDK" }` |

El nombre de la extensión única SPARSE no se compara con el nombre real del fichero (el lector
no lo conoce y renombrar un `.vmdk` es habitual): por definición, en monolithicSparse esa
extensión es el propio fichero. Si el nombre parece una ruta se trata como referencia externa.

## Validaciones

- Fichero ≥ 512 bytes, firma `KDMV`; si no, se distingue descriptor de texto, `COWD` o `Corrupt`.
- Caracteres de prueba de fin de línea exactos cuando el bit 0 está activo (detecta transferencias
  en modo texto) → `Corrupt`.
- `grainSize` potencia de 2 entre 8 y 2²⁰ sectores; `numGTEsPerGT` = 512;
  `capacity` ≤ `u64::MAX / 512`.
- Descriptor: `descriptorSize` > 0, ≤ 1 MiB y dentro del fichero; texto hasta el primer NUL con
  conversión UTF-8 tolerante; líneas ≤ 4096 bytes; sin caracteres de control (salvo tabulador);
  claves principales no repetidas; extensiones bien formadas; tamaño de la extensión = `capacity`.
- GD: tamaño ≤ `MAX_TABLE_BYTES` (256 MiB) **antes** de reservar memoria, dentro del fichero y sin
  solapar el descriptor; cada GT referenciada (2 KiB) dentro del fichero y sin solapar la cabecera,
  el descriptor, el GD ni otra GT.
- Granos: se validan **al abrir** (todas las GT presentes se cargan): la parte del grano que cubre el
  disco virtual debe estar dentro del fichero y no solapar la cabecera, el descriptor, el GD, una GT
  ni otro grano (entradas duplicadas incluidas); la entrada 1 sin el bit «zeroed-grain» es inválida.
  En modo estricto cualquier entrada inválida da `Corrupt` en la apertura; en modo tolerante se
  descarta (ver «Modo tolerante»).
- Toda la aritmética de desplazamientos es comprobada (`checked_*`); errores de E/S →
  `IoError::Read` (un fichero acortado tras abrirse da `UnexpectedEof`, nunca datos inventados).

## Modo tolerante (ADR-020)

`VmdkImage::open_with(file, true)` (o `OpenOptions::tolerant`). Correspondencia inequívoca: una GT
(entrada de GD ≠ 0) cabe en el fichero y no solapa la cabecera, el descriptor, el GD ni otra GT; un grano
(entrada de GT ≠ 0) cabe en el fichero (parte útil), no solapa esas estructuras, ninguna GT ni otro
grano, y la entrada 1 solo es válida con el indicador «zeroed-grain». Una entrada de GD inválida
descarta los granos de su GT; una de GT, solo su grano; si un grano solapa una GT se descartan los dos
(y con la GT, todos sus granos); en cualquier solape se descartan **todos** los implicados. Un grano
descartado se lee con `IoError::Read` y `allocation` lo informa como `Data`. `describe()` añade «; modo
tolerante: N granos descartados (no fiables)». Cabecera, descriptor y ubicación del GD siguen siendo
críticos (`Corrupt` en ambos modos). Pruebas (`vmdk.rs`, 2): grano fuera del fichero, sobre el
descriptor y el GD (que también cubren las GT), duplicado, solapado, entrada 1 sin indicador, GT fuera
del fichero, sobre el descriptor, duplicada y grano sobre una GT, con comparación sector a sector e
inmutabilidad; estructuras críticas en ambos modos. Con qemu-img:
`monolithic_sparse_with_duplicated_grain_in_tolerant_mode`.

## GD redundante

Se usa solo el GD principal (`gdOffset`). El redundante (`rgdOffset`, bit 1) no se lee ni se
compara. Posible mejora futura: ofrecerlo como alternativa de recuperación cuando el principal
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
- Memoria de trabajo de la apertura ≤ `open_memory_budget(t)` por tabla (17/8 × `t` + 64 KiB, ADR-020,
  addendum), con `t` = bytes del GD y de las GT presentes: como mucho unos 1088 MiB con ambos al
  límite. La detección de solapes usa una lista de índices `u32` (entradas del GD; para los granos,
  su posición en las GT cargadas) y el modo tolerante un bit por entrada. Reservas con
  `try_reserve_exact`; un fallo o un exceso es `UnsupportedFeature`, nunca un aborto. Picos medidos
  con entradas duplicadas: GD al límite (todas las entradas en la misma GT) 512 / 520 MiB
  (estricto / tolerante; antes 2,5 GiB); GT al límite (131 072 GT, todos los granos en el mismo
  sector) 521 / 529 MiB (antes 2,5 GiB). En modo estricto la apertura falla en la primera entrada
  inválida o en el primer solape encontrado.
- `allocation()` salta de golpe los tramos de 512 granos cuya GT no existe, así que un disco
  enorme casi vacío se describe en pocas iteraciones.

## API

- `VmdkImage::open(File) -> mr_io::Result<VmdkImage>` (estricto) y `VmdkImage::open_with(File, tolerant)`;
  `is_tolerant()`, `discarded_blocks()` (granos descartados).
- `BlockDevice`: `size()` = capacity × 512, `sector_size()` = 512, `read_at` con límites
  comprobados (`OutOfBounds`), `allocation(lba, count)` con tramos ordenados y fusionados
  (`Data` para granos asignados, `Sparse` para entradas 0, GT ausentes o granos a cero), validando
  el rango como la implementación por defecto (`InvalidRange`).
- `describe()`: p. ej. «VMDK monolithicSparse (cabecera v1), granos de 64 KiB».
- `descriptor()`, `grain_size_bytes()`, `parse_descriptor(&str)`.

## Pruebas y resultados reales

Comandos (desde la raíz del repositorio):

```
cargo fmt --all -- --check
cargo clippy -p mr-image --all-targets -- -D warnings
cargo test -p mr-image                      # 16 unitarias
cargo test -p mr-image --test vmdk_qemu -- --ignored   # 4 cruzadas con qemu-img
```

Resultado (qemu-img 11.1.2, Windows 11): fmt y clippy sin avisos; 16/16 unitarias y 4/4 de
integración superadas.

Unitarias (imágenes sintéticas construidas en memoria): parser del descriptor (estilo qemu, CRLF,
BOM, FLAT/ZERO, nombres con espacios y Unicode, bytes no UTF-8), descriptores maliciosos (rutas
`..\`, absolutas, UNC, `\\?\`, `/etc/passwd` como extensión y como padre), líneas y descriptores
enormes, caracteres de control, cabeceras con firma/versión/indicadores/grano/numGTEsPerGT no
válidos, caracteres de fin de línea alterados, streamOptimized, descriptor fuera del fichero,
GD/GT/granos fuera del fichero, fichero truncado, capacidad enorme rechazada sin reservar memoria y
disco de 64 TiB casi vacío abierto con un GD diminuto, lectura cruzando granos asignados, no
asignados y a cero, GT ausentes, capacidad no múltiplo del grano y capacidad 0.

Integración contra `qemu-img` (`MR_QEMU_IMG` o
`.tools\qemu\bin\qemu-img.exe` (relativa a la raíz del repositorio); solo `create`, `convert`,
`info`, `map`):

| Caso | Comprobaciones |
|---|---|
| RAW de 16 MiB (zonas pseudoaleatorias, ceros y bytes sueltos) → `convert -O vmdk -o subformat=monolithicSparse` | tamaño = `info` y = referencia; SHA-256 de la lectura completa = `convert -O raw`; 300 rangos aleatorios deterministas; `allocation()` = `map --output=json` (tramos de datos/no datos fusionados); tramos dispersos leen ceros; fichero intacto (SHA-256, tamaño, fecha) |
| RAW de 5 MiB + 37 sectores (no múltiplo del grano) | ídem |
| RAW de 3 000 000 bytes (no múltiplo del sector; qemu lo redondea) | ídem |
| RAW de 16 MiB con `zeroed_grain=on` (cabecera v2) | ídem |
| `subformat=streamOptimized` | `UnsupportedFeature` que menciona streamOptimized; fichero intacto |
| `subformat=monolithicFlat` | el descriptor da `UnsupportedFeature` citando `…-flat.vmdk`; el fichero plano da `Corrupt` (sin `KDMV`); no se sigue la referencia |
| `create -f vmdk -b base.vmdk -F vmdk hijo.vmdk` | `ExternalReference` con `base.vmdk` |

## Limitaciones

- Solo monolithicSparse. No se interpretan streamOptimized, discos multi-extensión, FLAT/VMFS,
  SESparse ni cadenas de instantáneas (padres).
- No se usa el GD redundante para recuperar un GD principal dañado, y sus GT no se consideran al
  buscar solapes (no se leen).
- `qemu-img convert` no genera entradas GTE «zeroed-grain» (las zonas de ceros quedan sin
  asignar), por lo que esas entradas solo se prueban con imágenes sintéticas.
- `uncleanShutdown` solo se informa; no se intenta reparar nada (solo lectura).
