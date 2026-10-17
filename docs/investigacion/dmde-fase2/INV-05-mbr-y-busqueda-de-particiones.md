# INV-05 · MBR/EBR y búsqueda exprés de particiones perdidas

Fecha: 2026-10-17 · Objetivo: `dmde.exe` 4.4.4.842 (SHA-256 `57a4febb…47adb4`) · Ghidra 12.1.4 vía REA,
base de datos efímera, binario sin modificar. Consultas acotadas (13 llamadas REA) que parten de la
evidencia de la fase 1 (rango 0x608b00–0x613300, validador GPT `FUN_005ec9a0`).

> Sala limpia (ADR-001): este documento describe **comportamiento** y lo contrasta con las
> especificaciones públicas. No contiene pseudocódigo. La implementación de MemRescue se diseña en §5
> a partir de las especificaciones y se valida con el corpus propio.

Leyenda: **OBS** = observado en el decompilado o en bytes · **INF** = inferencia · **SPEC** = especificación pública.

## 1. Especificaciones públicas de referencia

- **MBR clásico** (UEFI 2.10 §5.2.1 *Legacy MBR*): tabla de 4 entradas de 16 bytes en 0x1BE; firma
  `0x55 0xAA` en 0x1FE. Entrada: `BootIndicator` (0x00 o 0x80; otros valores reservados), CHS inicial,
  `OSType`, CHS final, `StartingLBA` (u32), `SizeInLBA` (u32).
- **MBR protector** (UEFI §5.2.3): una entrada tipo 0xEE desde LBA 1.
- **Particiones extendidas / EBR** (práctica documentada por Microsoft y la literatura de IBM PC): cada
  EBR contiene una entrada de datos con LBA **relativa al propio EBR** y una entrada de enlace (tipo
  0x05/0x0F) con LBA **relativa al inicio de la partición extendida**.
- Tipos de partición: listas públicas de identificadores (p. ej. 0x07 NTFS/exFAT, 0x0B/0x0C FAT32,
  0x83 Linux, 0x82 Linux swap, 0xEE GPT protector, 0xAF HFS, 0x42 LDM).
- Copias de respaldo de los FS: FAT32 (`BPB_BkBootSec`, normalmente sector 6), exFAT (región de arranque
  de respaldo en el sector 12), NTFS (copia del boot sector en el último sector del volumen), ext2/3/4
  (copias del superbloque en grupos 1, 3, 5, 7, 9…), HFS+ (cabecera de volumen en +1024 y copia a −1024
  del final).

## 2. Hallazgos verificados en DMDE

### 2.1 Nombres de tipo de partición: `FUN_00608c50` + tabla 0x8abbd0 (OBS)

Evidencias: `ev_129e7280…` (decompilado), `ev_c3ace8db…` y `ev_70b57209…` (tabla y cadenas).
Traduce el byte de tipo a 13 nombres, agrupando variantes ocultas/LBA/de fabricantes:

| Nombre | Tipos agrupados |
|---|---|
| FAT12 | 0x01, 0x11, 0xC1, 0xE1 |
| FAT16 | 0x04, 0x06, 0x14, 0x16, 0xC4, 0xE4 |
| DOSEXT | 0x05 |
| NTFS/exFAT | 0x07, 0x17, 0x87, 0xC7 |
| FAT32 | 0x0B, 0x1B |
| FAT32x | 0x0C, 0x1C |
| FAT16x | 0x0E |
| EXTx | 0x0F |
| ExtFS | 0x83 |
| Lin SWAP | 0x82 |
| LDM | 0x42 |
| GPT | 0xEE |
| HFS | 0xAF |

Cualquier otro tipo (incluido **0x85**, extendida de Linux) no tiene nombre y se trata como
«desconocido» (ver 2.3).

### 2.2 Esquema de particionado: `FUN_00609390` (OBS)

Distingue: ninguno, (vacío), **MBR**, **GPT**, **GPT+MBR** (híbrido) y **Apple**. Coincide con el manual
(«hybrid … not supported and appear only as found»): el híbrido se *reconoce*.

### 2.3 Aceptación de un sector como tabla MBR/EBR: `FUN_0062edf0` (OBS)

Evidencia: `ev_8fc79b84…`. Reglas, en términos de comportamiento:

1. **Indicador de arranque**: si una entrada **no vacía** tiene bits distintos de 0x80 en el indicador
   (`flag & 0x7F ≠ 0`), el sector **no** es una tabla. En una entrada vacía solo se anota como anomalía.
2. **Más de una entrada activa** (0x80) → no es una tabla.
3. **Más de una entrada de enlace** de tipo 0x05/0x0F → no es una tabla (0x85 no cuenta).
4. Se calcula el final máximo `inicio + tamaño` de las entradas cuyo inicio cae dentro del dispositivo
   (las que empiezan fuera se ignoran), y por separado el de las entradas de **tipo conocido** (2.1).
