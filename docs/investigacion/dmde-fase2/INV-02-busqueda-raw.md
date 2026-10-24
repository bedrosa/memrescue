# INV-02 · Búsqueda RAW por firmas: modelo de firma, barrido y cálculo de tamaño

Fecha: 2026-10-24 · Issue #30 (hito M7 · RAW por firmas) · Filas RW-01 a RW-08 de
[PARIDAD_DMDE_PRO.md](../../PARIDAD_DMDE_PRO.md) §6.

> Sala limpia (ADR-001): en esta fase **no** se ha ejecutado DMDE ni se ha analizado su binario (ni
> decompilación, ni desensamblado, ni herramientas de ingeniería inversa). Las posiciones del binario que
> figuran como «punto de partida» de INV-02 en `ROADMAP.md` proceden de una fase anterior y **no se han
> usado**. Las fuentes son el manual público de DMDE, especificaciones públicas de los formatos de fichero
> y la documentación pública de carvers de código abierto (solo como práctica general; no se copia código
> ni tablas de firmas, ADR-009). El diseño de §7 es propio de MemRescue.

Leyenda:

- **OBS** = observado en la documentación pública de DMDE (se indica URL y sección).
- **REPO** = consta en un documento anterior de este repositorio (se indica documento y apartado).
- **SPEC** = especificación pública de un formato o documentación pública de otra herramienta (se indica
  URL y sección).
- **INF** = inferencia propia, razonada a partir de lo anterior.
- **DESC** = desconocido: no se puede afirmar con las fuentes permitidas.

## 1. Fuentes consultadas

| Fuente | URL | Uso |
|---|---|---|
| Manual de DMDE: *Full Scan* | <https://dmde.com/manual/fullscan.html> | Búsqueda RAW, sintaxis de firmas, opciones, control del escaneo |
| Manual de DMDE: *File Panel* | <https://dmde.com/manual/filepanel.html> | Carpeta de resultados RAW y marcas de estado |
| Manual de DMDE: *Cluster Map* | <https://dmde.com/manual/clustermap.html> | Mapa de clústeres (base del filtrado) |
| Manual de DMDE: *Data Recovery* | <https://dmde.com/manual/datarecovery.html> | Errores de E/S en la recuperación |
| Manual de DMDE: *Editions* | <https://dmde.com/manual/editions.html> | Límite de ficheros por operación en la edición gratuita |
| DMDE, *FAQ* | <https://dmde.com/faq.html> | Recuperación por firmas sin nombres |
| PhotoRec, *Step By Step* | <https://www.cgsecurity.org/wiki/PhotoRec_Step_By_Step> | Opciones (paranoid, bruteforce, expert, keep corrupted), espacio libre o completo |
| PhotoRec, descripción y *How PhotoRec works* | <https://www.cgsecurity.org/wiki/PhotoRec> | Tamaño de bloque, barrido por bloque, tamaño por cabecera, comprobaciones |
| PhotoRec, *Add your own extension* | <https://www.cgsecurity.org/wiki/Add_your_own_extension_to_PhotoRec> | Firmas de usuario y sus límites |
| Scalpel, `scalpel.conf` | <https://github.com/sleuthkit/scalpel/blob/master/scalpel.conf> | Cabecera, pie, tamaño máximo, comodín, `REVERSE`, `NEXT` |
| foremost(8) | <https://manpages.ubuntu.com/manpages/noble/man8/foremost.8.html> | Modo rápido (inicio de sector), tamaño de bloque, extractores integrados |
| bulk_extractor(1) | <https://manpages.opensuse.org/Tumbleweed/bulk_extractor/bulk_extractor.1.en.html> | Páginas con margen (`-G`, `-g`) |
| S. Garfinkel, *Carving contiguous and fragmented files with fast object validation*, DFRWS 2007 | <https://dfrws.org/presentation/carving-contiguous-and-fragmented-files-with-object-validation/> | Validadores rápidos, reensamblado de bifragmentados |
| ITU-T T.81 (JPEG) | <https://www.w3.org/Graphics/JPEG/itu-t81.pdf> | Anexo B: marcadores, segmentos, datos con relleno 0xFF00 |
| JFIF 1.02 | <https://www.w3.org/Graphics/JPEG/jfif3.pdf> | Segmento APP0 |
| CIPA DC-008 (Exif 2.3x) | <https://www.cipa.jp/std/documents/e/DC-X008-Translation-2019-E.pdf> | APP1 con miniatura JPEG incrustada |
| PNG (W3C) | <https://www.w3.org/TR/png-3/> | §5.2 firma, §5.3 estructura de trozos con CRC, IHDR/IEND |
| GIF89a | <https://www.w3.org/Graphics/GIF/spec-gif89a.txt> | Cabecera, bloques, trailer 0x3B |
| TIFF 6.0 | <https://www.itu.int/itudoc/itu-t/com16/tiff-fx/docs/tiff6.pdf> | Cabecera, IFD, tiras |
| PDF (ISO 32000-1, copia pública de Adobe) | <https://opensource.adobe.com/dc-acrobat-sdk-docs/pdfstandards/PDF32000_2008.pdf> | §7.5 cabecera, `startxref`, `%%EOF`, actualizaciones incrementales, linealizado (anexo F) |
| ZIP, APPNOTE.TXT | <https://pkware.cachefly.net/webdocs/casestudies/APPNOTE.TXT> | §4.3: cabeceras locales, directorio central, EOCD, ZIP64 |
| ECMA-376 parte 2 (OPC) | <https://ecma-international.org/publications-and-standards/standards/ecma-376/> | `[Content_Types].xml` en paquetes OOXML |
| OASIS ODF 1.2 parte 3 | <https://docs.oasis-open.org/office/v1.2/os/OpenDocument-v1.2-os-part3.html> | §3.3: entrada `mimetype` primera y sin comprimir |
| [MS-CFB] | <https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-cfb/> | §2.2 cabecera, sectores de 512/4096, FAT, directorio |
| ISO/IEC 14496-12 (ISO BMFF) | <https://standards.iso.org/ittf/PubliclyAvailableStandards/> | §4.2 cajas: tamaño, `largesize`, tamaño 0, `ftyp` |
| Apple, *QuickTime File Format* | <https://developer.apple.com/documentation/quicktime-file-format> | Átomos (misma estructura que ISO BMFF) |
| Microsoft, *AVI RIFF File Reference* | <https://learn.microsoft.com/en-us/windows/win32/directshow/avi-riff-file-reference> | `RIFF` con tamaño, `AVIX` (OpenDML) |
| ID3v2.4 | <https://id3.org/id3v2.4.0-structure> | §3.1 cabecera con tamaño *syncsafe* |
| SQLite, *Database File Format* | <https://www.sqlite.org/fileformat2.html> | §1.3: cadena de cabecera, tamaño de página, tamaño en páginas y su validez |
| 7-Zip, `7zFormat.txt` | <https://github.com/ip7z/7zip/blob/main/DOC/7zFormat.txt> | *SignatureHeader*: desplazamiento y tamaño de la cabecera final |
| RAR 5.0, *technote* | <https://www.rarlab.com/technote.htm> | Firma y bloque de fin de archivo |
| RFC 1952 (gzip) | <https://www.rfc-editor.org/rfc/rfc1952> | §2.3 cabecera, ISIZE |
| RFC 3533 (Ogg) | <https://www.rfc-editor.org/rfc/rfc3533> | §6 páginas con CRC, indicador de fin de flujo |
| RFC 8794 (EBML) y RFC 9559 (Matroska) | <https://www.rfc-editor.org/rfc/rfc8794>, <https://www.rfc-editor.org/rfc/rfc9559> | Cabecera EBML, tamaño de `Segment` (también desconocido) |
| Microsoft, *PE Format* | <https://learn.microsoft.com/en-us/windows/win32/debug/pe-format> | `e_lfanew`, tabla de secciones, tabla de certificados |
| Documentos del repositorio | `PARIDAD_DMDE_PRO.md` (§6, SC-02 a SC-04, SC-12, RC-02, UX-03), `DECISIONES.md` (ADR-001, ADR-002, ADR-008, ADR-009, ADR-019, ADR-024, ADR-027), `formatos/mapa-de-validez.md`, INV-01 §7.2 y §7.3 | Contexto y restricciones |

