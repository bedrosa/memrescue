# 02 · Dependencias y tecnologías

## Imports estáticos (tabla de importación)

Fuente: `search_procedures` (evidencia `ev_4e3537859c354fc78e5e60e338c3a966e9eca5551fc5d0aed371e317a019aa73`).
Ghidra reporta 386 funciones externas y 402 thunks. El listado completo por DLL está en
[datos/imports_por_dll.txt](datos/imports_por_dll.txt).

| DLL | Nº | APIs significativas | Uso deducido |
|---|---|---|---|
| KERNEL32 | 157 | `CreateFileW/A`, `ReadFile`, `WriteFile`, `SetFilePointer`, `SetEndOfFile`, `DeviceIoControl`, `GetOverlappedResult`, `GetLogicalDrives`, `GetDriveTypeW`, `GetVolumeInformationW`, `GetDiskFreeSpaceW`, `CreateThread`, `CreateProcessW`, `VirtualAlloc`, `GetNativeSystemInfo` | E/S directa a discos/volúmenes, hilos de escaneo, ejecución de comandos externos (script de E/S) |
| USER32 | 143 | `CreateWindowExW`, `RegisterClassExW`, `CreateDialogIndirectParamW`, `LoadMenuIndirectW`, `DefFrameProcW`, `DefMDIChildProcW`, `SetWindowsHookExW`, portapapeles | UI Win32 propia (MDI, diálogos creados desde plantillas en memoria) |
| GDI32 | 34 | `ExtTextOutW`, `CreateDIBSection`, `BitBlt`, `SetDIBitsToDevice` | Dibujo de editor hex, mapa de clusters, vista previa |
| COMCTL32 | 14 | `ImageList_*`, `CreateStatusWindowW` | Iconos de árbol/lista, barra de estado |
| ADVAPI32 | 12 | `AllocateAndInitializeSid`, `RegOpenKeyExW`, `GetUserNameW` | Comprobación de administrador, configuración de proxy, informes |
| OLEAUT32 | 11 | `SysAllocStringLen`, `Variant*`, `SafeArray*` | RTL de Delphi (WideString/Variant) |
| SHELL32 | 8 | `ShellExecuteExW`, `DragAcceptFiles`, `SHCreateItemFromParsingName` (delay) | Abrir ayuda/web, arrastrar imágenes, diálogos modernos |
| COMDLG32 | 3 | `GetOpenFileNameW`, `GetSaveFileNameW` | Diálogos abrir/guardar |
| OLE32 | 2 | `CoCreateInstance`, `CoTaskMemFree` | Diálogo de carpetas (IFileDialog, inferido) |
| MSIMG32 | 1 | `AlphaBlend` (delay) | Iconos con alfa |
| MSVCRT | 1 | `memcpy` | Requerido por el zlib enlazado (ver abajo) |

## Imports dinámicos (`LoadLibrary`/`GetProcAddress`)

**OBSERVADO** como pares nombre-DLL/nombre-función en cadenas (no aparecen en la tabla de imports):

| DLL | Funciones | Dirección cadena | Propósito |
|---|---|---|---|
| setupapi.dll | `SetupDiGetClassDevsW`, `SetupDiEnumDeviceInterfaces`, `SetupDiGetDeviceInterfaceDetailW`, `SetupDiGetDeviceRegistryPropertyW`, `CM_Get_Parent`, `CM_Get_Device_IDW` | 0x4d22d0–0x4d2480 | Enumeración de discos y su bus (USBSTOR, NVMe, iSCSI, Spaces…) |
| kernel32.dll | `FindFirstVolumeW`, `FindNextVolumeW`, `GetVolumePathNamesForVolumeNameW`, `QueryDosDeviceW` | 0x4cf5cc–0x4cf828 | Enumeración de volúmenes y letras |
| advapi32.dll | `CheckTokenMembership` | 0x4b9d49 | ¿Es administrador? (`runas` en 0x4b9e1c para elevar) |
| advapi32.dll | `OpenEncryptedFileRawW`, `ReadEncryptedFileRaw`, `WriteEncryptedFileRaw`, `CloseEncryptedFileRaw` | 0x83a394 | Recuperación/restauración de ficheros EFS de NTFS en formato de copia de seguridad |
| kernel32.dll | `SetFileInformationByHandle` | 0x83a36c | Restaurar atributos/fechas al recuperar |
| hhctrl.ocx | `HtmlHelpW` | 0x4b9f08 | Ayuda CHM |
| gdiplus.dll | `GdiplusStartup`, `GdipLoadImageFromStream`, `GdipDrawImageRectRectI`… | 0x772e81–0x773030 | Vista previa de imágenes |
| d2d1.dll | `D2D1CreateFactory` | 0x772cc8 | Vista previa / renderizado (opcional) |
| ws2_32.dll / wsock32.dll | `WSAStartup`, `getaddrinfo`, `socket`, `connect` | 0x845a3b–0x845fcb | Activación en línea (HTTP/1.0 POST manual) |
| wininet.dll | `InternetOpenW`, `HttpOpenRequestW`, `HttpSendRequestW`… | 0x846b50–0x846cb8 | Activación en línea (alternativa) |
| winhttp.dll | `WinHttpOpen`, `WinHttpSendRequest`… | 0x847373–0x84752c | Activación en línea (alternativa, HTTPS) |