5. Aceptación:
   - en LBA 0 basta con que haya alguna entrada que empiece dentro del dispositivo;
   - fuera de LBA 0 se exige además la firma 0x55AA y (alguna entrada de tipo conocido o ningún
     indicador anómalo);
   - en **modo estricto** (búsqueda de tablas perdidas) se exige firma, al menos un tipo conocido,
     **ningún** tipo desconocido y ningún indicador anómalo.
6. **Firma ausente en LBA 0**: la tabla se lee igualmente, pero se marca con un estado distinto
   (coherente con la acción «MBR On/Off» del manual).
7. **Direccionamiento**: las entradas de enlace (0x05/0x0F) dentro de una extendida conocida son
   relativas al inicio de esa extendida; el resto, relativas a la posición del sector leído.
8. Cada entrada registra indicador, tipo, LBA absoluta, tamaño y la posición de la entrada en disco
   (`posición_tabla + 0x1BE + 16 × índice`), para poder editarla después.
9. Las posiciones pueden ser **negativas** respecto a la base (redondeo hacia abajo): la búsqueda trabaja
   con desplazamientos relativos, lo que permite representar particiones desplazadas o que empiezan
   antes del inicio de una imagen parcial. (INF sobre la finalidad; el cálculo es OBS.)

### 2.4 Sondeo exprés de una posición candidata: `FUN_0062fdf0` (OBS en mecánica, INF en tipos)

