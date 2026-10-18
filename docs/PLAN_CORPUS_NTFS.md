# Plan: imágenes NTFS con ficheros borrados (para M3–M4)

Estado: **primera tanda ejecutada** (issue #19, ADR-024). Las imágenes se generan en un ejecutor
desechable de GitHub Actions (`.github/workflows/ntfs-corpus.yml`), los manifiestos semánticos están
versionados en `tests/corpus/ntfs/manifests/` y `tools/fetch_ntfs_corpus.py` descarga y verifica las
imágenes. Pendiente: el conjunto de referencia creado por Windows (comprimidos LZNT1, §5).

## 1. Objetivo

Disponer de imágenes NTFS pequeñas y reproducibles con una **verdad conocida** (ficheros vivos,
borrados, sobrescritos, fragmentados, comprimidos, dispersos, con ADS) para probar el lector NTFS (M3) y
el motor de recuperación (M4), y poder declarar paridad con pruebas (ADR-002).

## 2. Principios

- **Independencia**: las imágenes las crea un formateador NTFS ajeno a MemRescue y la verdad se verifica
  con una herramienta forense independiente. MemRescue nunca genera su propio NTFS de prueba (un parser
  y un generador del mismo autor comparten errores).
- **Reproducibilidad**: se busca la regeneración byte a byte (§4.2); como red de seguridad, (a) las
  imágenes publicadas se identifican por su SHA-256, que está en el manifiesto versionado; (b) el script
  de generación está versionado; (c) los manifiestos describen la verdad **semántica** (rutas, tamaños,
  hashes de contenido, estado, runs), no bytes.
- **Sin discos reales y sin tocar el equipo del desarrollador**: todo ocurre sobre ficheros de imagen en
  un ejecutor Linux desechable. En local solo se descargan y validan ficheros.

## 3. Opciones de generación evaluadas

| Opción | Formateador | Verificación independiente | Requisitos | Valoración |
|---|---|---|---|---|
| A. WSL2 (Ubuntu) | `mkntfs` (ntfs-3g) sobre fichero; escritura con `ntfs-3g` | The Sleuth Kit (`fls`, `istat`, `icat`, `ffind`) | Instalar WSL2 + paquetes (administrador una vez) | Descartada: cambia el sistema del desarrollador |
| **A'. Ejecutor Ubuntu de GitHub Actions** | Igual que A | Igual que A | Ninguno en local; el ejecutor es desechable | **Elegida** (§4) |
| B. VM Windows | `Format-Volume` sobre VHDX montado | TSK en la VM o en el ejecutor | VM desechable con Hyper-V u otro hipervisor | Pendiente para el conjunto de referencia (LZNT1, EFS) |
| C. Windows anfitrión | `New-VHD` + `Format-Volume` | - | **Administrador** y montaje de volúmenes en el anfitrión | Descartada (seguridad) |

## 4. Decisión del propietario (2026-10-18, ADR-024)

1. El corpus NTFS se genera en un **ejecutor Ubuntu desechable de GitHub Actions**: `mkntfs` y
   `ntfs-3g` (paquetes de Ubuntu, versiones registradas) crean y pueblan las imágenes y **The Sleuth Kit**
   (`fls`, `istat`, `icat`, `ffind`, `ifind`, `mmls`, `fsstat`) es el oráculo independiente que rellena
   los manifiestos semánticos.
2. En el equipo del desarrollador **no se instala nada** y nada requiere administrador. Las imágenes se
   publican como **artefacto del workflow** con su SHA-256; un script local las descarga y las verifica.
3. La compresión LZNT1 creada por Windows queda **fuera de alcance por ahora**: necesita una VM Windows
   desechable que se autorizará más adelante (opción B). ntfs-3g puede comprimir, pero no sustituye a la
   referencia de Windows.

### 4.1 Flujo

```
ntfs-corpus.yml (ubuntu-24.04; workflow_dispatch o push a feat/** y fix/** que toque el generador)
  generate:  apt-get install ntfs-3g libfaketime sleuthkit
             generate.sh out/a ; generate.sh out/b               (dos generaciones desde cero)
             build_manifests.py (TSK) sobre A y sobre B          (manifiestos idénticos: reproducibilidad semántica)
             SHA256SUMS de A y B                                 (iguales: reproducibilidad byte a byte)
             verify_tsk.py, validate_ntfs_images.py, test_ntfs_validator.py
             diff manifiestos versionados / generados            (lo versionado sigue siendo la verdad de TSK)
             artefacto ntfs-corpus: imágenes, SHA256SUMS, PROVENANCE.txt, manifiestos, intención (90 días)
  verify:    ejecutor nuevo solo con sleuthkit: descarga el artefacto, sha256sum -c, validador,
             pruebas negativas y verify_tsk.py contra los manifiestos versionados
local:       python -I -X utf8 tools/fetch_ntfs_corpus.py       (gh run download + SHA-256 de cada imagen)
             python -I -X utf8 tests/generators/validate_ntfs_images.py tests/corpus/ntfs/generated tests/corpus/ntfs/manifests
```

Ficheros: `tests/generators/ntfs/generate.sh` (orquesta mkntfs, ntfs-3g, sfdisk y la corrupción),
`ntfs_tree.py` (escenarios y contenido determinista, escribe la intención), `ntfs_damage.py`
(corrupción determinista), `build_manifests.py` (manifiestos desde TSK), `verify_tsk.py` (segunda
comprobación con TSK, sin importar nada del generador); `tests/generators/validate_ntfs_images.py` y
`test_ntfs_validator.py` (validador estructural en Python y sus pruebas negativas).

### 4.2 Reproducibilidad conseguida: **byte a byte**

Dos generaciones desde cero en el mismo ejecutor, y generaciones en ejecuciones distintas del workflow
(máquinas distintas), producen **imágenes idénticas** (mismo SHA-256). Medidas aplicadas:

- `mkntfs --zero-time` (`-T`): todas las marcas de tiempo del formateo valen 1970-01-01 y la semilla del
  generador aleatorio de mkntfs sale de esa hora, así que el **número de serie** del volumen también es
  fijo (`34f5ee1202469ff7` en todas las imágenes). mkntfs no crea GUID de volumen sin `--with-uuid`.
- `ntfs-3g` se ejecuta bajo **libfaketime** con la hora congelada en `2026-01-01 00:00:00 UTC`
  (`FAKETIME` absoluto, sin avance). El constructor de manifiestos comprueba con `istat` que las cuatro
  marcas de `$STANDARD_INFORMATION` de cada entrada de usuario (también de las borradas) valen eso.
- `ntfs-3g` monta el fichero de imagen directamente (sin dispositivo loop), en primer plano
  (`no_detach`) y con `noatime`; el script espera a que termine el proceso antes de seguir.
- Contenido determinista (SHAKE-256 de una semilla y de la ruta, o texto numerado) y operaciones en orden
  fijo; las escrituras de ntfs-3g por FUSE son síncronas y el demonio es de un solo hilo.
- MBR y GPT con identificadores fijos (`label-id`, `uuid`) en `sfdisk`.

Cada manifiesto registra `byte_reproducible` (comparación A/B de esa ejecución) e `image_sha256`, y el
workflow falla si la verdad generada no coincide con la versionada. Si una actualización de Ubuntu
(ntfs-3g, núcleo o FUSE) cambiara los bytes, el workflow lo detectaría: se revisa el cambio, se toman
los manifiestos nuevos del artefacto `ntfs-corpus-debug` y se versionan. La **reproducibilidad
semántica** (manifiestos de A y B idénticos) queda como criterio de respaldo; el artefacto publicado
sigue identificado por su SHA-256.

## 5. Escenarios del corpus NTFS (primera tanda)

Volúmenes de 32 MiB (la mitad de lo previsto, para que el artefacto sea pequeño; el árbol ocupa unos
4,3 MiB), sectores de 512 B, clusters de 4 KiB, registros MFT de 1 KiB y NTFS 3.1.

| Imagen | Tamaño | Contenido | Verdad (TSK) |
|---|---|---|---|
| `ntfs_basic` | 32 MiB | Volumen sin tabla de particiones (empieza en el byte 0): 10 directorios en 3 niveles y 52 ficheros de 0 B a 1,5 MiB (residentes y no residentes, nombres con acentos, espacios y uno largo) | Árbol completo, tamaños, SHA-256, registros MFT, runs |
| `ntfs_deleted` | 32 MiB | Árbol básico con 5 ficheros y 1 directorio con 5 ficheros borrados, sin escritura posterior | 11 entradas no en uso, todas `recoverable = "full"` |
| `ntfs_overwritten` | 32 MiB | Árbol básico; volumen lleno con `/relleno.bin`; 3 borrados cuyos clusters reutilizan 2 ficheros nuevos; 2 borrados al final que quedan intactos | 2 objetivos `none` y 1 `partial` (10 de 16 clusters), con el fichero que ocupa ahora los clusters; 2 `full` |
| `ntfs_fragmented` | 32 MiB | 4 ficheros escritos a trozos de 8 KiB intercalados y 2 contiguos | Runs exactos (9 a 16 fragmentos por fichero intercalado) |
| `ntfs_sparse_ads` | 32 MiB | Dispersos: hueco inicial, intermedio, final (con `init_size` menor), varios huecos y solo hueco; ADS residentes y no residentes en ficheros y en un directorio, y un fichero vacío con ADS | Rangos dispersos, `init_size`, nombres, tamaños y SHA-256 de cada ADS |
| `ntfs_damaged_boot` | 32 MiB | `ntfs_basic` con el arranque principal sobrescrito (la copia de la última LBA queda) | TSK no la abre; árbol = `ntfs_basic` (TSK con la copia restaurada en una copia temporal) |
| `ntfs_damaged_mft` | 32 MiB | `ntfs_basic` con 4 registros MFT de ficheros sobrescritos | TSK no lee esos 4 (`mft_damaged`); el resto = `ntfs_basic` |
| `ntfs_damaged_mftmirr` | 32 MiB | `ntfs_basic` con los 4 registros de `$MFTMirr` sobrescritos | Árbol = `ntfs_basic` |
| `ntfs_mbr` | 33 MiB | Disco MBR (firma `4d524e31`), partición 0x07 activa en la LBA 2048 con el árbol básico | `mmls` + árbol = `ntfs_basic`; sectores ocultos = 2048 |
| `ntfs_gpt` | 34 MiB | Disco GPT (GUID fijos), partición de datos básicos en la LBA 2048 con el árbol básico | `mmls` + árbol = `ntfs_basic` |
| `ntfs_compressed` (VM) | - | Ficheros comprimidos por Windows (LZNT1) | **Pendiente** (§4, punto 3) |

La corrupción de los `ntfs_damaged_*` es determinista: bytes de SHAKE-256 con semilla fija sobre
regiones calculadas a partir del sector de arranque (`ntfs_damage.py`).

Limitaciones conocidas: ntfs-3g no crea nombres DOS 8.3, no implementa `fallocate` (los huecos nacen
sin escribir, no perforados), no escribe `$UsnJrnl` ni `$LogFile` con transacciones, y todas las imágenes
comparten número de serie. Son NTFS creados por ntfs-3g, no por Windows: el conjunto de referencia de
Windows (§3, opción B) cubrirá esas diferencias.

## 6. Manifiesto semántico

Esquema completo en `docs/PRUEBAS.md` §1.2. Resumen:

```toml
schema_version = 1
name = "ntfs_overwritten"
file = "ntfs_overwritten.img"
image_size = 33554432
image_sha256 = "..."              # el del artefacto publicado (y de cualquier regeneración)
byte_reproducible = true
tools = ["libfaketime 0.9.10-2.1", "ntfs-3g 1:2022.10.3-1.2ubuntu3.2", "sleuthkit 4.12.1+dfsg-1.1ubuntu2", "util-linux ..."]

[volume]                          # fsstat + istat de $MFT
offset = 0
serial = "34f5ee1202469ff7"
mft_lcn = 4
mft_runs = [[0, 4, 34]]           # [vcn, lcn, clusters]

[[files]]
path = "/sobrescritura/objetivo_3.bin"
type = "file"
mft_record = 129                  # istat (TSK), comprobado contra el inodo que dio ntfs-3g
sequence = 2
state = "overwritten"             # live | deleted | overwritten
size = 65536
sha256 = "..."                    # contenido original
resident = false
runs = [[0, 5389, 16]]
recoverable = "partial"           # full | partial | none
recovered_sha256 = "..."          # lo que devuelve icat hoy
clusters = 16
overwritten_clusters = 10
reason = "10 de 16 clusters con otro contenido, asignados ahora a /sobrescritura/nuevo_2.bin"
```

## 7. Pruebas que habilitará

- Listado del árbol vivo de MemRescue = listado de TSK (`fls -r`) = `[[files]]` con `state = "live"`.
- Contenido recuperado (vivos y borrados) = SHA-256 del manifiesto; runs y dispersos = los del manifiesto.
- Ficheros sobrescritos: MemRescue debe marcarlos y no presentarlos como íntegros (`recoverable`).
- Robustez: imágenes dañadas sin pánicos ni bucles (además del fuzzing), con el comportamiento de
  `[[damage]].expected`.
- Los analizadores de M2 (`mr-part`, búsqueda de perdidas) localizan el volumen en `ntfs_mbr` y `ntfs_gpt`.