En esta sesión se recuperaron y leyeron las páginas del manual de DMDE y la documentación de PhotoRec,
Scalpel, foremost y bulk_extractor. Los datos de formato citados en §4 (firmas, campos de tamaño y
marcadores de fin) son estables y de uso general; antes de implementar cada calculador se debe releer la
sección citada (§10, pregunta 12).

## 2. Lo que dice el manual de DMDE (comportamiento visible)

| # | Afirmación | Etiqueta y fuente |
|---|---|---|
| R1 | Las firmas se usan «como último recurso» para encontrar ficheros perdidos cuando no es posible recuperarlos por el FS. | OBS: fullscan.html, *Raw: File Signatures* |
| R2 | Las firmas también sirven para mejorar y comprobar la calidad de los resultados de NTFS, exFAT y HFS, y para marcar y filtrar ficheros hallados a la vez por el FS y por firma (mayor probabilidad de recuperación). | OBS: fullscan.html |
| R3 | Limitaciones declaradas: solo ficheros con firma; con fragmentados solo se recupera bien el primer fragmento; sin nombres ni estructura de directorios; el tamaño original no siempre se detecta. | OBS: fullscan.html |
| R4 | Si un tipo no tiene firma de fin ni información de tamaño interna, el fichero se considera continuo **hasta la siguiente firma encontrada**; buscar todos los tipos presentes puede mejorar la detección de tamaño. | OBS: fullscan.html |
| R5 | Para algunos tipos integrados, fragmentos consecutivos pueden combinarse en un fichero, por lo que el panel puede mostrar menos ficheros de los que contó el escaneo. | OBS: fullscan.html |
| R6 | Los resultados RAW aparecen en una carpeta virtual propia al abrir un volumen; una carpeta simbólica adicional excluye lo ya hallado por el FS del volumen usando el mapa de clústeres. | OBS: fullscan.html; filepanel.html (icono de la carpeta) |
| R7 | Una firma es una secuencia fija de bytes al principio del fichero; se escribe como texto o con códigos hexadecimales; un comodín marca un byte variable; una máscara de bits fija solo algunos bits del byte anterior. | OBS: fullscan.html, *File Signature* |
| R8 | Si la posición de la firma no es fija hay que dar desplazamientos mínimo y máximo; la firma entera debe caber en los primeros 4096 bytes del fichero. | OBS: fullscan.html |
| R9 | Solo se pueden añadir tipos con una firma única; se añaden desde los parámetros del escaneo. | OBS: fullscan.html |
| R10 | Se puede generar una firma a partir de muestras (extensiones y ruta de los ficheros de ejemplo) y probar si una firma identifica un fichero concreto. | OBS: fullscan.html |
| R11 | Opción de usar el tipo «para el cálculo del FS»: comprueba la coherencia entre el FS y lo hallado por firma y marca los ficheros del FS como hallados o no por firma. | OBS: fullscan.html |
| R12 | Una firma se marca «fiable» solo si tiene al menos 4 bytes distintos en posición fija; así un falso positivo no trunca otros ficheros. | OBS: fullscan.html |
| R13 | Tipo contenedor: puede contener otros tipos; marcarlo evita que se trunque al encontrar esos tipos dentro. | OBS: fullscan.html |
| R14 | Firma de fin (EOF) opcional para determinar el tamaño. | OBS: fullscan.html |
| R15 | Marcas del panel: hallado por firma con firma de fin encontrada, sin ella, o con tamaño detectado («probablemente fragmentado o parcialmente sobrescrito»); hallado **por su firma de fin**; fichero del FS hallado también por firma, o **no** hallado por firma en su posición («puede estar dañado, ser de otro tipo o no reconocido»). | OBS: filepanel.html, *File icons* |
| R16 | Start/Stop: si no cambian los parámetros, el escaneo continúa donde se detuvo; el registro del escaneo se guarda, se carga para continuar o reutilizar y puede guardarse automáticamente. Escaneo «a saltos» que cubre el disco gradualmente. | OBS: fullscan.html, *Log-file*, *Parameters* |
| R17 | El indicador de calidad «%»/«Check» de NTFS, exFAT y HFS cuenta enlaces correctos entre estructuras del FS y ficheros hallados por firma. | OBS: fullscan.html |
| R18 | La edición gratuita limita a 4000 ficheros por operación de recuperación. | OBS: editions.html |
| R19 | En la recuperación, los ficheros con errores de E/S pueden saltarse, recuperarse ignorando el error, ocultarse o moverse a una carpeta aparte; con imágenes sin mapa de errores se puede cruzar una lista de sectores. | OBS: datarecovery.html |
| R20 | El manual no describe: granularidad del barrido (sector, clúster, sin alinear), tamaños máximos por tipo, categorías de firmas, qué tipos tienen calculador de tamaño, ni qué hace con sectores ilegibles durante la búsqueda RAW. | OBS (por ausencia): fullscan.html, filepanel.html, clustermap.html, datarecovery.html |

## 3. Práctica pública de carvers de código abierto

| # | Práctica | Fuente |
|---|---|---|
| P1 | PhotoRec determina primero el tamaño de bloque (del superbloque o del sector de arranque si el FS está sano; si no, de la posición de los diez primeros ficheros hallados) y luego lee **bloque a bloque** comparando cada inicio de bloque con su base de firmas. | SPEC: cgsecurity.org/wiki/PhotoRec, *How PhotoRec works* |
| P2 | Cuando la cabecera declara el tamaño, PhotoRec trunca al tamaño declarado y descarta el fichero si queda más corto; los formatos de flujo (MP3) se analizan según llegan los datos y se detienen al acabar el flujo; al hallar una firma nueva comprueba la coherencia del fichero en curso. | SPEC: ídem |
| P3 | Tras recuperar un fichero, PhotoRec reexamina bloques anteriores donde se encontró una firma de un fichero demasiado corto, lo que recupera algunos fragmentados; *bruteforce* amplía esto a JPEG con un coste de CPU muy alto. | SPEC: ídem y *Step By Step*, opciones |
| P4 | Por defecto (*paranoid*) los ficheros recuperados se verifican y los inválidos se rechazan; *keep corrupted files* los conserva. El modo experto fuerza tamaño de bloque y desplazamiento. Se puede buscar en todo el volumen o solo en el espacio no asignado. | SPEC: *Step By Step* |
| P5 | Firmas de usuario de PhotoRec: una por línea con extensión, desplazamiento y valor (texto o hexadecimal); no permiten comprobar contenido, tamaño ni pie, para lo que hay que modificar el programa. | SPEC: *Add your own extension* |
| P6 | Scalpel y foremost: reglas de extensión, sensibilidad a mayúsculas, tamaño máximo (Scalpel admite mínimo:máximo), cabecera, pie opcional; `?` como comodín; `REVERSE` busca el **último** pie hacia atrás desde cabecera + máximo (PDF con varios `%%EOF`); `NEXT` corta en el primer pie sin incluirlo. | SPEC: scalpel.conf; foremost(8) |
| P7 | foremost tiene extractores integrados que analizan la estructura de varios formatos, un modo rápido que solo busca cabeceras al inicio de cada sector y una opción que desactiva la detección de corruptos. | SPEC: foremost(8), `-q`, `-a`, `-t` |
| P8 | bulk_extractor procesa el origen en páginas (16 MiB por defecto) con un margen adicional configurable. | SPEC: bulk_extractor(1), `-G`, `-g` |
| P9 | El tallado rápido y preciso es un problema de decisión en varios niveles que valida o descarta candidatos con validadores rápidos (JPEG, OLE, ZIP); los validadores permiten reensamblar ficheros en dos fragmentos (*bifragment gap carving*). | SPEC: Garfinkel, DFRWS 2007, resumen |

