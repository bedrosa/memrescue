# VHDX (Virtual Hard Disk v2): lector de solo lectura

Módulos: `core/mr-image/src/vhdx.rs` (`VhdxImage`), `core/mr-image/src/vhdx_log.rs` (registro
pendiente reproducido en memoria) y `core/mr-image/src/crc32c.rs` (`crc32c`, `Crc32c`).
Pruebas cruzadas: `core/mr-image/tests/vhdx_qemu.rs` y `core/mr-image/tests/vhdx_log_qemu.rs`.

## Especificación usada

- **[MS-VHDX] «Virtual Hard Disk v2 (VHDX) File Format»**, Microsoft Open Specifications.
- **CRC-32C (Castagnoli)**: polinomio reflejado `0x82F63B78`, valor inicial `0xFFFFFFFF`,
  complemento final (RFC 3720, apéndice B.4). Implementación propia por tabla generada en
  tiempo de compilación, con variante incremental (`Crc32c::new/update/finish`).

Implementación original a partir de la especificación pública; no se ha usado material de terceros.

## Variantes soportadas

| Variante | Estado |
|---|---|
| VHDX fijo (LeaveBlocksAllocated) | Soportado |
| VHDX dinámico | Soportado |
| Bloques de 1 MiB a 256 MiB (potencia de 2) | Soportado |
| Sector lógico 512 B y 4096 B | Soportado (4096 B solo probado con imágenes sintéticas: qemu-img no ofrece esa opción) |
| Diferencial (HasParent) | `IoError::ExternalReference` con la ruta del localizador; el padre **nunca** se abre |
| Registro pendiente (LogGuid ≠ 0) | **Reproducido en memoria** (el fichero nunca se escribe); registro no válido → `Corrupt` (ver «Registro pendiente») |
| Registro pendiente de más de 64 MiB | `UnsupportedFeature` (Hyper-V y qemu usan 1 MiB) |
| Versión de cabecera ≠ 1 o versión de registro ≠ 0 | `UnsupportedFeature` |

`describe()` devuelve p. ej. «VHDX dinámico, bloques de 32 MiB, sector lógico 512 B, sector físico 4096 B»;
con un registro pendiente añade «; registro reproducido en memoria (N entradas)»
(`VhdxImage::replayed_log_entries` da N).
«Fijo» se deduce de la bandera LeaveBlocksAllocated de File Parameters (el formato no tiene un campo de tipo).

## Validaciones

1. Fichero ≥ 1 MiB e identificador `vhdxfile` en el desplazamiento 0.
2. **Cabeceras** en 64 KiB y 128 KiB: firma `head` y CRC-32C de 4 KiB con el campo de suma a cero.
   Se usa la válida con **mayor número de secuencia** (con una sola válida, esa). Ninguna válida → `Corrupt`.
3. Registro: si LogLength ≠ 0, desplazamiento y longitud alineados a 1 MiB, fuera de la zona de
   cabeceras y dentro del fichero. Si LogGuid ≠ 0, el registro se valida y se reproduce en memoria
   **antes** de los pasos siguientes: desde aquí todas las lecturas del fichero (tablas de regiones,
   metadatos, BAT y bloques) pasan por la superposición del registro y los límites se comprueban
   contra el tamaño efectivo del fichero.
4. **Tablas de regiones** en 192 KiB y 256 KiB: firma `regi`, CRC-32C de 64 KiB, ≤ 2047 entradas.
   Se usa la primera copia válida. Región requerida desconocida → `UnsupportedFeature`; regiones
   (todas, también las opcionales) alineadas a 1 MiB, longitud múltiplo de 1 MiB no nula, a partir de
   1 MiB y dentro del fichero; BAT y metadatos obligatorias y no duplicadas; **sin solapes** entre
   zona de cabeceras, registro y regiones.
5. **Metadatos**: firma `metadata`, ≤ 2047 entradas, cada elemento dentro de la región (desde 64 KiB)
   y ≤ 1 MiB; duplicados → `Corrupt`; metadato requerido desconocido → `UnsupportedFeature`.
   - File Parameters: bloque potencia de 2 entre 1 MiB y 256 MiB; HasParent → `ExternalReference`
     (preferencia de clave: `relative_path`, `absolute_win32_path`, `volume_path`, `parent_linkage`).
   - Logical/Physical Sector Size: 512 o 4096.
   - Virtual Disk Size: no nulo, múltiplo del sector lógico, ≤ 64 TiB.
