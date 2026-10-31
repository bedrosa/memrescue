# Diario de la recuperación (JSON Lines, versión 1)

Escritor: `core/mr-recover/src/engine.rs` y `json.rs`. Issue #26 (M4); INV-01 §7.10. Es la **fuente de
verdad** de una recuperación ([`recuperacion.md`](recuperacion.md)): los informes y listas TXT, CSV y
HTML de #27 (RC-10 a RC-13) se derivan de él sin volver a leer el origen.

## Fichero

- **Ubicación**: la que indique el llamador (`--journal F`, `RecoverOptions::journal`, `journal` de
  `mr_recover_start`); por defecto `<destino>\MemRescue-registro\recuperacion-AAAAMMDD-HHMMSS-<id>.jsonl`
  (UTC). Nunca en el origen (se rechaza antes de empezar) y nunca sustituye un fichero existente
  (creación exclusiva; con la ubicación por defecto se añade `-2`, `-3`... si hiciera falta). La carpeta
  `MemRescue-registro` queda reservada: ninguna entrada recuperada la sustituye.
- **Codificación**: UTF-8 sin BOM; una línea por objeto JSON (RFC 8259), terminada en `\n`. Los
  caracteres de control y U+2028/U+2029 van escapados, así que ninguna línea se parte.
- **Escritura**: cada línea se escribe entera de una vez y se vacía al sistema al momento; al final se
  sincroniza con el disco. Si la ejecución se corta, todas las líneas completas son válidas; una
  recuperación terminada siempre acaba en una línea `fin`.
- **Orden**: `cabecera`, después una línea por directorio y por fichero en el orden del recorrido (un
  directorio aparece cuando se crea o reutiliza, o al salir de él si no se llegó a crear), y `fin`.
- **Estabilidad**: los nombres de campo y los códigos son estables dentro de una versión
  (`cabecera.version`); un cambio incompatible incrementa la versión. Campos nuevos se pueden añadir al
  final de un objeto sin cambiar la versión: los lectores deben ignorar los que no conocen.
- **Determinismo**: dos recuperaciones iguales dan las mismas líneas salvo `cabecera.id`,
  `cabecera.inicio`, `cabecera.destino.raiz`, `cabecera.origen.ruta` y `fin.fin`.

Las rutas de `destino` son **relativas a la raíz de destino**, con `/` como separador; el fichero de un
flujo alternativo escrito como flujo se indica `fichero:flujo`. Las fechas de los nodos se dan como
FILETIME (`*_filetime`, 100 ns desde 1601, UTC; 0 = desconocida) y en ISO 8601 con 7 decimales
(`null` si es 0 o anterior a 1970).

## Línea `cabecera`

| Campo | Contenido |
|---|---|
| `tipo` | `"cabecera"` |
| `version` | `1` |
| `motor` | Versión del motor |
| `id` | Identificador de la ejecución (hexadecimal) |
| `inicio` | Fecha y hora UTC (`AAAA-MM-DDTHH:MM:SSZ`) |
| `origen` | `ruta`, `descripcion`, `tamano`, `tamano_sector`, `lba_volumen` (LBA del origen donde empieza el volumen: partición o volumen encontrado; `null` si empieza en 0), `sistema_archivos` (`"ntfs"`), `tamano_cluster` |
| `destino` | `raiz` (ruta absoluta), `flujos_con_nombre`, `distingue_mayusculas`, `tamano_maximo_fichero` (`null` = sin límite conocido), `capacidades` (`"sonda"` u `"opciones"`) |
| `opciones` | `conflictos` (`preguntar`, `renombrar`, `omitir`, `omitir_si_identico`, `sobrescribir`), `estados_escritos` (lista de estados), `incluir_bytes_reasignados`, `relleno` (8 dígitos hexadecimales), `flujos_alternativos`, `fechas`, `atributos`, `marcar_incompletos`, `division` (`"auto"`, `"nunca"` o el tamaño de parte), `limite_renombrados`, `crear_directorios_vacios` |
| `seleccion` | Lista de `{nodo, ruta}` (ruta virtual) de los nodos seleccionados, sin los cubiertos por otros |
| `filtro` | `categorias`, `estados` (listas; vacía = todos), `solo_borrados`, `reglas`, `tamano_min`, `tamano_max`, `campo_fecha`, `fecha_min`, `fecha_max` (FILETIME o `null`) |
| `plan` | `ficheros`, `directorios`, `bytes` (de los flujos principales de los ficheros que se van a escribir, de los metadatos) |