## 4. Especificaciones: firma, tamaño y validación por formato

«Bits fijos» = bits fijados por la firma de cabecera en posición fija (estimación de su poder de
discriminación, §7.6). Abreviaturas de tamaño: **E** = recorrido de estructura (exacto), **D** = campo de
tamaño declarado, **P** = firma de fin, **S** = hasta la siguiente firma fiable o el máximo.

| Formato | Cabecera (desplazamiento) | Bits fijos | Tamaño | Validación barata | Notas | Fuente |
|---|---|---|---|---|---|---|
| JPEG/JFIF/Exif | `FF D8 FF` + marcador (0) | 24 (+8 si se exige `E0`/`E1`/`DB`/`EE`) | E: segmentos con longitud de 2 bytes hasta SOS; datos entrópicos hasta un marcador que no sea `FF00` ni RSTn; fin en `FF D9` | Segmentos con longitud coherente; DQT/DHT/SOF presentes antes de SOS; RSTn en secuencia | APP1 Exif contiene una miniatura con su propio `FFD8…FFD9`: un pie ingenuo corta en la miniatura | SPEC: T.81 anexo B; JFIF; Exif §4.5 |
| PNG | `89 50 4E 47 0D 0A 1A 0A` (0) | 64 | E: trozos longitud(4, BE) + tipo + datos + CRC hasta `IEND` | IHDR primero con longitud 13; CRC-32 de cada trozo | El CRC por trozo detecta exactamente dónde empieza un dato ajeno (útil para fragmentación) | SPEC: PNG §5.2, §5.3 |
| GIF | `GIF87a` / `GIF89a` (0) | 48 | E: descriptores y subbloques hasta el trailer `3B` | Tamaño de la paleta coherente con los indicadores | | SPEC: GIF89a §17 a §27 |
| BMP | `BM` (0) | 16 | D: `bfSize` (offset 2, LE) | `bfOffBits` < `bfSize`; cabecera DIB de tamaño conocido (12, 40, 108, 124) | Firma débil: no fiable sin validador | SPEC: documentación pública de BITMAPFILEHEADER |
| TIFF y RAW de cámara basados en TIFF | `49 49 2A 00` / `4D 4D 00 2A` (0) | 32 | E: máximo de (fin de cada IFD, fin de cada tira o tesela, fin de cada valor fuera de línea) | IFD con número de entradas razonable y tipos válidos | Muchos formatos RAW de cámara son TIFF con extensiones propias | SPEC: TIFF 6.0 §2 |
| PDF | `%PDF-` (0, admite basura previa hasta 1024 bytes) | 40 | P con regla «último»: último `%%EOF` dentro del máximo, sin cruzar otra firma fiable; D si está linealizado (`/L` del diccionario de linealización) | `startxref` antes del `%%EOF` apunta a `xref` o a un objeto de flujo de referencias | Las actualizaciones incrementales añaden varios `%%EOF`: el primero trunca | SPEC: ISO 32000-1 §7.5.2, §7.5.5, §7.5.6, anexo F |
| ZIP | `50 4B 03 04` (0) | 32 | E: EOCD `50 4B 05 06` + 22 + longitud del comentario; la posición del directorio central declarada en el EOCD debe ser coherente; ZIP64 con `50 4B 06 06` y `50 4B 06 07` | CRC-32 y tamaños de cada entrada; nombres válidos | Una cabecera local aparece en cada entrada: solo la primera es inicio de fichero (contenedor de sí mismo) | SPEC: APPNOTE §4.3.6, §4.3.16, §4.3.14 |
| OOXML (docx, xlsx, pptx) | ZIP cuyo contenido incluye `[Content_Types].xml` | (ZIP) | Como ZIP | Como ZIP; el tipo final sale de las partes (`word/`, `xl/`, `ppt/`) | Clasificación posterior a la firma | SPEC: ECMA-376 parte 2 |
| ODF (odt, ods, odp) | ZIP con `mimetype` como primera entrada sin comprimir: `mimetypeapplication/vnd.oasis.opendocument.` en el offset 30 | > 200 | Como ZIP | Como ZIP | Firma fuerte en posición fija | SPEC: ODF 1.2 parte 3 §3.3 |
| OLE/CFB (doc, xls, ppt, msg, msi) | `D0 CF 11 E0 A1 B1 1A E1` (0) | 64 | E: tamaño de sector (512 o 4096) por `SectorShift`; tamaño = (mayor sector referenciado por FAT, DIFAT, directorio y mini FAT + 1) × tamaño de sector + cabecera | Versión 3 con sectores de 512 o 4 con 4096; cadenas de FAT sin ciclos | Tipo por los nombres del directorio (`WordDocument`, `Workbook`, `PowerPoint Document`, `__substg1.0_`) | SPEC: [MS-CFB] §2.2, §2.3, §2.6 |
| MP4/MOV/3GP/HEIC (ISO BMFF) | `ftyp` en el offset 4 (MOV antiguo: `moov`, `mdat`, `wide`, `free` en 4) | 32 | E: cajas de nivel superior tamaño(4, BE) + tipo; tamaño 1 → `largesize` de 8 bytes; tamaño 0 → hasta el final del fichero (se trata como S); fin en la primera caja de tipo desconocido | Tipos de caja de 4 caracteres imprimibles; `ftyp` con marcas conocidas; `moov` presente | Recorrer las cajas solo lee cabeceras: barato aunque `mdat` mida GB | SPEC: ISO/IEC 14496-12 §4.2; QTFF |
| AVI, WAV, WebP (RIFF) | `RIFF` (0) + forma en el offset 8 (`AVI `, `WAVE`, `WEBP`) | 64 | D: 8 + tamaño (offset 4, LE); AVI > 1 GiB encadena segmentos `RIFF…AVIX` que se suman | Primer trozo coherente (`hdrl`, `fmt `, `VP8`) | | SPEC: AVI RIFF File Reference |
| MP3 | `ID3` (0) o sincronía de trama (11 bits a 1) | 24 (ID3) / 11 (trama) | E: cabecera ID3v2 (10 + tamaño *syncsafe*, + 10 si hay pie) y luego tramas con longitud por tasa, frecuencia y relleno; `TAG` de 128 bytes (ID3v1) al final si existe | Al menos N tramas consecutivas válidas (N = 4 propuesto) | La sincronía sola es muy débil: no fiable | SPEC: ID3v2.4 §3.1; cabecera de trama MPEG pública |
| Ogg (ogg, opus) | `OggS` (0) | 32 | E: páginas con su CRC hasta la de indicador de fin de flujo de cada flujo lógico | CRC de cada página | | SPEC: RFC 3533 §6 |
| Matroska/WebM | `1A 45 DF A3` (0) | 32 | D: tamaño del elemento `Segment` (puede ser «desconocido» → S) | DocType `matroska` o `webm` | | SPEC: RFC 8794 §4; RFC 9559 |
| FLAC | `fLaC` (0) | 32 | S (sin tamaño total); E opcional por tramas | Bloque STREAMINFO primero | | SPEC: formato FLAC público |
| SQLite 3 | `SQLite format 3\0` (0) | 128 | D: tamaño de página (offset 16; 1 = 65536) × páginas (offset 28) si el contador de validez (offset 92) iguala al de cambios (offset 24) | Tamaño de página potencia de 2 entre 512 y 65536 | Si el recuento no es válido: S | SPEC: fileformat2 §1.3 |
| 7z | `37 7A BC AF 27 1C` (0) | 48 | D: 32 + `NextHeaderOffset` (offset 12) + `NextHeaderSize` (offset 20) | CRC de la *StartHeader* (offset 8) | | SPEC: 7zFormat.txt, *SignatureHeader* |
| RAR 4 / RAR 5 | `Rar!\x1A\x07\x00` / `Rar!\x1A\x07\x01\x00` (0) | 56 / 64 | E: bloques con su tamaño hasta el bloque de fin de archivo | CRC de las cabeceras de bloque | | SPEC: technote RAR 5.0 |
| gzip | `1F 8B 08` (0) | 24 | S, o E descomprimiendo deflate hasta el final del flujo + 8 (CRC32, ISIZE) | Indicadores reservados a cero; CRC32 final | Descomprimir cuesta CPU: solo con presupuesto | SPEC: RFC 1952 §2.3 |
| PE (exe, dll) | `MZ` (0) + `PE\0\0` en `e_lfanew` | 16 + 32 | E: máximo de (fin de cada sección en el fichero, fin de la tabla de certificados) | `e_lfanew` < 4096; máquina y número de secciones razonables | | SPEC: PE Format |
| Texto, HTML, XML | Prefijos como `<?xml`, `<!DOCTYPE html`, `<html` | 32 a 64 | S hasta el primer byte no textual o el máximo | Proporción de bytes imprimibles o UTF-8 válido | Muy propenso a falsos positivos; desactivado por defecto | INF |

