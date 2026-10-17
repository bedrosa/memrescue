# 06 · Capa de E/S de dispositivos

## Códigos de control usados (OBS)

Método: búsqueda de inmediatos en el rango 0x4cf000–0x4dd000 (evidencias
`ev_443e9171…`, `ev_2aeb62dd…`, `ev_1ae84ce7…`, `ev_4aaac493…`, `ev_9465ca0e…`) y lectura
de los bytes de la instrucción desde el fichero (solo lectura). Todos aparecen como
`mov edx, imm32` (`C7 C2 xx xx xx xx`) o `mov edi, imm32`, es decir, el segundo argumento
de `DeviceIoControl` en la convención Win64.

| Código | Nombre | Función(es) | Uso |
|---|---|---|---|
| 0x00090083 | `FSCTL_ALLOW_EXTENDED_DASD_IO` | 0x4d08d0 | Permitir leer todo el volumen, incluidas zonas fuera del FS |
| 0x00090018 | `FSCTL_LOCK_VOLUME` | 0x4d0cb0 | Bloquear volumen antes de escribir |
| 0x00090020 | `FSCTL_DISMOUNT_VOLUME` | 0x4d0cb0 | Forzar desmontaje (mensaje 1329) |
| 0x00070048 | `IOCTL_DISK_GET_PARTITION_INFO_EX` | 0x4d0f50 | Offset/tamaño de la partición del volumen |
| 0x00074004 | `IOCTL_DISK_GET_PARTITION_INFO` | 0x4d0f50 | Respaldo para sistemas antiguos |
| 0x0007405C | `IOCTL_DISK_GET_LENGTH_INFO` | 0x4d0f50, 0x4d9c60 | Tamaño en bytes |
| 0x00070000 | `IOCTL_DISK_GET_DRIVE_GEOMETRY` | 0x4d9c60 | Bytes/sector, CHS |
| 0x002D1400 | `IOCTL_STORAGE_QUERY_PROPERTY` | 0x4d7360, 0x4d84e0, 0x4d8b60 | Descriptor de dispositivo/adaptador (bus, modelo, serie); INF: también datos de protocolo NVMe |
| 0x0004D014 | `IOCTL_SCSI_PASS_THROUGH_DIRECT` | 0x4d6b00, 0x4d7730, 0x4d9870 | Comandos SCSI crudos (lectura con control de errores, INQUIRY, etc.) |
| 0x0004D030 | `IOCTL_ATA_PASS_THROUGH_DIRECT` | 0x4d7f20 | Comandos ATA crudos |
| 0x0004D008 | `IOCTL_SCSI_MINIPORT` | 0x4d82c0, 0x4d86f0 | SMART vía minipuerto / NVMe (INF) |
| 0x00074080 | `SMART_GET_VERSION` | 0x4d7e00 | Capacidades SMART |
| 0x0007C088 | `SMART_RCV_DRIVE_DATA` | 0x4d7b90 | Leer atributos/IDENTIFY |
| 0x0007C084 | `SMART_SEND_DRIVE_COMMAND` | 0x4d7b90 | Enviar comando SMART |

No se encontró `IOCTL_DISK_GET_DRIVE_GEOMETRY_EX` (0x700A0) en ese rango (`ev_6dfb3496…`).
Algunas coincidencias con comodines (`0x7ffc0`, `0x7c0c7`, `0x7c640`…) son falsos positivos (no son `mov edx`).

Los 14 llamadores de `DeviceIoControl` (thunk 0x415800) son: 0x4d08d0, 0x4d0cb0, 0x4d0f50, 0x4d1530,
0x4d16f0, 0x4d27c0, 0x4d2850, 0x4d28f0, 0x4d2a50, 0x4d35e0, 0x4d4eb0, 0x4d6a10, 0x4d9c60, 0x603cd0
(`ev_cf7cd7f4…`). Los códigos de 0x4d1530–0x4d4eb0 y 0x603cd0 no se determinaron (pueden pasarse
por registro/variable).

## Enumeración de dispositivos (OBS por cadenas, INF por flujo)

- Rutas: `\\.\PhysicalDrive%l` y `\\.\HCD%l` (cadenas en `.data`, sin referencia directa: se usan
  vía tabla de punteros o construcción indirecta).
- SetupAPI cargada dinámicamente (`setupapi.dll`, `SetupDiGetClassDevsW`, `SetupDiEnumDeviceInterfaces`,
  `CM_Get_Parent`, `CM_Get_Device_IDW`) para relacionar disco ↔ controlador ↔ bus. Nombres de bus
  reconocidos en cadenas: `USBSTOR`, `ATAPI`, `IEEE-1394`, `Fibre`, `iSCSI`, `Virtual`, `FBVirtual`,
  `Spaces`, `SCSIDISK`, `NvmeMini`.
- Volúmenes: `FindFirstVolumeW/FindNextVolumeW`, `GetVolumePathNamesForVolumeNameW`, `QueryDosDeviceW`.
- Opción del menú "Physical Devices" vs "Logical Disks" (mensajes 3731, 0x4a988f).

## Manejo de errores de lectura (OBS por claves de configuración y textos)

Claves de `ioparams.ini` / `[ioparams]` (0x5f1e6c, 0x5f2540): `skipioerrors`, `queryioerrors`,
`querydrivenotready`, `retries` (CRC), `seekretries`, `buffer` (sectores por E/S), `deviojump`
(salto tras error), `deviojumpreturn` (leer hacia atrás tras salto), `devioskipfiller` y
`deviobadfiller` (patrón hex para sectores omitidos/malos), `scsitimeout`, `handleresetonerror`,
`devlistresetonerror`, `autoresume`, `autoresumedelay`, `checkhandledelay`, `deviowait`,
`deviohandler` (script).

Códigos de error internos (0x5de195…): sin error, sector inválido, escritura no soportada,
error E/S Windows, abortado, solo lectura, doble acceso, cancelado por usuario, error interno,
omitir sectores, timeout, fallo de *sense*. Tabla de *sense keys* SCSI (0x5dde80): No Sense…Miscompare.

**INFERIDO** (no decompilado): el bucle de lectura reintenta según `retries`, salta `deviojump`
sectores tras un error, opcionalmente vuelve hacia atrás (`deviojumpreturn`), rellena los sectores no
leídos con el patrón configurado y registra rangos en el log de copia. Pendiente de confirmar en código.

## SMART / NVMe (OBS por cadenas)

- Tabla de ~80 nombres de atributos SMART ATA (0x5ee295–0x5ee9fa), con columnas Worst/Thresh/Raw.
- Campos del log de salud NVMe (0x5efa1d): CriticalWarning, Temperature, AvailableSpare, PercentageUsed,
  DataUnitRead/Written, PowerOnHours, UnsafeShutdowns, MediaErrors…

## Imágenes y medios

- VHD (`conectix` 0x4dc070) y VHDX (`vhdxfile`/`metadata` 0x4db41c) en el rango de E/S de dispositivos
  (funciones 0x4db850 / 0x4dc080 / 0x4dc660 con constantes cercanas; no decompiladas).
- Perfiles de medio óptico (0x5f0718): CD-ROM… BD-RE, HD DVD (respuesta a GET CONFIGURATION, INF).
- Modo "Raw Image", "I/O FILE", "I/O ATA", "Overlapped", "Use DMA" (0x5f501a–0x5f507e).