6. **BAT**: `chunk ratio = (2^23 × sector lógico) / bloque`; entradas totales
   `n + (n − 1) / chunk_ratio` (`n` = bloques de datos); el bloque `b` está en el índice
   `b + b / chunk_ratio` (cada `chunk_ratio` entradas de bloque va una de mapa de bits de sectores).
   - Tamaño de la BAT ≤ `MAX_TABLE_BYTES` (256 MiB; si no, `UnsupportedFeature`) y ≤ longitud de la
     región (que ya está dentro del fichero): la memoria queda acotada por ambos **antes** de reservar.
     Se lee por trozos de 1 MiB y solo se guarda un `u64` por bloque de datos.
   - Memoria de trabajo de la apertura ≤ `open_memory_budget(t)` = 17/8 × `t` + 64 KiB, con `t` =
     bytes de BAT (ADR-020, addendum): bloques decodificados (8 bytes por bloque), lista de índices
     `u32` de los bloques presentes para la detección de solapes y, en modo tolerante, un bit por
     bloque. Reservas con `try_reserve_exact`; un fallo o un exceso es `UnsupportedFeature`. Con la
     BAT al límite (33 546 243 bloques de 1 MiB) y todas las entradas en el mismo bloque, el pico
     medido es de 385 MiB (estricto) y 389 MiB (tolerante); antes, 1,4 GiB. En modo estricto la
     apertura falla en la primera entrada inválida o en el primer solape encontrado.
   - Estados de bloque: NOT_PRESENT, UNDEFINED, ZERO, UNMAPPED → ceros (`Allocation::Sparse`);
     FULLY_PRESENT → datos; PARTIALLY_PRESENT (solo válido con padre) y 4/5 → `Corrupt` en modo
     estricto, bloque descartado en modo tolerante.
   - Mapa de bits: solo NOT_PRESENT o PRESENT (en modo tolerante, los demás se cuentan y se ignoran).
   - Bloque presente: desplazamiento ≥ 1 MiB, la parte del bloque que cubre el disco virtual dentro
     del fichero y sin solaparse con cabeceras, registro, regiones (metadatos, BAT, opcionales) **ni
     con otro bloque** (entradas duplicadas incluidas).
7. `read_at` y `allocation` con aritmética comprobada; `allocation` valida el rango como la
   implementación por defecto y devuelve tramos ordenados y fusionados que lo cubren exactamente.

Todo error de E/S del fichero se devuelve como `IoError::Read`; no hay pánicos ni `unsafe`.

## Registro pendiente (reproducción en memoria, issue #10)

Un VHDX que se cerró sin aplicar su registro (LogGuid ≠ 0 en la cabecera activa) tiene la BAT o los
metadatos del fichero desfasados: hay que reproducir el registro para leer el estado coherente. Se
hace en memoria, sin escribir nunca el fichero, según el apartado «Log» de [MS-VHDX].

**Estructuras.** Búfer circular de sectores de 4 KiB. Entrada: cabecera `loge` (CRC-32C de toda la
entrada con el campo de suma a cero, EntryLength, Tail, SequenceNumber, DescriptorCount, LogGuid,
FlushedFileOffset, LastFileOffset), descriptores de 32 bytes (`desc`: TrailingBytes, LeadingBytes,
FileOffset, SequenceNumber; `zero`: ZeroLength, FileOffset, SequenceNumber) y un sector `data` por
descriptor de datos (SequenceHigh, 4084 bytes, SequenceLow). El sector reproducido es LeadingBytes
+ los 4084 bytes + TrailingBytes. Una entrada puede cruzar el final del registro.

