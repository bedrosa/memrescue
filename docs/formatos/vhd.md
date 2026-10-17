# Formato VHD (Microsoft Virtual Hard Disk)

Módulo: `core/mr-image/src/vhd.rs` (`mr_image::vhd::VhdImage`). Solo lectura.

## Especificación usada

Microsoft, *Virtual Hard Disk Image Format Specification*, versión 1.0 (documento público).
La implementación es original y se basa solo en ese documento. Todos los enteros son big-endian.

- **Pie** (512 bytes, cookie `conectix`): características, versión de formato `0x00010000`,
  *data offset*, marca de tiempo, aplicación/versión/SO creador, tamaño original, **tamaño actual**,
  geometría CHS, **tipo de disco**, **suma de control**, id único y *saved state*.
- **Suma de control** (pie y cabecera dinámica): complemento a uno de la suma de todos los bytes de la
  estructura excluyendo los 4 bytes del propio campo.
- **Cabecera dinámica** (1024 bytes, cookie `cxsparse`, en *data offset*): *table offset* (BAT),
  versión `0x00010000`, *max table entries*, *block size*, suma de control, id y nombre (UTF-16BE)
  del padre y localizadores del padre.
- **BAT**: `max table entries` enteros u32; cada uno es el sector (×512) donde empieza el bloque o
  `0xFFFFFFFF` si no está asignado.
- **Bloque asignado**: mapa de bits de sectores (1 bit por sector, redondeado a múltiplo de 512 bytes)
  seguido de `block size` bytes de datos.

## Variantes soportadas

| Tipo | Valor | Comportamiento |
|---|---|---|
| Fijo | 2 | Datos en `fichero[0..current_size]`, pie a continuación. `describe()` → «VHD fijo». |
| Dinámico | 3 | BAT + bloques. `describe()` → «VHD dinámico, bloques de 2 MiB». |
| Diferencial | 4 | `IoError::ExternalReference` con el nombre del padre (o su id). **Nunca se abre el padre.** |
| Ninguno / reservados | 0, 1, 5, 6 | `IoError::UnsupportedFeature`. |
| Otros | - | `IoError::Corrupt`. |

`size()` = campo *Current Size* del pie; `sector_size()` = 512.

## Validaciones

- Fichero ≥ 511 bytes; cookie, suma de control y versión mayor 1 del pie.
- Tamaño virtual múltiplo de 512.
- Fijo: el fichero mide exactamente `current_size + 512` (o `+ 511` con pie antiguo).
- Dinámico:
  - Cabecera dinámica completa dentro del fichero, después de la copia del pie (≥ 512), con cookie,
    suma de control y versión mayor 1 válidas.
  - `block size` potencia de 2 entre 512 B y 256 MiB.
  - `max table entries × block size ≥ current size`.
  - BAT completa (`max table entries × 4` bytes) dentro del fichero, sin solapar la copia del pie ni
    la cabecera dinámica, y ≤ `mr_image::MAX_TABLE_BYTES` (256 MiB). Las comprobaciones de tamaño se
    hacen **antes** de reservar memoria; solo se leen las entradas que cubren el tamaño virtual.
  - Cada bloque asignado (mapa de bits + parte útil de los datos) dentro de la zona de datos (antes
    del pie final), sin solapar la copia del pie, la cabecera dinámica, la BAT **ni otro bloque**
    (entradas duplicadas incluidas). En modo estricto cualquier entrada que no lo cumpla da `Corrupt`;
    en modo tolerante se descarta (ver «Modo tolerante»).
- Toda la aritmética de desplazamientos usa `checked_*`/`saturating_*`; ningún dato corrupto provoca
  pánicos (hay una prueba de corrupción aleatoria).
- `read_at` comprueba límites con aritmética comprobada (`OutOfBounds`); `allocation` valida el rango
  como la implementación por defecto (`InvalidRange`).
- Errores de E/S: `IoError::Read` con el desplazamiento y la longitud **virtuales** de la parte
  afectada (igual que `ImageFile`, para que el lector tolerante pueda localizar el sector); los fallos
  al leer estructuras durante `open` llevan el desplazamiento dentro del fichero.

## Decisiones

