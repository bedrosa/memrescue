# Plan: imágenes NTFS con ficheros borrados (para M3–M4)

Estado: **planificado, no ejecutado**. Requiere decisiones del propietario (ver §4).

## 1. Objetivo

Disponer de imágenes NTFS pequeñas y reproducibles con una **verdad conocida** (ficheros vivos,
borrados, sobrescritos, fragmentados, comprimidos, dispersos, con ADS) para probar el lector NTFS (M3) y
el motor de recuperación (M4), y poder declarar paridad con pruebas (ADR-002).

## 2. Principios

- **Independencia**: las imágenes las crea un formateador NTFS ajeno a MemRescue y la verdad se verifica
  con una herramienta forense independiente. MemRescue nunca genera su propio NTFS de prueba (un parser
  y un generador del mismo autor comparten errores).
- **Reproducibilidad**: el NTFS real no es byte a byte determinista (marcas de tiempo, GUID, número de
  serie). Por eso: (a) las imágenes generadas se **archivan** comprimidas con su SHA-256 y son esas las que
  se usan en las pruebas; (b) el script de generación queda versionado para poder recrearlas; (c) los
  manifiestos describen la verdad **semántica** (rutas, tamaños, hashes de contenido, estado), no bytes.
- **Sin discos reales**: todo ocurre sobre ficheros de imagen o discos virtuales dentro de una VM/WSL.

## 3. Opciones de generación evaluadas

| Opción | Formateador | Verificación independiente | Requisitos | Valoración |
|---|---|---|---|---|
| **A. WSL2 (Ubuntu)** | `mkntfs` (ntfs-3g) sobre fichero *loop*; escritura con `ntfs-3g` | The Sleuth Kit (`fls`, `istat`, `icat`, `ffind`) | Instalar WSL2 + paquetes `ntfs-3g`, `sleuthkit` | **Recomendada**: todo scriptable, sin privilegios de Windows |
| B. VM Windows | `Format-Volume` sobre VHDX montado | TSK en la VM o en WSL | VM desechable con Hyper-V u otro hipervisor | Mejor fidelidad (NTFS de Windows real); más trabajo manual |
| C. Windows anfitrión | `New-VHD` + `Format-Volume` | - | **Administrador** y montaje de volúmenes en el anfitrión | Descartada para fases iniciales (seguridad) |

Recomendación: **A** para la mayor parte del corpus y **B** para un pequeño conjunto de referencia con
NTFS creado por Windows (comprimidos LZNT1 de Windows, EFS), cuando se autorice una VM.

## 4. Decisiones necesarias del propietario

1. Autorizar la instalación de **WSL2 + Ubuntu** (cambio de sistema; requiere administrador una vez).
2. Más adelante, autorizar una **VM Windows desechable** para el conjunto de referencia.

## 5. Escenarios del corpus NTFS (primera tanda)

| Imagen | Contenido | Verdad esperada |
|---|---|---|
| `ntfs_basic` | 64 MiB, árbol de 3 niveles, 50 ficheros de tamaños variados (residentes y no residentes) | Árbol completo, tamaños, SHA-256 de cada fichero |
| `ntfs_deleted` | `ntfs_basic` + borrado de 10 ficheros y 1 directorio sin sobrescribir | Registros MFT no en uso recuperables; contenido íntegro |
| `ntfs_overwritten` | Borrados + escritura posterior que reutiliza clusters de 3 de ellos | Esos 3 no recuperables íntegros (hash distinto), resto sí |
| `ntfs_fragmented` | Ficheros escritos intercalados para forzar fragmentación | Lista de runs esperada (vía `istat`), contenido íntegro |
| `ntfs_sparse_ads` | Ficheros dispersos y con flujos alternativos | Rangos dispersos; nombres y hashes de ADS |
| `ntfs_compressed` (VM) | Ficheros comprimidos por Windows (LZNT1) | Contenido descomprimido íntegro |
| `ntfs_damaged_*` | Copias con corrupción determinista: boot sector, MFT parcial, `$MFTMirr` | Comportamiento tolerante esperado por escenario |

## 6. Manifiesto semántico (borrador)

```toml
name = "ntfs_deleted"
image = "ntfs_deleted.img.zst"
image_sha256 = "…"
generator = "tests/generators/ntfs/make_ntfs_deleted.sh"
tools = ["mkntfs 2022.x", "ntfs-3g 2022.x", "sleuthkit 4.x"]

[[files]]
path = "/docs/informe.txt"
size = 12345
sha256 = "…"
state = "deleted"          # live | deleted | overwritten
mft_record = 71            # obtenido con `ffind` (TSK), no con MemRescue
recoverable = "full"       # full | partial | none
```

## 7. Pruebas que habilitará

- Listado del árbol vivo de MemRescue = listado de TSK (`fls -r`).
- Contenido recuperado (vivos y borrados) = SHA-256 del manifiesto.
- Ficheros sobrescritos: MemRescue debe marcarlos y no presentarlos como íntegros.
- Robustez: imágenes dañadas sin pánicos ni bucles (además del fuzzing).