## Bibliotecas enlazadas estáticamente

| Componente | Evidencia | Estado |
|---|---|---|
| **RTL de Delphi** (System, SysUtils, Classes, Generics, Rtti, SyncObjs, FastMM) | Nombres de unidad en RTTI, recursos `DVCLAL`/`PACKAGEINFO` | OBSERVADO |
| **zlib 1.2.7** | Cadena `"1.2.7"` (0x5d4080) usada por `FUN_005dbba0`, que llama a `inflateInit_(strm, "1.2.7", 0x58)`; 0x58 = `sizeof(z_stream)` en Win64 (LLP64). Luego `inflate(strm, Z_FINISH=4)`, comprueba `Z_STREAM_END=1` y `inflateEnd`; devuelve `total_out` (offset 0x1C). Evidencia `ev_fa2b43019b1c718e41a4057442bee94df75162c3a1baa7f72bcf550ec5d93b5e` | OBSERVADO (decompilado) |
| Origen de zlib | Delphi incluye zlib como `.obj` en `System.ZLib`, que importa `memcpy` de `msvcrt.dll`; encaja con el único import de MSVCRT | INFERIDO |
| **MD5** | `FUN_00458720`: transformación de bloque con las 64 constantes de RFC 1321 (0xd76aa478…0xeb86d391) y rotaciones 7/12/17/22, 5/9/14/20, 4/11/16/23, 6/10/15/21. Evidencia `ev_2c82be153aac7306ed218e341e0e99b196276ebc06083ca839ee2e8d65e8ffc6` | OBSERVADO |
| **CRC-32** (polinomio reflejado 0xEDB88320) | Tabla en `.data` 0x8a7e44 que empieza por `96 30 07 77` (= 0x77073096, entrada [1] de la tabla estándar); `FUN_0045a3d0` → `FUN_0045a370(0, buf, len)` usada para validar cabeceras GPT | OBSERVADO (tabla + uso); implementación interna no decompilada |
| **SHA-256** | Constantes K en `.data` 0x885744 (`98 2F 8A 42` = 0x428a2f98) | OBSERVADO (constantes); función no localizada |
| **SHA-1** | Cadenas de informe `SHA-1: %20cX` (0x835d6b) y opción `&SHA-1` | Solo cadenas |
| **LZNT1** (descompresión NTFS) | `FUN_00613930` (ver doc 05) | OBSERVADO |
| Generador QR | Alfabeto del modo alfanumérico QR `"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ $%*+-./:"` (0x82a7bc) + mensajes "Scan [QR code]" | INFERIDO (usado en activación manual) |

## Tecnologías y formatos manejados (según cadenas y código)

- **Acceso a dispositivos**: `\\.\PhysicalDrive%l`, `\\.\HCD%l` (cadenas 0x88fe09, 0x88fcd9), volúmenes, CD/DVD/BD
  (tabla de perfiles de medio 0x5f0718), pass-through SCSI/ATA, SMART (ATA) y registro de salud NVMe. Ver doc 06.
- **Contenedores de imagen**: RAW/IMG/BIN/IMA, VHD (`conectix`), VHDX (`vhdxfile`), VMDK (`# Disk DescriptorFile`),
  EnCase E01/L01/S01 (filtro de diálogo 0x4baade). Solo VHD/VHDX tienen constantes en zona de código de dispositivo;
  el soporte real de E01 no se ha verificado en código.
- **Esquemas de partición**: MBR/EBR, GPT (incluido híbrido "GPT+MBR"), Apple Partition Map.
- **Sistemas de archivos**: NTFS, FAT12/16/32, exFAT, ExtFS (ext2/3/4), HFS+/HFSX, APFS, ReFS (hasta v3.14 según changelog), Btrfs, ISO9660 (`CD001`).
- **RAID**: 0, 1, 4, 5 (4 variantes), 6 (4 variantes), rotación personalizada, JBOD, VHD diferencial.
- **Exportación**: listas de ficheros TXT/CSV/HTML (plantilla `listtmpl.htm`), log de copia compatible con *ddrescue mapfile*.
- **Red**: HTTP/1.0 manual sobre sockets, WinINet y WinHTTP, con soporte de proxy de IE
  (`Software\Microsoft\Windows\CurrentVersion\Internet Settings`, `ProxyServer`, `ProxyEnable`).
- **Localización**: ficheros `.lng` (19 idiomas en `locals\`), tablas de página de códigos `multi.tbl`.

## Dependencias de entorno

- Windows x64; requiere privilegios de administrador para acceso a discos (mensaje `1066=You need Administrator/Superuser privileges…`).
- Portable: no requiere instalación (mensaje `2380=DMDE software runs in portable mode without installation…`);
  configuración en `dmde.ini` junto al exe o en `%APPDATA%`/`%LOCALAPPDATA%\com.dmde.DMDE\`.
- Sin dependencias de runtime externas (no .NET, no VC++ redistributable).