**Entrada válida**: firma, EntryLength múltiplo de 4 KiB entre 4 KiB y la longitud del registro,
Tail múltiplo de 4 KiB dentro del registro, LogGuid igual al de la cabecera activa, descriptores con
firma conocida y el número de secuencia de la entrada, sectores de datos con firma `data` y
SequenceHigh/Low iguales a la secuencia, exactamente los sectores de descriptores más los de datos, y
CRC-32C correcto. La estructura se comprueba antes que el CRC: dos entradas estructuralmente válidas
no pueden solaparse (todo sector interior empieza por `desc`, `zero` o `data`), así que el CRC de un
registro hostil cuesta como mucho unas pocas veces su tamaño.

**Secuencia activa** (algoritmo de la especificación): se recorre el registro desde el principio; en
cada posición con una entrada válida se forma la secuencia más larga de entradas válidas contiguas
(circularmente, sin pasar de la longitud del registro) con números de secuencia consecutivos. Está
completa si el Tail de su cabeza (la última) señala una entrada de la secuencia. La activa es la
completa con la cabeza de mayor número de secuencia; se reproducen, en orden, las entradas desde la
señalada por el Tail hasta la cabeza, y la escritura posterior prevalece. Por tanto se **ignoran**,
como manda la especificación: entradas de otros registros (otro LogGuid), una entrada posterior a
medio escribir (CRC o secuencia incorrectos) y secuencias más recientes incompletas (su Tail señala
una entrada que no está).

**Tamaño del fichero.** FlushedFileOffset de la cabeza mayor que el fichero → fichero truncado,
`Corrupt`. LastFileOffset mayor que el fichero → el fichero se considera ampliado con ceros hasta
ese tamaño (tamaño efectivo), como haría la reproducción real.

**Modo estricto (y también tolerante: el registro es una estructura crítica, ADR-020)**: es `Corrupt`
un registro pendiente sin ninguna secuencia completa (registro vacío, CRC o LogGuid incorrectos,
Tail fuera de la secuencia…), LogLength 0 con LogGuid ≠ 0, FlushedFileOffset o LastFileOffset no
múltiplos de 1 MiB, FileOffset o ZeroLength no múltiplos de 4 KiB, y escrituras en el identificador o
las cabeceras (primeros 192 KiB: la cabecera activa ya se ha elegido para localizar el registro), en
el propio registro, desbordadas o más allá del tamaño efectivo. Las escrituras en las tablas de
regiones, los metadatos, la BAT o los bloques se aplican antes de interpretarlos.

**Superposición y memoria.** Se lee el registro entero (≤ 64 MiB) y se conserva para reconstruir los
sectores reproducidos; se guardan los sectores vigentes (desplazamiento y posiciones en el registro,
el más reciente de cada desplazamiento salvo que lo anule un tramo a ceros posterior) y los tramos a
ceros fusionados. Toda esa memoria se reserva en el presupuesto de la apertura (ADR-020, addendum),
ampliado con `open_memory_budget(2 × LogLength)`; el pico es de unas 2,3 veces la longitud del
registro (unos 2,3 MiB con el registro habitual de 1 MiB). Una lectura toma el fichero real, ceros
entre su final y el tamaño efectivo, y encima los tramos a ceros y los sectores reproducidos.

**Diferencias con qemu.** qemu solo reproduce al abrir en lectura y escritura y, si no encuentra
ninguna secuencia válida con LogGuid ≠ 0, abre sin reproducir; aquí eso es `Corrupt` (no se puede
saber si las estructuras del fichero quedaron a medio actualizar). qemu redondea un LastFileOffset no
alineado; aquí es `Corrupt` (la especificación exige múltiplos de 1 MiB).

## Decisiones

- **Registro pendiente**: se reproduce en memoria (apartado anterior); nunca se escribe el fichero.
  Las cabeceras antiguas con LogGuid ≠ 0 pero menor secuencia no afectan.
- **Tablas de regiones**: se toma la primera copia válida; no se exige que ambas coincidan.
- **UNDEFINED/UNMAPPED** se entregan como **dispersos** (`Allocation::Sparse`, ceros; en el mapa de
  validez, estado `Sparse`), igual que `qemu-img` (que los convierte a RAW como ceros). Motivos: solo
  se abren discos sin padre (HasParent es `ExternalReference`), así que no hay otro disco del que
  heredar el contenido; la especificación declara indefinido el contenido de un bloque UNDEFINED y
  permite que uno UNMAPPED se lea como ceros o como datos anteriores, de modo que ningún contenido es
  «el del disco». No son daño (no se marcan `Unreadable`) ni datos (no se lee el desplazamiento que
  pudiera conservar la entrada, que podría apuntar a datos obsoletos o a otro bloque). Consecuencia
  para la recuperación: los restos que un bloque UNMAPPED (descartado con TRIM) aún conserve en el
  fichero no se exponen por esta vía; quedan al alcance de un análisis del fichero VHDX como RAW.