- **Mapa de bits**: se sigue la especificación. Bit a 1 → sector con datos (se lee del fichero,
  `Allocation::Data`); bit a 0 → sector no escrito (ceros, `Allocation::Sparse`). Orden de bits: el
  bit más significativo de cada byte corresponde al primer sector. qemu-img escribe mapas de bits
  completos al asignar un bloque, por lo que sus imágenes coinciden bloque a bloque con `qemu-img map`.
  Se guarda en caché el último mapa leído (una entrada, protegida por `Mutex`).
- **Pie de 511 bytes**: si los últimos 512 bytes no forman un pie válido y los últimos 511 empiezan por
  `conectix`, se acepta ese pie (el byte reservado que falta no influye en la suma). `describe()` lo
  indica.
- **Copia del pie**: si el pie final no es válido, se usa la copia del byte 0 **solo** si es válida
  (cookie, suma, versión) y declara tipo dinámico o diferencial (los fijos no tienen copia). En ese
  caso los bloques se validan contra el tamaño completo del fichero, ya que no se sabe si el pie final
  llegó a escribirse. `describe()` añade «pie final dañado: se usa la copia del byte 0».
- **Último bloque**: solo se exige que quepa en el fichero la parte que cubre el tamaño virtual.
- **Diferenciales**: el nombre del padre se lee de la cabecera dinámica (UTF-16BE, hasta el primer
  nulo). Si está vacío se usa el id del padre; si la cabecera está dañada se devuelve igualmente
  `ExternalReference` con el motivo. No se interpretan los localizadores ni se accede a otros ficheros.
- Campos que no se usan: geometría CHS, marca de tiempo, creador, *saved state*, *features*.

## Modo tolerante (ADR-020)

`VhdImage::open_with(file, true)` (o `OpenOptions::tolerant`). Correspondencia inequívoca de una entrada
de la BAT: el bloque (mapa de bits + parte útil) cabe entero en la zona de datos y no solapa la copia
del pie, la cabecera dinámica, la BAT completa (`max table entries × 4`) ni el bloque de otra entrada.
El VHD no tiene campo de estado: el valor `0xFFFFFFFF` es «sin asignar» y cualquier otro es un sector;
el sector 0 (copia del pie) nunca es válido. Si dos bloques se solapan (o dos entradas son iguales) se
descartan **los dos**. Un bloque descartado se lee con `IoError::Read` (rango virtual) y `allocation` lo
informa como `Data` aunque su mapa de bits tuviera sectores a 0 (el mapa forma parte del bloque
descartado). `describe()` añade «; modo tolerante: N bloques descartados (no fiables)». Un VHD fijo no
tiene tablas: nada que descartar. Pie, cabecera dinámica, tamaño de bloque y ubicación de la BAT siguen
siendo críticos (`Corrupt` en ambos modos).

## Límites

- Bloques entre 512 B y 256 MiB (mapa de bits ≤ 64 KiB por bloque).
- BAT ≤ 256 MiB (`MAX_TABLE_BYTES`) y siempre ≤ tamaño del fichero.
- Memoria de trabajo de la apertura ≤ `open_memory_budget(t)` = 17/8 × `t` + 64 KiB, con `t` = bytes
  de BAT leídos (ADR-020, addendum): la BAT se lee por trozos de 1 MiB y se decodifica sobre la marcha
  (4 bytes por entrada, lo que conserva la imagen), la detección de solapes usa una lista de índices
  `u32` de los bloques asignados y el modo tolerante un bit por entrada. Las reservas usan
  `try_reserve_exact`: un fallo o un exceso de presupuesto es `UnsupportedFeature`, nunca un aborto.
  Con la BAT al límite y todas las entradas duplicadas el pico medido es de 512 MiB (estricto) y
  520 MiB (tolerante); antes, 2,8 GiB. En modo estricto la apertura falla en la primera entrada
  inválida o en el primer solape encontrado.
- Las peticiones de lectura no se trocean más allá de los bloques: el tamaño de cada `read_at` lo
  limita quien llama.

## Pruebas