Consecuencias (INF):

- Los formatos se reparten en tres familias de tamaño. **Exacto por estructura o campo** (PNG, GIF, ZIP,
  OLE, ISO BMFF, RIFF, SQLite, 7z, RAR, Ogg, TIFF, PE, BMP, JPEG): el tamaño no depende de lo que haya
  detrás. **Por pie** (PDF): depende de elegir el pie correcto (primero frente a último). **Desconocido**
  (texto, FLAC, gzip sin descomprimir, Matroska de tamaño desconocido): solo cabe S, que es la regla R4 de
  DMDE.
- Un calculador exacto que encuentra una incoherencia antes del final (CRC de PNG, marcador inválido en
  JPEG, caja de tipo imposible) señala **dónde** se rompe la continuidad: es la marca natural de
  «probablemente fragmentado o parcialmente sobrescrito» (R15).
- El mismo calculador sirve de validador barato: rechazar una cabecera cuyo recorrido falla en el primer
  bloque elimina la mayor parte de los falsos positivos de firmas cortas (P4, P9).

## 5. Inferencias sobre la búsqueda RAW de DMDE

| # | Inferencia | Base |
|---|---|---|
| I1 | La búsqueda RAW forma parte del mismo barrido que la búsqueda de FS del Full Scan (mismo diálogo de parámetros, mismo registro y reanudación): una sola lectura secuencial alimenta ambos. | INF: R1, R2, R16, R17 |
| I2 | La firma de cabecera tiene posición fija o un intervalo de posiciones, siempre dentro de los primeros 4096 bytes; el patrón admite bytes exactos, comodines y máscaras por byte, pero no repeticiones ni alternativas. | INF: R7, R8 |
| I3 | Hay tres fuentes de tamaño: calculador integrado («información de tamaño interna»), firma de fin y, en su defecto, la siguiente firma. «Fiable» decide qué firmas pueden cerrar un fichero abierto y «contenedor» impide que las firmas internas lo cierren. | INF: R4, R12, R13, R14 |
| I4 | Que exista la marca «hallado por su firma de fin» indica que un pie sin cabecera también genera un resultado, probablemente hacia atrás hasta un límite. | INF: R15 |
| I5 | La combinación de fragmentos consecutivos en un fichero (R5) sugiere que el barrido produce aciertos parciales (p. ej. por bloques) que luego se agrupan por tipo. | INF: R5 |
| I6 | El filtrado de lo ya hallado por el FS se hace al presentar (carpeta simbólica con mapa de clústeres), no excluyendo áreas del barrido. | INF: R6 |
| I7 | El cruce FS ↔ firma compara el inicio de cada fichero del FS con la existencia de una firma reconocida en esa posición, y alimenta tanto el indicador de calidad como la marca de cada fichero. | INF: R2, R11, R15, R17 |

## 6. Desconocido

- DESC: granularidad del barrido (cada sector, cada clúster, cada byte) y si es configurable.
- DESC: qué tipos integrados tienen calculador de tamaño y cuáles usan pie o siguiente firma (RW-04).
- DESC: tamaños máximos por tipo y cómo se fija el límite cuando no hay pie ni siguiente firma.
- DESC: alcance hacia atrás de un resultado hallado solo por su firma de fin.
- DESC: tratamiento de sectores ilegibles dentro de una firma o de un fichero RAW, y nombres de los
  ficheros RAW recuperados.
- DESC: consumo de memoria y estructura del registro guardado con resultados RAW.
- DESC: formato interno de las firmas de usuario guardadas (irrelevante: MemRescue define el suyo, ADR-009).

Ninguna de estas incógnitas bloquea MemRescue: §7 define un comportamiento propio, completo y comprobable.

## 7. Propuesta de diseño propio: `mr-raw`

### 7.1 Objetivos e invariantes

1. **Un solo barrido secuencial** del área seleccionada, compartible con el escáner de FS de M6
   (`mr-scan`), que alimenta el emparejador y los calculadores de tamaño a la vez.
2. **Memoria acotada** e independiente del tamaño del dispositivo; los resultados se vuelcan a la base de
   trabajo (SQLite, ADR-008), no se acumulan en RAM.
3. **Un byte no fiable nunca es evidencia** (ADR-019): ni una firma ni un calculador leen bytes de sectores
   `Unreadable` o `Skipped`.
4. **Tamaño explicable**: cada resultado declara de dónde sale su tamaño (`estructura`, `campo`, `pie`,
   `siguiente_firma`, `maximo`) y si el final se verificó.
5. **Determinista**: mismas firmas, opciones y origen producen los mismos resultados y nombres.
6. **Solo lectura**; cancelable en cualquier momento; reanudable si no cambian firmas ni opciones.
7. Biblioteca de firmas propia y declarativa (ADR-009), versionada y con la fuente de cada dato.

### 7.2 Modelo y sintaxis de firmas (propia)

Fichero TOML por familia (`core/mr-raw/firmas/*.toml`), cargado y validado al inicio; las firmas de
usuario usan el mismo esquema en un fichero aparte del perfil de MemRescue.

```toml
[[tipo]]
id = "png"                      # identificador estable, [a-z0-9_]+
extensiones = ["png"]
categoria = "imagen"            # §7.13
fuente = "PNG (W3C) §5.2, §5.3"
contenedor = false
tamano_min = 67
tamano_max = "256 MiB"
calculador = "png"              # integrado; omitido si no hay

  [[tipo.cabecera]]             # varias cabeceras = alternativas
  patron = '89 "PNG" 0D 0A 1A 0A'
  desplazamiento = 0            # o [min, max] con max + longitud ≤ 4096

  [tipo.pie]                    # opcional, solo si no hay calculador
  patron = '"IEND" ?? ?? ?? ??'
  regla = "primero"             # primero | ultimo
  incluir = true                # el pie forma parte del fichero
  busqueda_max = "256 MiB"
```