## Campos comunes de `directorio` y `fichero`

| Campo | Contenido |
|---|---|
| `tipo` | `"directorio"` o `"fichero"` |
| `nodo` | Identificador en el árbol virtual |
| `ruta_virtual` | Ruta visible en el árbol (componentes escapados como en `vfs.md`) |
| `nombre` | Nombre original en su forma visible (UTF-8) |
| `nombre_utf16` | Solo si el nombre tiene unidades UTF-16 sueltas: las unidades originales en hexadecimal (4 dígitos cada una, en orden). Si es `null`, `nombre` es el original exacto |
| `nombre_cambiado`, `nombre_recortado` | El nombre de destino difiere del original (escape, reservado, recorte) |
| `categoria` | `vivo`, `borrado`, `huerfano`, `metadatos` |
| `borrado` | Entrada borrada |
| `origen_nombre` | `estructura`, `residual`, `registro`, `sintetico` |
| `indicadores` | Códigos de `mr_vfs::flags` (`deleted`, `from_index`, `hard_link`...) |
| `registro`, `secuencia` | Referencia MFT (`null` en carpetas sintéticas) |
| `tamano` | Tamaño de los metadatos |
| `tiempos` | `creacion`, `modificacion`, `cambio`, `acceso` (ISO) y `*_filetime` |
| `atributos` | Atributos DOS originales |
| `destino` | Ruta relativa escrita (`null` si no se escribió) |
| `ruta_larga` | La ruta completa supera 259 caracteres |
| `accion` | Ver abajo |
| `conflicto` | `null` o `{politica, nombre_pedido, motivo, decision, intentos}`: `motivo` `existe_en_destino` o `misma_recuperacion`; `decision` `renombrado`, `omitido`, `identico`, `sustituido`, `fusionado` u `omitido_limite`; `intentos` = número de sufijos probados |
| `flujos` | Lista de flujos (abajo); en directorios, sus flujos alternativos |
| `error` | `null` o `{clase, codigo, mensaje}`; `clase` `origen` o `destino` |

**Acciones de directorio**: `creado`, `fusionado` (ya existía y se reutiliza), `no_creado` (no recibió
ningún fichero), `omitido_conflicto`, `omitido_directorio` (un antecesor se omitió), `cancelado`,
`error`.

## Línea `fichero`

Además de los comunes:

| Campo | Contenido |
|---|---|
| `estado_inicial` | Estado de los datos del árbol virtual (sin leer datos) |
| `estado` | Estado final: igual o **rebajado** a `parcial` si la lectura encontró tramos no fiables; nunca se sube. Solo `integro` es íntegro |
| `accion` | `escrito`, `escrito_sustituyendo`, `omitido_estado` (su estado no se escribe con las opciones), `omitido_conflicto`, `omitido_identico` (ya está, mismo tamaño y SHA-256; `destino` es el existente), `omitido_directorio`, `no_soportado` (p. ej. EFS), `cancelado` (en curso al cancelar: no queda nada con su nombre), `error` |
| `marca_incompleto` | Ruta relativa del `.mrbad` o `null` |
| `fechas_aplicadas` | Se fijaron las fechas en lo escrito |

## Objeto de `flujos`

