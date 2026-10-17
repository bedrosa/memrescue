# 09 · Configuración, scripts y plantillas

Fuentes: cadenas del binario (OBS) y ficheros que acompañan al ejecutable en
la carpeta de la copia de referencia (leídos, no modificados). El parser/ejecutor de cada lenguaje
**no se ha decompilado** (análisis pendiente).

## Ficheros del producto

| Fichero | Función |
|---|---|
| `dmde.ini` / `dmde-444.ini` / `dmdedef.ini` | Configuración (`[Setup]`: idioma, tablas de códigos, plantillas, `startup=`). Variables de ruta `%appres%`, `%appdata%`, `%appdir%` |
| `locals\*.lng` (19 idiomas) | Traducciones `ID=texto` que sustituyen la tabla interna; cabecera `098`–`104` (modo debug, versión mín./máx., código de idioma, charset) |
| `locals\multi.tbl` | Tablas de página de códigos (Unicode↔char, transliteración, mayúsculas) |
| `template.txt` | Plantillas del editor de disco |
| `listtmpl.htm` | Plantilla de lista HTML de ficheros |
| `deviohsc.txt` | Documentación + ejemplo del script de manejo de E/S |
| `ioparams.ini`, `devcopy.ini`, `raidinfo.ini`, `mftruns.ini`, `rawsign.ini` | Persistencia de parámetros de E/S, copia, RAID, runs MFT y firmas |
| `*.log` (`fullscan.log`, `ntfsscan.log`, `fatscan.log`, `raidscan.log`, `scanauto.log`) | Logs de escaneo en formato INI (`[ntfsscan3.1]`, `scan_range=`, `volume%l=`…) |
| `dump*.dat`, `dumpio.bin` | Volcados de cambios/rollback y de E/S |

## Script de manejo de E/S (`deviohandler`)

Documentado en `deviohsc.txt`; palabras clave confirmadas en el binario (0x4c3e80).

- **Variables** (`%NOMBRE%`, con formato opcional `%LBA:10x%`): `CONFIRM`, `DISKNUM`, `TRYNUM`, `LBA`,
  `SECNUM`, `ERROR`, `ATASTATUS`, `ATAERROR`, `LINE`, `SERVICE`, `LASTRES`, `LASTERR`.
- **Comandos**: `SHOWDLG`, `WAIT`, `DELAY N`, `EXECCMD`/`EXECCMDQ`, `EXEC`/`EXECQ "fichero" args`,
  `MSDOS`, `RETURN`, `RETRETRY`, `RETIGNORE`, `RETABORT`, `ADDLOG`, `CANCELIO`, `OVLRESLT`,
  `RESETHANDLE`, `ATARESET`, `ATARESETDET`, `RESETDEVLIST`, `LOADNUM`, `ERROREX`.
- Uso: decidir por script qué hacer ante cada error de lectura (reintentar, ignorar, abortar,
  reiniciar el dispositivo, ejecutar un programa externo, p. ej. para apagar/encender un disco).

## Motor de plantillas del editor (`template.txt`)

Palabras clave confirmadas en el binario (0x73030a): `big_endian`, `CALCSIZESTART/END`,
`LOADSTART/END`, `FLUSHSTART/END`, `LABEL`, `IF/ELSEIF/ELSE/ENDIF`, `WHILE/ENDWHILE`, `BREAK`,
`CONTINUE`, `DEFINE/ENDDEFINE` (macros con parámetros `%1%`), `TOGGLE:`; variables `$x`, `$i`;
funciones de navegación `$OPENDEVOFS`, `$OPENVOLSEC`, `$OPENCLUSTER`, `$OPENFILENUM`, `$GOTOREC`,
`$GOTOOFS`, `$RECSIZE`, `$NEXTOFS`, `$OFFSET`, `$RECOFFSET`, `$VOLDATAOFS`; tipos de fecha
`FILETIME`, `UNIXDATE`, `DOSDATE`.

Ejemplo real del fichero: bucles `WHILE $i<5 … ENDWHILE` que emiten campos `{4},x:$x,%u` (tamaño 4,
columna `$x`, formato entero sin signo) y `EOL`. Es un lenguaje declarativo-imperativo para
describir estructuras binarias y navegar entre ellas desde el editor hex.

## Formato de cadenas propio

El núcleo usa especificadores no estándar en sus formatos: `%l` (32 bits), `%I` (64 bits),
`%~I` (con separador de miles), `%IV` (tamaño legible), `%W` (cadena ancha), `%"127s` (cadena
entrecomillada con límite), `%*csx` (bytes en hex), `%016hb` (binario). Útil como referencia si se
quiere leer o generar logs compatibles.