- Bits reservados de la BAT (3–19) no se validan, para no rechazar imágenes de otros escritores.
- El último bloque solo necesita estar en el fichero en la parte que cubre el tamaño virtual.

## Modo tolerante (ADR-020)

`VhdxImage::open_with(file, true)` (o `OpenOptions::tolerant`). Correspondencia inequívoca de una entrada
de bloque: estado NOT_PRESENT/UNDEFINED/ZERO/UNMAPPED (ceros) o FULLY_PRESENT con el bloque (parte útil)
entero en el fichero, a partir de 1 MiB, sin solapar la zona de cabeceras, el registro, ninguna región
ni el bloque de otra entrada. Estados 4, 5 y PARTIALLY_PRESENT, desplazamientos desbordados, bloques
fuera del fichero y solapes descartan el bloque (en un solape, **todos** los implicados). Un bloque
descartado se lee con `IoError::Read` y `allocation` lo informa como `Data`. Las entradas de mapa de
bits con estado inválido no afectan a los datos de un disco sin padre: se cuentan («N entradas de mapa
de bits no válidas ignoradas») y se ignoran. `describe()` añade «; modo tolerante: N bloques descartados
(no fiables)». Identificador, cabeceras, registro, tablas de regiones, metadatos y ubicación de la BAT
siguen siendo críticos (`Corrupt` en ambos modos). Pruebas (`vhdx.rs`, 3): fuera del fichero,
desbordado, en la zona de cabeceras, sobre el registro, los metadatos y la BAT, duplicada, estados 4 y 5,
PARTIALLY_PRESENT y varias a la vez (comparación sector a sector con `mr_io::resilient`, inmutabilidad),
mapa de bits inválido ignorado y estructuras críticas en ambos modos.

## Pruebas y resultados reales

`cargo test -p mr-image`: 16 unitarias (2 de CRC-32C con vectores públicos: «123456789» → `0xE3069283`,
32 ceros → `0x8A9136AA`, 32×`0xFF`, secuencias creciente y decreciente de RFC 3720; 14 de VHDX sobre
imágenes construidas en ficheros temporales): lectura completa y cruzando bloques, último bloque
parcial, `allocation`, sector 4096 y fijo, entradas de mapa de bits intercaladas (bloque 4096 en el
índice 4097), cabecera de mayor secuencia / CRC malo / ambas malas, tablas de regiones (copia de
respaldo, región requerida desconocida, BAT fuera del fichero, desalineada o solapada, demasiadas
entradas, falta de metadatos), registro mal situado, metadatos inválidos (bloque, sectores, tamaño
virtual, duplicados, requeridos desconocidos, fuera de región), LogGuid no nulo, HasParent con y sin
localizador, estados de BAT inválidos, bloques fuera del fichero o solapados, tamaños enormes (64 TiB,
1 TiB, región de 4 GiB) sin reservar memoria. Se comprueba que el fichero no cambia.

`cargo test -p mr-image -- --ignored` (qemu-img 11.1.2): 2 pruebas, 14 imágenes. RAW determinista de
64 MiB (datos pseudoaleatorios en MiB 0–2, 10, 17–20, 40 y 63; ceros en el resto) y RAW de
37 MiB + 3584 B (no múltiplo del bloque). Variantes `qemu-img convert -f raw -O vhdx -o …`:
`subformat=fixed`, `fixed,block_size=1M`, `fixed,block_state_zero=off`, `dynamic`,
`dynamic,block_size=1M`, `dynamic,block_size=8M`, `dynamic,block_size=256M`. En cada una: tamaño
virtual == `qemu-img info`, SHA-256 de la lectura completa == `qemu-img convert -f vhdx -O raw`
(== RAW original), 300 rangos aleatorios deterministas idénticos, `allocation()` coherente con
`qemu-img map --output=json` (lo no-dato en qemu es disperso aquí, lo que aquí es dato lo es en
qemu, lo disperso se lee como ceros), fichero VHDX y RAW intactos (SHA-256, tamaño y fecha).
Resultado: todas pasan (≈2 min en perfil de depuración).