| Campo | Contenido |
|---|---|
| `nombre` | `""` = flujo principal; si no, el nombre del flujo alternativo |
| `destino` | Ruta relativa (`fichero`, `fichero:flujo` o el hermano `fichero%3Aflujo`); `null` con partes o si falló |
| `forma` | `principal`, `partes` (dividido), `flujo` (flujo con nombre del fichero de salida), `fichero_hermano` |
| `tamano` | Tamaño del flujo en el origen |
| `bytes_escritos` | Bytes escritos (o leídos para comparar en `omitido_identico`) |
| `bytes_por_estado` | `data`, `sparse`, `uninitialized` (fiables) y `unreadable`, `unmapped`, `outside_source`, `corrupt`, `possibly_overwritten` (no fiables), con recuentos exactos |
| `bytes_no_fiables` | Suma de los no fiables |
| `bytes_relleno` | Bytes escritos con el patrón de relleno |
| `tramos_no_fiables` | Hasta 10 000 tramos `{desplazamiento, longitud, estado, relleno, lba, lba_origen, sectores}`: desplazamiento en el flujo; `relleno` = se escribió el patrón (si no, los bytes del disco); `lba` del volumen y `lba_origen` (= `lba_volumen` de la cabecera + `lba`), en sectores del origen, y `sectores` (`null` si el tramo no está en disco: residente, disperso, sin mapa o comprimido). Un tramo que abarca varios runs se parte por run |
| `tramos_truncados` | Hay más tramos que los listados (los recuentos siguen siendo exactos) |
| `hashes` | `{ambito, crc32, md5, sha1, sha256}` de los bytes **escritos**. `ambito`: `contenido` (ningún tramo no fiable: es el hash del contenido), `escrito_con_relleno` (hay tramos no fiables: **solo** identifica el fichero escrito, nunca es el hash del original) o `no_calculado` (sin valores) |
| `partes` | `null` o lista `{destino, desplazamiento, longitud, hashes}` de cada parte (`hashes` con el mismo ámbito que el conjunto) |
| `error` | `null` o `{clase, codigo, mensaje}` del flujo |

## Línea `fin`

| Campo | Contenido |
|---|---|
| `estado` | `completa`, `cancelada` o `fallida` |
| `fin` | Fecha y hora UTC |
| `motivo` | Motivo de una recuperación `fallida` (destino lleno...), si no `null` |
| `totales` | `ficheros_planificados`, `directorios_planificados`, `ficheros_escritos`, `directorios_creados`, `directorios_fusionados`, `bytes_escritos`, `bytes_no_fiables`, `bytes_relleno`, `escritos_por_estado` (objeto por estado final), `omitidos_por_estado`, `omitidos_por_conflicto`, `omitidos_identicos`, `no_soportados`, `errores`, `renombrados`, `rutas_largas`, `marcados_incompletos`, `cancelados` |
| `lineas` | Número de líneas del diario, esta incluida (comprobación de integridad) |

## Ejemplo (abreviado)

```json
{"tipo":"cabecera","version":1,"motor":"0.2.0","id":"e7be58188d600000","inicio":"2026-10-25T19:27:20Z","origen":{"ruta":"...","descripcion":"RAW (sector a sector)","tamano":33554432,"tamano_sector":512,"lba_volumen":null,"sistema_archivos":"ntfs","tamano_cluster":4096},"destino":{"raiz":"...","flujos_con_nombre":true,"distingue_mayusculas":false,"tamano_maximo_fichero":null,"capacidades":"sonda"},"opciones":{...},"seleccion":[{"nodo":20,"ruta":"/sobrescritura"}],"filtro":{...},"plan":{"ficheros":5,"directorios":1,"bytes":...}}
{"tipo":"fichero","nodo":84,"ruta_virtual":"/sobrescritura/objetivo_3.bin","nombre":"objetivo_3.bin","nombre_utf16":null,...,"estado_inicial":"parcial","estado":"parcial","accion":"escrito","conflicto":null,"flujos":[{"nombre":"","destino":"sobrescritura/objetivo_3.bin","forma":"principal","tamano":65536,"bytes_escritos":65536,"bytes_por_estado":{"data":24576,...,"possibly_overwritten":40960},"bytes_no_fiables":40960,"bytes_relleno":40960,"tramos_no_fiables":[{"desplazamiento":0,"longitud":40960,"estado":"possibly_overwritten","relleno":true,"lba":43112,"lba_origen":43112,"sectores":80}],"tramos_truncados":false,"hashes":{"ambito":"escrito_con_relleno","crc32":"03c06363","md5":"...","sha1":"...","sha256":"..."},"partes":null,"error":null}],"marca_incompleto":"sobrescritura/objetivo_3.bin.mrbad","fechas_aplicadas":true,"error":null}
{"tipo":"fin","estado":"completa","fin":"2026-10-25T19:27:20Z","motivo":null,"totales":{...},"lineas":40}
```

## Pruebas

`core/mr-recover/tests` (un analizador JSON propio e independiente del escritor lee cada línea):
cabecera y `fin` con `lineas` exacto también al cancelar, acciones y conflictos, nombres con
`nombre_utf16`, tramos con LBA exacta de sectores ilegibles simulados y de clústeres reasignados del
corpus, ámbitos de hash, partes, flujos hermanos y determinismo entre dos ejecuciones.
