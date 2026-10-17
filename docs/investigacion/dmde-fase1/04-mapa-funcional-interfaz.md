# 04 · Mapa funcional a partir de la interfaz

Fuente: tabla de 1 549 mensajes en inglés embebida en `.text` (0x4a9092–0x4b655b), formato
`ID=texto`, que actúa como idioma por defecto y que los ficheros `locals\*.lng` sobrescriben por ID.
Esta tabla describe **lo que la interfaz ofrece**; no prueba por sí sola que el código asociado
funcione. El nivel de verificación en código de cada área está en
[10-funcionalidades-verificables.md](10-funcionalidades-verificables.md).

> Nota de formato: los IDs se citan tal como aparecen (p. ej. `1183=&Full Scan...`); `~` es
> espacio no separable y `&` marca el acelerador de teclado.

## 1. Selección de origen

| IDs | Función |
|---|---|
| 1010–1022, 1111–1112, 3005 | Seleccionar disco físico / volumen lógico / imagen / log ("Select Disk / Task", "Open Image / Log") |
| 1113, 1038–1052, 3030–3101 | Construir RAID virtual (stripe, offset, orden de discos) y **cálculo automático de RAID** por escaneo |
| 1084–1108, 2520–2525, 2970 | Parámetros de E/S: reintentos CRC/seek, salto tras error, patrón para sectores malos/omitidos, timeouts SCSI, reset ATA, script de manejo de errores |
| 1115, 1326–1330 | Bloquear volúmenes para acceso directo |
| 3160–3161, 2940–2942 | SMART (umbrales/valores), modelo, firmware, número de serie |

## 2. Particiones

| IDs | Función |
|---|---|
| 1376–1459, 2607–2671 | Gestor de particiones: mostrar encontradas/eliminadas, insertar (undelete), borrar, crear partición RAW, restaurar boot sector desde copia, activar/desactivar firma MBR/GPT, marcar activa, deshacer/rehacer, guardar/cargar particionado, aplicar |
| 1351, 1413–1416 | Búsqueda de particiones perdidas en todo el disco |
| 1419, 2634–2643 | Reglas MBR (2 TiB, 4 primarias) y GPT |
| 2669, 2646–2647 | Escribir boot sectors / código de arranque MBR |

## 3. Apertura de volúmenes y navegación

| IDs | Función |
|---|---|
| 1529–1530, 1591–1631 | Abrir volumen, verificar parámetros de FS, uso de tablas FAT, MFT/MFTMirr |
| 1121–1132, 1670–1684 | Árbol de directorios, panel de ficheros, panel de búsqueda; búsqueda por nombre |
| 2086–2180 | FS virtual: "All Found / Virtual FS", más/menos resultados, incluir encontrados, procesar journal (Beta) |
| 1658–1669 | Mapa de clusters (qué fichero ocupa cada cluster) e informe |

## 4. Escaneo (reconstrucción)

| IDs | Función |
|---|---|
| 1183, 1967, 2065–2073, 1949–1958 | Escaneo completo / rápido / de volumen; continuar, pausar, rango |
| 1922–1934, 1964–1965, 2131–2143 | Búsqueda NTFS: fragmentos MFT, INDX, volúmenes candidatos |
| 1971, 1981, 1883, 1887 | Búsqueda FAT/exFAT: validez de tablas FAT |
| 1955 | ExtFS: lectura de tablas de inodos |
| 1907–1911, 2396–2399, 2586–2592, 3339–3342 | Guardar/cargar log de escaneo, autoguardado, compatibilidad entre versiones |
| 2480–2500 | Cálculo de volúmenes por estructuras FS o por firmas RAW; resultados principales/adicionales |

## 5. Búsqueda RAW por firmas

| IDs | Función |
|---|---|
| 3332–3337, 3630–3640 | Incluir RAW, filtrar RAW por datos del FS, alineación a cluster, excluir clusters usados |
| 3350–3397 | Editor de tipos de fichero: título, extensión, firma (hex con `\x00`, `?`), rango de offset, límites de tamaño, firma EOF, opciones (fiable, datos densos, contenedor, texto), condiciones `[OR]/[AND]/[AND NOT]` |
| 3600–3619 | Autodetección de firma a partir de ficheros de muestra |
| 3400–3422 | Estados de resultados: normal, borrado, encontrado, por firma (+/-EOF, truncado) |

## 6. Recuperación y exportación

| IDs | Función |
|---|---|
| 1686–1760, 2890–2932, 2960–2969 | Recuperar ficheros/directorios: categorías, filtro de nombres, destino en otro disco, renombrado automático, dividir ficheros, restaurar permisos/propietario, flujos alternativos NTFS |
| 2971–2996 | Manejador de eventos de recuperación (Ignorar/Omitir/Renombrar/Fusionar/Dividir/Mover a `$Bad`) |
| 1768–1775, 3570–3590 | Informe de recuperación (posiciones de error E/S, CRC32), lista de ficheros, lista de sectores/clusters, lista HTML |
| 3508–3515, 3560–3568 | EFS: recuperar ficheros cifrados NTFS en formato de copia de seguridad y restaurarlos |
| 2198–2200, 2215, 2891–2893 | **Límites de edición**: sin activación, hasta 4 000 ficheros y solo desde el panel activo |

## 7. Editor de disco (hex)

| IDs | Función |
|---|---|
| 1133–1173 | Modos de vista: hex/texto, dir FAT, tabla FAT, MBR/GPT, boot sector, dir NTFS, registro MFT; ir a cluster/sector/registro; plantillas personalizadas |
| 1147–1154, 1192–1193, 1460–1517 | Rellenar bloque/sectores con patrón, copiar/pegar, pegar fichero, **copiar sectores** (dispositivo/fichero → dispositivo/fichero) con log, hilos dobles, copia inversa, exportar a mapfile de ddrescue |
| 1538–1542, 1181–1182, 2550 | Buscar cadena (Unicode/sensible a mayúsculas), buscar sector especial |
| 1209–1215, 1548–1568, 3240–3241 | Escritura diferida: deshacer/rehacer, volcado y carga de rollback, aplicar cambios, modo global de solo lectura |
| 1194–1198, 1780–1823 | Herramientas NTFS en modo escritura: copiar fichero dentro, crear directorio, borrar fichero, reparar registros INDX |
| 3280–3282, 1396, 1409 | Crear imagen/clon, imagen dividida |

## 8. Aplicación

| IDs | Función |
|---|---|
| 1199, 994–997 | Idioma; guardado de ajustes |
| 1201–1207, 2201–2380 | Acerca de, licencia (EULA), activación (en línea, manual con QR, portátil ligada a un disco), clave de licencia, contraseña de arranque |
| 1204 | Modo oscuro (no oficial) |
| 3700–3838 | Sistema de pistas emergentes para usuarios noveles |