Gramática del patrón (EBNF; espacios separan elementos y se ignoran):

```
patron     = elemento { elemento } ;
elemento   = byte | mascara | comodin | salto | texto | rango ;
byte       = HEX HEX ;                       (* 4B *)
mascara    = HEX HEX "/" HEX HEX ;           (* valor/máscara: 02/03 fija los dos bits bajos *)
comodin    = "??" ;                          (* un byte cualquiera *)
salto      = "{" NUM [ "," NUM ] "}" ;       (* de n a m bytes cualesquiera, total ≤ 64 *)
texto      = [ "i" ] '"' { CARACTER } '"' ;  (* ASCII; con i, sin distinguir mayúsculas *)
rango      = "[" HEX HEX "-" HEX HEX "]" ;   (* un byte dentro del intervalo *)
```

Diferencias con lo observado en DMDE (R7, R8): máscara escrita junto al byte (`valor/máscara`), saltos
acotados y rangos de byte, texto sin distinguir mayúsculas, alternativas como cabeceras separadas y pie con
regla `primero`/`ultimo` (P6). Se mantiene el límite de 4096 bytes para la posición de la firma (paridad
RW-02) y se valida al cargar.

Calculadores (`calculador`) y validadores son código de MemRescue, no expresiones del fichero: el fichero
solo los nombra. Campos declarativos adicionales para tamaños declarados simples, sin código:

```toml
  [tipo.campo_tamano]           # tamaño = valor × escala + ajuste
  desplazamiento = 4
  ancho = 4                     # 1, 2, 4 u 8
  orden = "le"                  # le | be
  escala = 1
  ajuste = 8
```

Errores de carga (patrón vacío, firma fuera de 4096, máscara sin bits, `tamano_min > tamano_max`,
`id` duplicado, calculador desconocido) son fatales y se informan con fichero y línea; una firma de usuario
inválida no detiene la carga del resto.

### 7.3 Emparejador

- Compilación: cada patrón se reduce a una lista de `(desplazamiento, valor, máscara)` más saltos; se
  elige como **ancla** la secuencia de bytes totalmente fijos más larga (preferiblemente de 2 a 4 bytes)
  y el resto se comprueba al encontrar el ancla.
- Modo alineado (§7.4): para cada inicio candidato `p` se consulta una tabla de 65 536 entradas indexada
  por los dos primeros bytes fijos de las firmas con desplazamiento 0; las firmas con desplazamiento
  distinto o variable se agrupan por su desplazamiento. Coste constante por inicio candidato.
- Modo sin alinear: búsqueda multipatrón de las anclas sobre el búfer (Aho-Corasick o una técnica SIMD
  equivalente; dependencia a evaluar en `HERRAMIENTAS_TERCEROS.md`), y verificación completa en cada
  acierto. Se descartan los aciertos cuyo inicio calculado (`posición del ancla − desplazamiento`) no
  respeta el intervalo de la firma.
- Un acierto cuyo intervalo de bytes toca un sector no fiable se descarta antes de comparar (§7.10).
- Bordes de búfer: cada bloque se procesa con un margen de `4096 + longitud máxima de patrón` bytes del
  siguiente (P8); un acierto se atribuye solo al bloque que contiene su inicio, para no duplicarlo.

### 7.4 Granularidad del barrido

| Modo | Inicios candidatos | Uso | Por defecto |
|---|---|---|---|
| `sector` | Múltiplos de `S_dev` (o 512 si se fuerza) desde el inicio del área | Dispositivos y particiones sin FS conocido | Sí |
| `cluster` | Inicio de cada clúster del volumen seleccionado (origen y tamaño del FS reconocido) | Volumen conocido: menos aciertos falsos y más rápido | Al abrir un volumen con FS reconocido |
| `byte` | Cada byte | Ficheros incrustados, imágenes de memoria, ficheros dentro de contenedores no reconocidos | No (opción explícita) |

- La granularidad fija dónde puede **empezar** un fichero; los calculadores y la búsqueda de pies siempre
  trabajan a nivel de byte.
- En `sector` y `cluster`, una firma con desplazamiento variable `[min, max]` se busca en
  `p + min … p + max` para cada inicio `p` (no en todo el búfer).
- Los ficheros incrustados (miniatura Exif, JPEG dentro de un PDF o de un ZIP sin comprimir) solo se
  encuentran en modo `byte`; en los otros modos se informan, si el calculador los detecta, como
  **hijos** del contenedor, no como resultados independientes (§7.7).
- Advertencia en la interfaz: `cluster` con un tamaño de clúster equivocado pierde ficheros; si la
  propuesta sale de un FS dañado se sugiere `sector` (paralelo a P1 y P4).

### 7.5 Determinación del tamaño

Orden de preferencia por resultado, de más a menos fiable:

1. **`estructura`**: recorrido del formato (tabla de §4). Si el recorrido termina en su marcador de fin,
   `final_verificado = sí`.
2. **`campo`**: tamaño declarado (BMP, RIFF, SQLite, 7z, Matroska) comprobado contra `tamano_min` y
   `tamano_max`; `final_verificado = no` salvo que el formato tenga una comprobación del final (p. ej.
   CRC de la cabecera final de 7z).
3. **`pie`**: búsqueda del pie hasta `busqueda_max`, sin cruzar el inicio de otra firma fiable que no sea
   interna de un contenedor; regla `primero` o `ultimo` (PDF usa `ultimo`, P6).
4. **`siguiente_firma`**: hasta el inicio del siguiente resultado con firma **fiable** (R4, R12), o hasta
   el primer clúster asignado a un fichero vivo del FS si hay mapa de clústeres (mejora propia, §7.9).
5. **`maximo`**: `tamano_max` del tipo, recortado al final del área.

Reglas:

- Un calculador que falla **antes** de llegar al final devuelve el tamaño hasta el último punto coherente
  y el estado `roto_en(desplazamiento)`; el resultado se marca `probablemente_fragmentado` (R15) y se
  recupera hasta ese punto o hasta el máximo según la opción `recortar_en_rotura` (por defecto sí).
- Un calculador que falla en el **primer** bloque rechaza el acierto (validación, §7.6), salvo con la
  opción `conservar_invalidos` (equivalente a P4 *keep corrupted*), que lo registra con `invalido`.
- Si el tamaño exacto supera `tamano_max` el resultado se conserva, se recorta al máximo y se marca
  `excede_maximo`; si es menor que `tamano_min`, se rechaza.
- Calculadores con coste alto (descompresión deflate de gzip) solo se ejecutan con la opción
  `calculo_profundo` y con un tope de CPU por fichero; sin ella, el tamaño es `siguiente_firma`.

### 7.6 Validación y falsos positivos

- **Fuerza de una firma**: `bits_fijos` = suma de bits fijados por los elementos en posición fija (la
  máscara cuenta sus bits; comodines, saltos y rangos no cuentan). Una firma es **fiable** si
  `bits_fijos ≥ 32` y sus bytes fijos no son todos iguales, criterio propio próximo a R12 pero medido en
  bits para tener en cuenta las máscaras.
- Estimación (INF) en datos aleatorios: el número esperado de aciertos falsos es `inicios × 2^-bits_fijos`.
  Un disco de 4 TB en modo `sector` tiene unos 7,8 × 10⁹ inicios: con 16 bits (`BM`) salen unos 120 000
  aciertos falsos; con 32 bits, unos 2; con 64 bits, ninguno. Los datos reales no son aleatorios (ceros,
  texto), por lo que la cifra solo ordena el riesgo.
