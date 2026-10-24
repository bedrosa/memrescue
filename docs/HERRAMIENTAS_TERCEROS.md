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

1. **Claves**: `msys2.gpg` y `msys2-trusted` desde `github.com/msys2/MSYS2-keyring` (HTTPS), fijados al
   commit `5d249077a0cfeec9dbf51515e5e01da413d28859` (2026-08-14, último que modifica `msys2.gpg`) y con
   SHA-256 comprobado antes de usarlos: `msys2.gpg` =
   `1257d4ccc536c53333445a618039af4c3ea3f56b96601de9af89151ea529c19a`, `msys2-trusted` =
   `a8d39040a7b6cc14bf4394b5d9c838443925c32b00a7d34e8729c972392dca04`. Si no coinciden, el script se
   detiene. `msys2-trusted` contiene 5 claves maestras con confianza total.
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
- Excepción acotada (registro de VHDX, issue #10): `convert --image-opts driver=vhdx,file.filename=…,read-only=off`
  solo sobre una **copia desechable** creada por la prueba en un directorio temporal, para que qemu reproduzca el
  registro pendiente en esa copia y sirva de referencia. El fichero que lee MemRescue nunca se pasa a qemu en
  lectura y escritura.

## Corpus NTFS: mkntfs, ntfs-3g, libfaketime y The Sleuth Kit (autorizado por el propietario el 2026-10-18, ADR-024)

Se usan **solo en un ejecutor Ubuntu desechable de GitHub Actions** (workflow `NTFS corpus`,
`.github/workflows/ntfs-corpus.yml`). No se instalan ni se ejecutan en el equipo del desarrollador, no
forman parte de MemRescue y no se distribuyen con él; lo único que llega al equipo local son las imágenes
generadas, verificadas por SHA-256 contra los manifiestos versionados (`tools/fetch_ntfs_corpus.py`).

| Herramienta | Paquete de Ubuntu 24.04 (versión de la primera tanda) | Uso | Licencia |
|---|---|---|---|
| `mkntfs` | `ntfs-3g` 1:2022.10.3-1.2ubuntu3.2 | Formatear el fichero de imagen (`--zero-time`, geometría fija) | GPL-2.0-or-later |
| `ntfs-3g` (FUSE) | `ntfs-3g` 1:2022.10.3-1.2ubuntu3.2 (`libntfs-3g89t64`) | Montar la imagen y escribir, borrar, crear dispersos y ADS | GPL-2.0-or-later |
| libfaketime | `libfaketime` 0.9.10-2.1 | Congelar la hora que ve ntfs-3g (marcas de tiempo reproducibles) | GPL-2.0 |
| The Sleuth Kit | `sleuthkit` 4.12.1+dfsg-1.1ubuntu2 (`libtsk19t64`) | Oráculo independiente: `mmls`, `fsstat`, `fls`, `istat`, `icat`, `ffind`, `ifind` | IBM-PL-1.0, CPL-1.0, GPL-2.0-or-later y otras (ver `/usr/share/doc/sleuthkit/copyright`) |
| `sfdisk` | `util-linux` 2.39.3-9ubuntu6.6 (preinstalado) | Tablas MBR y GPT con identificadores fijos | GPL-2.0-or-later |

Normas:

- **Procedencia**: paquetes del archivo oficial de Ubuntu instalados con `apt-get` (firmas del archivo
  verificadas por apt) en una máquina que GitHub destruye al terminar. El ejecutor está fijado a
  `ubuntu-24.04` y las acciones de GitHub a un commit concreto (SHA completo).
- **Versiones registradas**: cada artefacto lleva `PROVENANCE.txt` (imagen del ejecutor, núcleo, versión
  de cada paquete y de cada herramienta, SHA-256 de cada imagen) y cada manifiesto lleva `tools` con las
  versiones de los paquetes. Una versión nueva que cambie los bytes de las imágenes hace fallar el
  workflow hasta que se revisen y se versionen los manifiestos nuevos.
- **Privilegios**: `sudo` solo en el ejecutor y solo para el montaje FUSE de ntfs-3g; no se usan
  dispositivos loop ni discos. El token del workflow es de solo lectura y no hay secretos.
- **Desechable**: nada persiste salvo el artefacto (90 días; el de depuración, 14). Las imágenes se pueden
  regenerar byte a byte relanzando el workflow.
- **Uso**: solo sobre las imágenes sintéticas del corpus; nunca sobre imágenes de usuarios ni dispositivos.
  Pendiente: un conjunto de referencia creado por Windows en una VM desechable (LZNT1), cuando se autorice.
