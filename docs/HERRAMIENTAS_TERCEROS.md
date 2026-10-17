# Herramientas de terceros usadas en desarrollo y pruebas

Estas herramientas **no** forman parte de MemRescue, no se distribuyen con él y no están en git
(`.tools/` está en `.gitignore`). Se usan solo para generar y verificar imágenes de prueba desechables.

## qemu-img 11.1.2 (autorizado por el propietario el 2026-10-17)

| Campo | Valor |
|---|---|
| Uso | Crear imágenes VHD/VHDX/VMDK de prueba a partir de RAW sintéticos y convertirlas de nuevo a RAW como **referencia independiente** |
| Ubicación | `.tools\qemu\bin\qemu-img.exe` (+ DLL de dependencias) |
| Origen | Paquete `mingw-w64-ucrt-x86_64-qemu-image-util` 11.1.2-1 del repositorio oficial **MSYS2 ucrt64** (`https://repo.msys2.org/mingw/ucrt64/`), vía recomendada por <https://www.qemu.org/download/> |
| SHA-256 `qemu-img.exe` | `b4ef313ad2bd5070e44a5cccb50d8c2529dd68303c44ef8926b4c5a38ca219ff` |
| SHA-256 paquete | `2b374c173d36d1071b0cf1be7204d9cf89051af9a492f408180bac642ef70f47` (coincide con el publicado en packages.msys2.org) |
| Dependencias | 48 paquetes adicionales (resueltos de forma transitiva); lista completa con versión, fichero y SHA-256 en `.tools\qemu\PROVENANCE.json` |
| Licencias | QEMU: GPL-2.0-or-later AND LGPL-2.1-or-later AND BSD-2-Clause AND BSD-3-Clause AND MIT. Textos de licencia de todos los paquetes en `.tools\qemu\licenses\` |
| Instalación | Ninguna: extracción de ficheros con Python. Sin administrador, sin modificar PATH ni registro, sin ejecutar instaladores |
| Reproducir | `python -I tools\fetch_qemu_img.py .tools\qemu` |

### Verificación de procedencia e integridad (`tools/fetch_qemu_img.py`)

1. **Claves**: `msys2.gpg` y `msys2-trusted` desde `github.com/msys2/MSYS2-keyring` (HTTPS); `msys2-trusted`
   contiene 5 claves maestras con confianza total. SHA-256 de `msys2.gpg`:
   `1257d4ccc536c53333445a618039af4c3ea3f56b96601de9af89151ea529c19a`.
2. **Base de datos del repositorio** (`ucrt64.db`) verificada con su firma GPG. La firmó la clave
   `5F944B027F7FE2091985AA2EFA11531AA0AA7F57` (Christoph Reiter, *MSYS2 development key*), que **no** es
   maestra pero está certificada con firmas válidas por **3 claves maestras distintas**
   (`755B8182ACD22879`, `790AE56A1D3CFDDC`, `F40D263ECA25678A`). Es el modelo de confianza de pacman/MSYS2.
   Regla aplicada: se acepta una clave firmante maestra o certificada por **≥ 2** maestras distintas, no
   revocada ni caducada (GnuPG con confianza total exige 1).
3. **Cada uno de los 49 paquetes**: SHA-256 igual al de la base de datos firmada **y** firma GPG propia válida
   de una clave aceptada por la regla anterior.
4. Solo después se extraen `qemu-img.exe`, las DLL y las licencias, y se ejecuta `qemu-img --version`.

### Normas de uso

- Solo sobre imágenes sintéticas creadas por los generadores de MemRescue en `tests/corpus/generated`.
- Nunca sobre imágenes de usuarios ni dispositivos.
- Sin opciones de reparación (`qemu-img check -r` prohibido); solo `create`, `convert`, `info`, `map`, `check` (sin `-r`).