- Consecuencias: una firma **no fiable** nunca cierra un fichero abierto (regla 4 de §7.5) y solo produce
  resultado si su calculador o validador la confirma; sin validador, se carga desactivada y la interfaz
  lo explica.
- Validadores de primer nivel (P9): baratos, sobre los primeros bloques (§4, columna «validación barata»).
  Validadores de segundo nivel (CRC de cada trozo PNG, CRC de entradas ZIP, CRC de páginas Ogg, tramas MP3
  consecutivas): se ejecutan durante el recorrido y deciden `final_verificado` y `roto_en`.
- Un acierto dentro de un fichero ya abierto de tipo contenedor o con tamaño exacto en curso no genera un
  resultado independiente (evita que la cabecera local de cada entrada ZIP o la miniatura Exif aparezcan
  como ficheros), salvo en modo `byte` con la opción `incrustados` (§7.7).

### 7.7 Contenedores e incrustados

- `contenedor = true` (R13) o calculador de tamaño exacto: los aciertos de otras firmas dentro de
  `[inicio, fin)` del fichero en curso no lo cierran.
- Con la opción `incrustados` (modo `byte`), esos aciertos se registran como **hijos** con referencia al
  padre y categoría propia; la interfaz los muestra bajo el padre y no se cuentan dos veces en las
  estadísticas.
- Un fichero ZIP no se divide en OOXML/ODF/JAR por firmas distintas: se reconoce como ZIP y el calculador
  reclasifica el tipo por su contenido (§4).

### 7.8 Fragmentación

- **Paridad (RW-03)**: se asume contigüidad desde la cabecera, como R3. Los resultados con `roto_en` o con
  tamaño `campo` cuya validación interna falla se marcan `probablemente_fragmentado`.
- **Unión de bloques consecutivos (R5)**: no aplica en el diseño propio, que nunca parte un fichero en
  aciertos de bloque; el emparejador ya produce un acierto por inicio de fichero.
- **Mejora opcional (RW-08, no paridad)**: reensamblado de **dos fragmentos** para formatos con validador
  fuerte (PNG y ZIP por CRC, JPEG por decodificación de la secuencia de MCU, Ogg por CRC de página), en la
  línea de P3 y P9:
  1. Para cada resultado `roto_en(r)`, candidatos a continuación: inicios de clúster (o sector) en el
     espacio no asignado y no reclamado por otro resultado exacto, hasta una distancia máxima.
  2. Para cada candidato se valida la continuación desde `r` hasta el siguiente punto de control (siguiente
     CRC o marcador); se acepta la primera que verifique y se sigue hasta el fin.
  3. Presupuesto explícito de lecturas y CPU por fichero y global; al agotarse, el fichero queda como
     `probablemente_fragmentado`.
  4. El resultado se marca `reensamblado` con la lista de tramos; nunca se presenta como íntegro salvo
     que el formato tenga un control de integridad completo que lo verifique (CRC de todas las entradas
     ZIP o de todos los trozos PNG): entonces `reensamblado_verificado`.
- Fuera de alcance: más de dos fragmentos, formatos sin validador y ficheros intercalados.

### 7.9 Interacción con los sistemas de archivos conocidos

- **Área de barrido** (`area`): dispositivo, partición, volumen o rango; el barrido es siempre sobre bytes
  del dispositivo, independiente del FS.
- **Exclusión** (`excluir_asignado`, por defecto **no**): con un volumen y su mapa de clústeres (SC-12),
  los clústeres asignados a ficheros vivos se pueden excluir **del barrido** para ahorrar tiempo. Por
  defecto se barre todo y se filtra **al presentar** (como I6), porque el cruce de la regla siguiente
  necesita las firmas de los ficheros vivos y porque el mapa puede estar mal si el FS está dañado.
- **Filtrado al presentar** (RW-05): vista `[RAW]` completa y vista `[RAW sin asignados]`, que oculta los
  resultados cuyo inicio cae en un clúster asignado a un fichero vivo, borrado recuperable o encontrado del
  FS (consulta por rangos sobre el mapa, sin releer datos).
- **Cruce FS ↔ firma** (R2, R11, R15): para cada fichero del FS con datos, se mira si hay un resultado RAW
  que **empieza** en su primer clúster y si el tipo coincide con la extensión: marca `firma = coincide`,
  `firma = distinta` (otro tipo), `firma = ausente` (tipo con firma conocida pero sin acierto: posible
  daño) o `firma = no_aplica`. Este cruce alimenta el indicador de calidad de M6 (SC-04, INV-03).
- **Final por asignación** (mejora propia): un fichero RAW sin tamaño exacto (`siguiente_firma`) se corta
  también en el primer clúster asignado a un fichero vivo distinto, con la anomalía correspondiente.
- **Marcado**: todo resultado lleva categoría `raw` (reservada en INV-01 §7.2), carpeta sintética
  `[RAW]` y no hereda nombre ni tiempos del FS.

### 7.10 Mapa de validez y sectores ilegibles

- Cada bloque llega con el estado por sector de ADR-019. El emparejador y los calculadores consultan una
  vista «bytes fiables» (`Read`, `Recovered`, `Sparse`); un patrón de relleno o una firma que coincida
  sobre `Unreadable` o `Skipped` **no** es un acierto.
- Firma ilegible: si los bytes de la cabecera de un inicio candidato no son fiables, el inicio se cuenta en
  `inicios_no_verificables` (estadística por área) y no se intenta.
- Calculador que encuentra bytes no fiables a mitad de fichero: no puede seguir el recorrido. El tamaño pasa
  a `pie`, `siguiente_firma` o `maximo` según el tipo, con la anomalía `ilegible_en(desplazamiento)`; si
  el recorrido puede **saltar** la zona (cajas ISO BMFF o RIFF cuyo tamaño está en bytes fiables antes de
  la zona), lo hace y conserva el tamaño exacto.
- El resultado guarda los tramos con su estado (los de ADR-026 e INV-01 §7.3: `data`, `sparse`,
  `unreadable`, …) y un resumen `fiab_datos` (`integro` solo si `final_verificado` y todo fiable;
  `parcial` si hay tramos no fiables; nunca `integro` con `siguiente_firma` o `maximo`, que pasan a
  `integro_no_verificable`).
- La recuperación aplica la política de M4 (relleno marcado, informe de tramos, ningún hash sobre bytes
  sustituidos), equivalente a las opciones de R19.

### 7.11 Memoria y rendimiento en dispositivos de varios TB

- **Lector**: bloques secuenciales de 4 MiB alineados a sector, con lectura anticipada de 2 a 4 bloques
  (configurable) y margen de §7.3. Memoria del barrido: unos 16 a 24 MiB fijos.
- **Calculadores en flujo**: cada fichero abierto tiene un analizador con estado acotado (≤ 64 KiB) que
  consume los bloques según pasan; si necesita bytes más adelante (cabecera de la siguiente caja ISO BMFF,
  EOCD de un ZIP) registra «interés en el desplazamiento X» y se reactiva cuando el barrido llega a X. Así
  el cálculo de tamaño no requiere lecturas aleatorias ni una segunda pasada.
- **Límite de abiertos**: como máximo `max_abiertos` analizadores (4096 por defecto); al superarlo, el más
  antiguo se cierra con tamaño `siguiente_firma`/`maximo` y la anomalía `analisis_abandonado`. El estado
  total queda por debajo de 256 MiB en el peor caso.