Unitarias (`cargo test -p mr-image`, 16 pruebas en `vhd.rs`, imágenes construidas en ficheros
temporales según la especificación): suma de control correcta e incorrecta, cookie mala, tipos de
disco reservados/desconocidos, pie de 511 bytes, tamaño actual incoherente (fijo más corto/largo, no
múltiplo de 512, BAT insuficiente), lecturas cruzando bloques asignados, no asignados y con mapa de
bits parcial (comparadas byte a byte), `allocation` exacta, pie final dañado → copia, ambos dañados,
cabecera dinámica dañada o fuera del fichero, BAT fuera del fichero / con desbordamiento / solapada,
tamaños de bloque absurdos, `max_table_entries` enorme (sin reservar memoria), bloque fuera del
fichero, ficheros truncados (antes y después de abrir), diferencial → `ExternalReference`, y 300
corrupciones aleatorias deterministas sin pánicos.

Modo tolerante (`vhd.rs`, 3 pruebas): entradas fuera del fichero, sobre el pie final, sobre la copia
del pie, sobre la cabecera dinámica, sobre la BAT, duplicadas, solapadas con otro bloque y varias a la vez:
`Corrupt` en estricto; en tolerante, comparación sector a sector contenido ↔ estado con
`mr_io::resilient` (bloques sanos idénticos a la imagen sin dañar, dañados `Unreadable` con `BAD!`),
lectura estricta y lectura que aborta fallan, `allocation` = `Data`, número de bloques en `describe()`
e inmutabilidad (hash, tamaño y fecha); sin daños = estricto; estructuras críticas dañadas → `Corrupt`
en ambos modos. Con qemu-img: `qemu_dynamic_with_damaged_bat_entry_in_tolerant_mode` daña la entrada
de un bloque con datos y compara el resto con la conversión de referencia.

Integración contra **qemu-img 11.1.2** como referencia independiente
(`core/mr-image/tests/vhd_qemu.rs`, marcadas `#[ignore]`; se ejecutan con
`cargo test -p mr-image --test vhd_qemu -- --ignored`). Ruta de qemu-img: `MR_QEMU_IMG` o
`.tools\qemu\bin\qemu-img.exe` (relativa a la raíz del repositorio). Solo se usan `convert`, `info` y
`map`. Casos:

- RAW de 16 MiB (bloques de 2 MiB con datos pseudoaleatorios, ceros, medio bloque y un único sector).
- RAW de 10 MiB + 3 sectores (tamaño no múltiplo del bloque).

Cada uno se convierte a VHD fijo y dinámico (`-O vpc -o subformat=…,force_size=on`) y de vuelta a RAW
con qemu-img (referencia). Se comprueba: tamaño virtual = RAW de referencia = `qemu-img info`;
SHA-256 de la lectura completa en trozos irregulares = SHA-256 de la referencia y del RAW original;
300 rangos aleatorios deterministas idénticos byte a byte; `allocation()` sector a sector frente a
`qemu-img map --output=json` (`Sparse` ⇔ `"data": false`) y que lo disperso sea cero; y que el VHD
no cambia (hash, tamaño y fecha) tras leerlo.

Resultados reales (2026-10-17):

| Caso | Variante | Tamaño | Sectores dispersos | SHA-256 |
|---|---|---|---|---|
| 16 MiB | fijo | 16 777 216 | 0 | `f9d8081d…db1759` |
| 16 MiB | dinámico | 16 777 216 | 12 288 | `f9d8081d…db1759` |
| 10 MiB + 3 sectores | fijo | 10 487 296 | 0 | `fb7e1d96…eb4554d` |
| 10 MiB + 3 sectores | dinámico | 10 487 296 | 8 192 | `fb7e1d96…eb4554d` |

2/2 pruebas con qemu-img superadas; 16/16 unitarias; `cargo clippy -p mr-image --all-targets -- -D
warnings` y `cargo fmt --all -- --check` sin avisos.

## Limitaciones conocidas

- No se leen discos diferenciales (ni cadenas de padres); solo se informa del padre.
- En modo estricto (por defecto) una sola entrada de BAT inválida impide abrir la imagen; el modo
  tolerante (explícito) descarta solo los bloques afectados.
- No se usan los localizadores de padre ni el campo *saved state*.
- No se ha probado con VHD reales de Virtual PC / Hyper-V / Disk2vhd, solo con imágenes sintéticas
  y generadas por qemu-img (que escribe mapas de bits completos; el caso de mapa parcial solo se
  cubre con imágenes sintéticas).
- No se detecta el uso de la geometría CHS en lugar de *Current Size* que hacen algunas herramientas
  antiguas: el tamaño virtual es siempre *Current Size*.