Evidencia: `ev_c3c0817384…`. Progreso: `FUN_0062fa70` (`ev_bb1d45ca…`) usa el mensaje 1351 de la UI
(«Partition searching… #n LBA») con LBA = posición / tamaño de sector.

Dada una posición candidata (inicio y, opcionalmente, final esperado) y unas opciones, comprueba con
lecturas de 512 bytes:

| Comprobación | Posición leída | Correspondencia (SPEC) |
|---|---|---|
| Estructura principal del FS | inicio + desplazamientos del superbloque de cada FS (1–9 tipos) | Boot sector / superbloque |
| Copia en el sector 6 | inicio + 6 × tamaño de sector (con el tamaño indicado y con el del dispositivo) | FAT32 `BPB_BkBootSec` |
| Copia en el sector 12 | inicio + 12 × tamaño de sector | exFAT, región de arranque de respaldo |
| Copias a 64, 128, 256 y 384 MiB | inicio + 0x4000000 / 0x8000000 / 0x10000000 / 0x18000000 | Superbloques de respaldo de ext (grupos 1 y 3 con bloques de 4 KiB): INF |
| Copias a 64 y 256 MiB | inicio + 0x4000000 / 0x10000000 | Otro FS con copias a distancia fija: tipo no identificado (INF: Btrfs/otro) |
| −1024 bytes | candidato − 0x400 | Cabecera HFS+ en +1024 desde el inicio |
| Último sector | final − tamaño de sector (indicado, del dispositivo y 512) | Copia del boot sector NTFS al final |
| Tabla de particiones | inicio, si ningún FS coincide en el desplazamiento 0 | MBR/EBR (2.3) |

Otras observaciones:
- Si el tipo de la entrada de tabla sugiere un FS, se prueba primero ese FS en la posición esperada; para
  el tipo «2» se prueba un segundo reconocedor alternativo si el primero falla (INF: FAT16 frente a FAT32).
- Un código de retorno especial interrumpe toda la búsqueda (cancelación del usuario).
- Las opciones (máscara de bits) activan o desactivan cada familia de comprobaciones.

## 3. Lo que NO se ha verificado

- El **bucle externo** de la búsqueda exprés (qué posiciones candidatas se generan: alineaciones, saltos,
  extremos del disco). Se sabe que hay tres funciones que informan del mensaje 1351
  (`FUN_0062cda0`, `FUN_0062fa70`, `FUN_00637190`); solo se ha analizado la segunda vía.
- La lista exacta de desplazamientos de `FUN_0060ba00` / `FUN_00608bd0` (posición del superbloque por tipo de FS).
- Cómo se construye la lista final y se marcan los indicadores T/E/B/C/F/f/x (`FUN_0060ee00`, «sectores
  sobrantes»).
- La identidad de los tipos internos de FS 1–9 (solo se infieren por las posiciones de las copias).

## 4. Comparación con la especificación

| Aspecto | Especificación | DMDE | Implicación para MemRescue |
|---|---|---|---|
| Indicador de arranque | 0x00/0x80, resto reservado | Rechaza la tabla si una entrada usada tiene otro valor | Usar como criterio para distinguir tabla de datos aleatorios |
| Firma 0x55AA | Obligatoria | Opcional en LBA 0 (se marca), obligatoria fuera | Leer la tabla de LBA 0 sin firma pero marcarla |
| Varias activas | No definido explícitamente | Se rechaza | Criterio de plausibilidad |
| Extendida 0x85 | Tipo de Linux | No reconocida como enlace ni nombrada | **Mejora**: tratar 0x85 como enlace extendido |
| EBR | Datos relativos al EBR; enlace relativo a la extendida | Igual | Implementar según la especificación |
| Entradas fuera del disco | - | Se ignoran para el cálculo de extensión | Marcar como error, no descartar en silencio |

## 5. Diseño propuesto para MemRescue (original)

`mr-part` (M2), sin traducir lo observado:

1. `parse_mbr(sector, lba_tabla, contexto) -> MbrTable { entradas, anomalías, confianza }`, con
   `Confidence` (ADR-003) en lugar de aceptar/rechazar:
   - `Invalid`: indicador no válido en entrada usada, más de un enlace extendido.
   - `Suspicious`: varias activas, tipos desconocidos, entradas fuera del disco o solapadas, falta de firma.
   - `Valid`: firma correcta y entradas coherentes.
2. `walk_ebr_chain` con detección de ciclos, límite de enlaces y ambos modos de direccionamiento; 0x85
   aceptado.
3. Búsqueda exprés en dos fases, con progreso y cancelación (`mr-api` Task):
   - **Candidatos**: LBA 0, posiciones alineadas (63, 2048, múltiplos de 1 MiB), inicios declarados por
     las tablas y finales de particiones conocidas.
   - **Pruebas por candidato**: reconocedores de boot/superbloque y comprobación cruzada con sus
     copias de respaldo en las posiciones de la especificación de cada FS (sector 6, sector 12, último
     sector, superbloques de respaldo…). Un volumen confirmado por **primario + copia** recibe más
     confianza que uno con solo una de las dos.
4. Resultado: lista de particiones «encontradas» junto a las de la tabla, con indicadores propios
   (tabla, entrada, boot, copia, estructuras del FS) y estado borrada/encontrada.

## 6. Pruebas necesarias (para PARIDAD PT-01, PT-05)

- Corpus actual: `mbr_basic` (cadena de 2 EBR), `gpt_basic`, `gpt_4kn` → lectura correcta.
- Corpus a generar (generador propio, verificable con el validador independiente):
  - tabla MBR borrada con sectores de arranque FAT32/NTFS/exFAT sintéticos y sus copias de respaldo;
  - firma 0x55AA ausente; dos entradas activas; tipo 0x85; EBR en bucle; entrada fuera del disco;
  - imagen recortada al inicio (desplazamiento negativo).
- Hasta tener formateadores de terceros (WSL o VM, pospuesto) las pruebas de reconocimiento de FS usarán
  sectores de arranque sintéticos construidos según la especificación, documentándolo como limitación.

## 7. Evidencias

| ID | Contenido |
|---|---|
| `ev_1e5b5ed9ef80e548426c6ea5dfa50b38ce3976a35b1c66430389f98cc9a88cfe` | xrefs a «DOSEXT» |
| `ev_10ac57cdc5d0586b8add0cb7335d80086f52019cc919cc8acf7cc1dcb30f4e58` | xrefs a «GPT+MBR» |
| `ev_129e7280173fbef45beb1e1bfc76d33d0d9ecf99de3887f5797bc5aedfc85b21` | Decompilado de `FUN_00608c50` y `FUN_00609390` |
| `ev_c3ace8db1e975ced3158e2a20e857916582a12ecacd38ec1e0289fe8351098ed` | Tabla de punteros 0x8abbd0 |
| `ev_70b57209f58bcacb2eb7a77c61484143297e42d36fbf7a9099310ad564103089` | Cadenas de nombres de tipo |
| `ev_36e88477b55d71831e87a0560ccc1bd30db1351e6c62e80c7b2b7ba9017d308c` | Llamadores de `FUN_00608c50` |
| `ev_c236353ccb082ef0d676a429d7cf80dc654abcc4a74b63046ab54103cb31e79e` | Usos del mensaje 1351 (0x547) |
| `ev_3ed5349193079f3f0397cb8e9954090f21a9a5bb5c22a26efe19fbe355b4a90a` | Usos del mensaje 1370 (0x55A) |
| `ev_bb1d45ca0ac831a1f787ac9e40a689125aaf1a7b60b3502755250cdf3c518f70` | Decompilado de `FUN_0062fa70` |
| `ev_91a29f9eae510e0f61d1ed2f52b045c8a2576572fceb057b474bd0af74b78271` | Llamadores de `FUN_0062fa70` |
| `ev_c3c0817384458ca2c4c8e53ca969eee2598bb166b9e2b062926ecb81dfab8034` | Decompilado de `FUN_0062fdf0` |
| `ev_8fc79b84efac5f318886350965cbd67b79801ddb2c65f413fc92495ea7e9482b` | Decompilado de `FUN_0062edf0` |

Además se leyeron del fichero (solo lectura) los bytes de las instrucciones que cargan los mensajes
1351/1370 para confirmar que son `mov edx, imm32` (0x62d488, 0x62fae8, 0x63728d) y `mov [rbp+0x58], imm`
(0x60f549).