- **Resultados**: se escriben por lotes en la base de trabajo (≈ 64 a 96 bytes por resultado más
  tramos); la interfaz pagina desde SQLite. 10⁷ resultados ocupan del orden de 1 GiB en disco y nada en
  RAM.
- **Rendimiento**: el barrido debe estar limitado por E/S. Objetivos: emparejador alineado ≥ 2 GB/s por
  núcleo y sin alinear ≥ 500 MB/s por núcleo con 200 firmas. Como referencia (INF), 4 TB a 200 MB/s
  (disco mecánico) son unas 5,6 h; el procesado se reparte en hilos por bloque con unión ordenada de
  aciertos, de modo que el resultado no depende del número de hilos.
- **Escaneo a saltos** (SC-02): el emparejador funciona igual sobre bloques no consecutivos; los
  calculadores que necesitan bytes de otro bloque se resuelven con lecturas aleatorias acotadas al final de
  cada salto. La reanudación (§7.12) unifica ambos modos.

### 7.12 Progreso, cancelación y reanudación

- Progreso cada 100 ms como máximo: bytes barridos sobre el área, velocidad, tiempo estimado, resultados
  por categoría, inicios no verificables y analizadores abiertos.
- Cancelación comprobada entre bloques y dentro de los calculadores largos: el resultado parcial es válido,
  tiene estado `Cancelada` y los ficheros abiertos se cierran con su mejor tamaño y la anomalía
  `barrido_interrumpido`.
- Punto de control cada 256 MiB (o 30 s) en la base de trabajo: posición del barrido, analizadores abiertos
  (su inicio y tipo) y huella de las opciones y del conjunto de firmas (SHA-256 de la biblioteca
  normalizada). Reanudar exige la misma huella (paridad con R16: «si no cambian los parámetros»); se
  reanuda desde el inicio del analizador abierto más antiguo y los aciertos repetidos se deduplican por
  `(inicio, tipo)`.
- Cambiar firmas u opciones obliga a un barrido nuevo; se conservan los resultados anteriores como sesión
  aparte.

### 7.13 Resultados, categorías y marcas