Observaciones: qemu marca como ZERO los bloques nulos también en discos fijos (salvo
`block_state_zero=off`), por lo que un VHDX fijo de qemu puede tener tramos dispersos. `qemu-img map`
de VHDX informa todo como datos (el controlador de qemu no expone estado de bloques), así que la
comparación de asignación es una comprobación de coherencia, no de igualdad; la exactitud de la
dispersión se verifica con las pruebas unitarias.

Registro pendiente (issue #10). Unitarias en `core/mr-image/src/vhdx/tests/registro.rs` (10) con
entradas construidas según la especificación y un **modelo independiente** (cada escritura se aplica
a mano sobre una copia del fichero sin registro, y la lectura con registro pendiente debe coincidir
byte a byte, en tamaño y en asignación con la de esa copia): datos y ceros sobre la BAT, cambio del
tamaño virtual en los metadatos, tres entradas con vuelta al principio del registro (una entrada
cruza el final), orden entre ceros y datos dentro de una entrada y entre entradas, secuencia más
reciente incompleta, entrada a medio escribir y entradas de otro LogGuid ignoradas, Tail que limita
lo reproducido, saltos y discrepancias de secuencia (descriptor, SequenceHigh/Low) que cortan la
secuencia, LastFileOffset que amplía el fichero, modo tolerante con registro y BAT dañada, y 18
registros no válidos que dan `Corrupt` en ambos modos. Se comprueba que el fichero no cambia.
Corrupción (`core/mr-image/tests/corruption.rs`): 300 inversiones deterministas dirigidas también a
las entradas del registro (la mitad con sus CRC-32C recalculadas; abren 163 en modo estricto) y
fichero acortado después de abrir.

Contra qemu-img (`core/mr-image/tests/vhdx_log_qemu.rs`, 2 pruebas ignoradas). qemu se niega a abrir
en solo lectura un VHDX con registro pendiente válido («contains a log that needs to be replayed») y
lo reproduce al abrirlo en lectura y escritura. Oráculo: `qemu-img info` sobre el VHDX pendiente debe
fallar con ese mensaje (qemu también ve el registro válido) y `qemu-img convert --image-opts
driver=vhdx,file.filename=COPIA,read-only=off -O raw` sobre una **copia desechable** reproduce el
registro en la copia y da la referencia; la lectura de MemRescue del original debe tener el mismo
SHA-256, y el original no cambia (SHA-256, tamaño y fecha). Casos: (1) entradas escritas por el
propio qemu al convertir un RAW a VHDX dinámico (bloques de 8 MiB y de 1 MiB), vueltas a dejar
pendientes poniendo su LogGuid en la cabecera activa, primero tal cual (idempotente) y después con
los sectores que escriben puestos a ceros en el fichero (la BAT queda vacía; sin reproducir el
registro el contenido difiere); (2) entradas sintéticas sobre un VHDX de qemu: datos y ceros sobre
dos bloques y la BAT, tres entradas con vuelta al principio del registro (con y sin cambio de la BAT)
y una cuarta entrada a medio escribir que ambos ignoran. Resultado: todas coinciden con qemu.

## Limitaciones

- No se abren discos diferenciales.
- Registro pendiente: si no hay ninguna secuencia válida la imagen se rechaza (qemu la abriría sin
  reproducir); no hay prueba con registros escritos por Hyper-V (solo por qemu y sintéticos).
- No se valida la ubicación de los bloques de mapa de bits de sectores (no se usan en discos sin padre).
- El modo tolerante no tiene prueba con qemu-img (la BAT se localiza en la tabla de regiones); se cubre
  con imágenes sintéticas.
- No hay prueba con qemu de sector lógico de 4096 B (qemu-img no lo permite crear); cubierto con
  imágenes sintéticas.
- Los metadatos de usuario y las regiones opcionales desconocidas se ignoran.
