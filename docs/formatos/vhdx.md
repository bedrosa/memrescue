# VHDX (Virtual Hard Disk v2): lector de solo lectura

Módulos: `core/mr-image/src/vhdx.rs` (`VhdxImage`) y `core/mr-image/src/crc32c.rs` (`crc32c`, `Crc32c`).
Pruebas cruzadas: `core/mr-image/tests/vhdx_qemu.rs`.

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
| Registro pendiente (LogGuid ≠ 0) | `IoError::UnsupportedFeature` («registro pendiente; no se reproduce todavía») |
| Versión de cabecera ≠ 1 o versión de registro ≠ 0 | `UnsupportedFeature` |

`describe()` devuelve p. ej. «VHDX dinámico, bloques de 32 MiB, sector lógico 512 B, sector físico 4096 B».
«Fijo» se deduce de la bandera LeaveBlocksAllocated de File Parameters (el formato no tiene un campo de tipo).

## Validaciones

1. Fichero ≥ 1 MiB e identificador `vhdxfile` en el desplazamiento 0.
2. **Cabeceras** en 64 KiB y 128 KiB: firma `head` y CRC-32C de 4 KiB con el campo de suma a cero.
   Se usa la válida con **mayor número de secuencia** (con una sola válida, esa). Ninguna válida → `Corrupt`.
3. Registro: si LogLength ≠ 0, desplazamiento y longitud alineados a 1 MiB, fuera de la zona de
   cabeceras y dentro del fichero.
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

## Decisiones

- **Registro pendiente**: no se reproduce. Leer sin reproducirlo podría devolver metadatos o BAT
  obsoletos; rechazarlo es lo seguro. Las cabeceras antiguas con LogGuid ≠ 0 pero menor secuencia
  no afectan.
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

## Limitaciones

- No se reproduce el registro ni se abren discos diferenciales.
- No se valida la ubicación de los bloques de mapa de bits de sectores (no se usan en discos sin padre).
- El modo tolerante no tiene prueba con qemu-img (la BAT se localiza en la tabla de regiones); se cubre
  con imágenes sintéticas.
- No hay prueba con qemu de sector lógico de 4096 B (qemu-img no lo permite crear); cubierto con
  imágenes sintéticas.
- Los metadatos de usuario y las regiones opcionales desconocidas se ignoran.