- Nodo de `mr-vfs` (INV-01 §7.2) con categoría `raw`, bajo `[RAW]\<categoría>\<extensión>\`. Nombre
  determinista por posición: `raw_<desplazamiento en bytes, hexadecimal de 12 dígitos>.<ext>` (p. ej.
  `raw_0000A1B2C000.jpg`); los hijos incrustados añaden `_en_<nombre del padre>`.
- Categorías propuestas: `imagen`, `video`, `audio`, `documento`, `hoja_calculo`, `presentacion`,
  `archivo_comprimido`, `base_de_datos`, `correo`, `ejecutable`, `texto`, `otros`. Se seleccionan por
  categoría o por tipo en la configuración del barrido y en el filtro de recuperación (RC-02).
- Marcas (RW-07, con iconos y nombres propios, UX-03): origen de tamaño (`estructura`, `campo`, `pie`,
  `siguiente_firma`, `maximo`), `final_verificado`, `probablemente_fragmentado`, `reensamblado`,
  `ilegible`, `excede_maximo`, `invalido`, `hallado_por_pie` y, en los ficheros del FS, el resultado del
  cruce de §7.9.
- **Hallado solo por el pie** (paridad con R15 e I4): un pie de un tipo con regla de pie y sin cabecera en
  el intervalo `busqueda_max` anterior se informa como `hallado_por_pie` con inicio desconocido; no se
  recupera por defecto (no hay un inicio fiable), pero se puede abrir en el editor de sectores.

### 7.14 Asistente de firmas desde muestras (RW-06)

1. Entrada: extensiones, categoría y al menos 3 ficheros de muestra (se recomiendan 10 o más).
2. Para cada desplazamiento `d` de los primeros 4096 bytes se calcula la máscara de bits iguales en todas
   las muestras (`AND` de los `XNOR` frente a la primera). Se eligen los bloques con más `bits_fijos`
   próximos al inicio y se construye el patrón con bytes exactos, máscaras y comodines.
3. Lo mismo sobre los últimos 4096 bytes para proponer un pie (solo si hay bits fijos suficientes en la
   misma posición relativa al final).
4. Comprobaciones antes de guardar: fuerza (§7.6), colisión con la biblioteca integrada y de usuario
   (mismo prefijo o prefijo contenido), y tasa de aciertos sobre un corpus negativo (una muestra del propio
   origen fuera de los ficheros de ejemplo, o datos aleatorios) con estimación de falsos positivos por TB.
5. «Probar»: ejecuta emparejador y calculador sobre un fichero o un rango del origen y muestra acierto,
   tamaño y origen del tamaño.

### 7.15 Interfaces

- **API Rust**: `RawSearch::new(origen, area, firmas, opciones)` → tarea de `mr-api` con progreso y
  cancelación; `RawResults` paginado desde la base de trabajo; `Signatures::load(rutas)` con errores por
  línea; `Signatures::from_samples(...)`; `Signatures::test(...)`.
- **ABI** (ADR-013): `mr_raw_*` con iteradores paginados y estructuras de resultado de tamaño fijo.
- **CLI**: `raw-scan <origen> [--area ...] [--modo sector|cluster|byte] [--tipos ...] [--categorias ...]
  [--firmas fichero.toml] [--reanudar]`, `raw-list`, `raw-sig-test`, `raw-sig-from-samples`; salida JSON
  estable para las pruebas.
- **Interfaz**: pestaña RAW en el diálogo de Full Scan (RW-01), carpeta `[RAW]` y vista filtrada en el
  panel de ficheros, editor de firmas de usuario con el asistente y la prueba.

### 7.16 Diferencias deliberadas respecto a lo observado

- Sintaxis propia en TOML, con saltos, rangos, texto sin mayúsculas y pie con regla (no la de DMDE).
- Fiabilidad medida en bits fijos, no en bytes distintos.
- Granularidad explícita (`sector`, `cluster`, `byte`) y visible.
- Origen del tamaño y verificación del final visibles por fichero; tramos ilegibles nunca ocultos.
- Corte por clúster asignado y reensamblado de bifragmentados como mejoras opcionales.
- Nombres de resultado por desplazamiento, no por contador.

## 8. Correspondencia con PARIDAD

| Fila | Cubierta por | Notas |
|---|---|---|
| RW-01 Búsqueda RAW integrada en Full Scan | §7.1, §7.4, §7.11, §7.12, §7.15 | Un barrido compartido con `mr-scan` |
| RW-02 Comodín, máscara, offset variable ≤ 4096 | §7.2, §7.3 | Sintaxis propia; límite de 4096 conservado |
| RW-03 EOF, límites, contenedores, «fiable» | §7.5, §7.6, §7.7 | Regla `primero`/`ultimo`; fiabilidad en bits |
| RW-04 Calculadores por formato | §4, §7.5 | Lista inicial propuesta en §10, pregunta 2 |
| RW-05 Filtrado con mapa de clústeres | §7.9 | Depende de SC-12 |
| RW-06 Firma desde muestras y prueba | §7.14 | |
| RW-07 Marcadores de estado RAW | §7.13 | Con origen del tamaño |
| RW-08 Fragmentados | §7.8 | Mejora opcional, bifragmentos con validador |
| SC-03 Pausa/reanudación | §7.12 | Huella de opciones y firmas |
| SC-04 Calidad con firmas | §7.9 (cruce FS ↔ firma) | Métrica en INV-03 |
| RC-02 Categorías | §7.13 | Categoría `raw` y subcategorías por tipo |
| UX-03 Marcas en el panel | §7.13 | Iconos propios |

## 9. Plan de pruebas

### 9.1 Generador de corpus sintético

Script determinista (`tools/gen_raw_corpus.py`, Python sin dependencias externas, semilla fija) que produce
imágenes y un manifiesto JSON con la verdad: por fichero `{tipo, inicio, longitud, sha256, tramos,
incrustado_en, estado_esperado}`.

- **Muestras por formato** construidas desde la especificación con la biblioteca estándar: PNG (zlib y
  CRC), GIF, BMP, ZIP (`zipfile`, también ZIP64 y con comentario), OOXML y ODF mínimos, SQLite (`sqlite3`),
  gzip, TIFF, RIFF/WAV, ISO BMFF mínimo (`ftyp`, `moov`, `mdat`, también con `largesize`), PDF con y sin
  actualización incremental, OLE mínimo según [MS-CFB], JPEG base escrito a mano (tablas de cuantificación
  y Huffman estándar del anexo K de T.81) con y sin miniatura Exif, MP3 con ID3v2 y tramas.
- **Disposición**: inicios alineados a 512, 4096 y 64 KiB y sin alinear; relleno entre ficheros con ceros,
  aleatorio, texto y **firmas falsas** (cabeceras válidas seguidas de basura); ficheros truncados,
  concatenados sin hueco, incrustados (JPEG en ZIP almacenado, miniatura Exif, JPEG en PDF).
- **Fragmentación conocida**: ficheros en 2 y 3 fragmentos con hueco conocido, fragmentos intercalados de
  dos ficheros, y fragmento sobrescrito por otro fichero.
- **Ilegibles**: manifiestos de `FaultyDevice` con sectores ilegibles en la cabecera, a mitad de un fichero
  con tamaño exacto saltable (ISO BMFF) y no saltable (JPEG), y relleno que imita una firma.
- **FS mixto**: imágenes NTFS del corpus de ADR-024 (ficheros vivos y borrados con verdad de TSK) para
  RW-05 y el cruce FS ↔ firma; se añade un escenario con ficheros conocidos borrados y clústeres
  reasignados.
- **Escala**: imagen dispersa de 2 TiB con una docena de ficheros en posiciones conocidas, para medir
  memoria máxima, tiempo y determinismo (los ceros `Sparse` no se leen, ADR-019).

### 9.2 Oráculos independientes

1. **Verdad del generador** (exacta para inicio, longitud y SHA-256).
2. **Validadores externos** sobre los ficheros tallados, en un ejecutor desechable de GitHub Actions
   (patrón de ADR-024): `file`/libmagic para el tipo, Pillow para imágenes, `zipfile.testzip`,
   `sqlite3 PRAGMA integrity_check`, `qpdf --check`, `ffprobe` para vídeo y audio. Comprueban que lo
   tallado es un fichero válido sin compartir código con MemRescue.
3. **PhotoRec** (paquete `testdisk` de Ubuntu, versión registrada) en el mismo ejecutor, en modo de
   órdenes no interactivo, sobre las imágenes sintéticas y sobre imágenes con **ficheros reales** generados
   en el ejecutor (ImageMagick, ffmpeg, LibreOffice sin interfaz) y copiados sin FS. Se compara el conjunto
   de SHA-256 recuperados por formato (exhaustividad y precisión de cada herramienta frente a la verdad),
   no la igualdad con PhotoRec, que tiene otro diseño. PhotoRec es GPL: solo se ejecuta como herramienta
   externa; no se copia código ni firmas (ADR-001, ADR-009).
4. Opcional: foremost y Scalpel como segunda referencia para los tipos con pie.

Métricas por formato y escenario: exhaustividad, precisión, tasa de tamaño exacto, falsos positivos por GB
en datos aleatorios y de texto. Los umbrales se fijan tras la primera medición (pregunta 9).

### 9.3 Pruebas propias

| Área | Pruebas |
|---|---|
| Sintaxis | Cada elemento de la gramática, errores con línea, límites (4096, salto ≤ 64), firmas de usuario inválidas no bloquean las integradas |
| Emparejador | Aciertos en bordes de bloque (dentro del margen), desplazamiento variable, máscaras, sin distinguir mayúsculas; igualdad entre modo `byte` y la búsqueda ingenua sobre datos aleatorios (prueba de propiedad) |
| Calculadores | Por formato: válido, truncado, roto a mitad, campos fuera de rango, `largesize`, tamaño 0, ZIP64, PDF con varios `%%EOF`; fuzzing acotado (sin pánicos, sin bucles, memoria acotada) |
| Fiabilidad | Firmas no fiables no cierran ficheros; falsas cabeceras rechazadas por validador |
| Contenedores | Entradas ZIP y miniatura Exif no generan resultados salvo con `incrustados` |
| Mapa de validez | Relleno con aspecto de firma nunca es acierto; ilegible en cabecera, a mitad saltable y no saltable; `fiab_datos` correcto |
| FS | Vista filtrada, cruce FS ↔ firma (coincide, distinta, ausente), exclusión del barrido |
| Reanudación | Cancelar en la k-ésima posición y reanudar da el mismo resultado que un barrido completo; huella distinta obliga a barrido nuevo |
| Determinismo | Dos ejecuciones y distinto número de hilos: mismos resultados y nombres |
| Escala | Memoria máxima por debajo del límite en la imagen de 2 TiB; tiempo dominado por E/S |
| Asistente | Muestras sintéticas con prefijo común conocido: la firma propuesta lo contiene; colisión detectada |
| Paridad | Escenarios de RW-01 a RW-07 de PARIDAD y, opcionalmente, comparación manual con DMDE por el propietario (ADR-002) |

## 10. Preguntas abiertas para el propietario

1. ¿Granularidad por defecto `sector` en dispositivos y `cluster` en volúmenes reconocidos, con `byte`
   solo a petición?
2. ¿Primera entrega de calculadores: JPEG, PNG, GIF, BMP, TIFF, PDF, ZIP (con OOXML y ODF), OLE, ISO BMFF,
   RIFF, MP3, SQLite, 7z, RAR? ¿Se aplazan Matroska, Ogg, FLAC, gzip profundo, PE y texto?
3. ¿Se barre por defecto todo el área y se filtra al presentar (propuesta), o se excluyen los clústeres
   asignados del barrido cuando hay volumen?
4. ¿Se mantiene el límite de 4096 bytes para la posición de la firma o se amplía (p. ej. 64 KiB) como
   mejora opcional para formatos con cabecera tardía?
5. ¿Se activa el corte por clúster asignado (§7.9) por defecto?
6. ¿Entra el reensamblado de bifragmentados (RW-08) en M7 o se pospone?
7. ¿Se muestran los incrustados (miniaturas Exif) como resultados hijos por defecto o solo con la opción?
8. ¿Se aceptan los nombres por desplazamiento (`raw_<hex>.<ext>`) y los nombres de categorías propuestos?
9. ¿Umbrales mínimos de exhaustividad y precisión frente al generador para marcar las filas RW como
   probadas, y si la comparación con PhotoRec es informativa o bloqueante?
10. ¿Se autoriza un workflow de GitHub Actions desechable (como ADR-024) con PhotoRec, ImageMagick, ffmpeg
    y LibreOffice para el oráculo y los ficheros reales? Requeriría un ADR nuevo.
11. ¿Biblioteca de búsqueda multipatrón de terceros para el modo `byte` o implementación propia?
12. Releer las secciones citadas de cada especificación al implementar su calculador (en esta fase solo se
    recuperaron el manual de DMDE y la documentación de los carvers).
13. ¿Se exportan y comparten las firmas de usuario (importar/exportar TOML) o quedan solo en el perfil
    local?
