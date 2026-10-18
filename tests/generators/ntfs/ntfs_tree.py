#!/usr/bin/env python3
"""Puebla un volumen NTFS montado con ntfs-3g según un escenario del corpus y escribe su «intención».

Uso (en el ejecutor desechable de GitHub Actions, como root porque el montaje FUSE es de root):
    python3 -I ntfs_tree.py <escenario> <punto_de_montaje> <intencion.json>

Escenarios: basic, deleted, overwritten, fragmented, sparse_ads.

El contenido de cada fichero es determinista: SHAKE-256 de una semilla y de la ruta (binarios) o
líneas de texto numeradas (textos). La intención (`*.intent.json`) describe lo que el generador hizo:
rutas, tamaños, SHA-256 del contenido escrito, número de inodo que dio ntfs-3g y estado esperado. No
es la verdad del manifiesto: `build_manifests.py` obtiene la verdad con The Sleuth Kit y solo usa la
intención para comprobar que coinciden.

Todas las operaciones siguen un orden fijo para que ntfs-3g asigne los mismos registros MFT y los
mismos clusters en cada ejecución.
"""
from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys

SEED = "memrescue-ntfs-corpus-v1"
CHUNK = 64 * 1024


# ----------------------------------------------------------------------------------- contenido


def text_bytes(label: str, size: int) -> bytes:
    """Texto UTF-8 con líneas numeradas, recortado a `size` bytes exactos."""
    out = bytearray()
    n = 1
    while len(out) < size:
        out += f"MemRescue corpus NTFS · {label} · línea {n:06d} · ñandú, cigüeña, árbol\n".encode("utf-8")
        n += 1
    return bytes(out[:size])


def binary_bytes(label: str, size: int) -> bytes:
    return hashlib.shake_256(f"{SEED}|{label}".encode("utf-8")).digest(size) if size else b""


def pattern_bytes(label: str, size: int) -> bytes:
    """Relleno comprimible: cada bloque de 4 KiB empieza con una cabecera de texto y sigue con ceros."""
    out = bytearray()
    block = 0
    while len(out) < size:
        head = f"MR-RELLENO|{label}|bloque {block:08d}|".encode("ascii")
        out += head + bytes(4096 - len(head))
        block += 1
    return bytes(out[:size])


def content(spec: dict) -> bytes:
    """Contenido lógico completo de un flujo según su especificación (también para dispersos)."""
    kind = spec["kind"]
    if kind == "text":
        return text_bytes(spec["label"], spec["size"])
    if kind == "binary":
        return binary_bytes(spec["label"], spec["size"])
    if kind == "pattern":
        return pattern_bytes(spec["label"], spec["size"])
    if kind == "segments":
        buf = bytearray(spec["size"])
        for seg in spec["segments"]:
            buf[seg["offset"]:seg["offset"] + seg["length"]] = binary_bytes(seg["label"], seg["length"])
        return bytes(buf)
    raise ValueError(f"tipo de contenido desconocido: {kind}")


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


# ------------------------------------------------------------------------------- árbol básico

# Árbol de 3 niveles con 52 ficheros. Tamaños elegidos para cubrir datos residentes (0 a ~600 B),
# el límite residente/no residente y ficheros no residentes de 1 cluster a 1,5 MiB. Nombres con
# acentos, eñes, espacios y un nombre largo para probar la conversión UTF-16.
BASIC_DIRS = [
    "/docs",
    "/docs/proyectos",
    "/docs/proyectos/alfa",
    "/docs/proyectos/beta",
    "/docs/cartas",
    "/fotos",
    "/fotos/2025",
    "/fotos/2025/verano",
    "/datos",
    "/datos/tablas",
]
BASIC_FILES = [
    ("/LEEME.txt", 1234, "text"),
    ("/vacío.dat", 0, "binary"),
    ("/un_byte.bin", 1, "binary"),
    ("/configuración.ini", 300, "text"),
    ("/Nombre largo con espacios y acentos para probar UTF-16 en NTFS (versión final).txt", 2500, "text"),
    ("/docs/informe_2026.txt", 3072, "text"),
    ("/docs/notas.md", 500, "text"),
    ("/docs/año_fiscal.csv", 12288, "text"),
    ("/docs/índice.txt", 650, "text"),
    ("/docs/borrador.bin", 4095, "binary"),
    ("/docs/firma.bin", 4097, "binary"),
    ("/docs/proyectos/plan.txt", 8000, "text"),
    ("/docs/proyectos/presupuesto.bin", 65536, "binary"),
    ("/docs/proyectos/riesgos.md", 120, "text"),
    ("/docs/proyectos/calendario.txt", 4096, "text"),
    ("/docs/proyectos/acta.txt", 750, "text"),
    ("/docs/proyectos/alfa/requisitos.txt", 20000, "text"),
    ("/docs/proyectos/alfa/diseño.bin", 100000, "binary"),
    ("/docs/proyectos/alfa/pruebas.txt", 37, "text"),
    ("/docs/proyectos/alfa/registro.bin", 300000, "binary"),
    ("/docs/proyectos/alfa/pequeño.bin", 1000, "binary"),
    ("/docs/proyectos/beta/a.bin", 10, "binary"),
    ("/docs/proyectos/beta/b.bin", 5000, "binary"),
    ("/docs/proyectos/beta/c.bin", 50000, "binary"),
    ("/docs/proyectos/beta/d.txt", 400, "text"),
    ("/docs/cartas/carta_01.txt", 1500, "text"),
    ("/docs/cartas/carta_02.txt", 600, "text"),
    ("/docs/cartas/carta_03.txt", 9000, "text"),
    ("/docs/cartas/sobre.bin", 32768, "binary"),
    ("/docs/cartas/sello.bin", 200, "binary"),
    ("/fotos/portada.jpg", 150000, "binary"),
    ("/fotos/miniatura.png", 3000, "binary"),
    ("/fotos/icono.bmp", 680, "binary"),
    ("/fotos/panorámica.raw", 1048576, "binary"),
    ("/fotos/2025/enero.jpg", 200000, "binary"),
    ("/fotos/2025/febrero.jpg", 180000, "binary"),
    ("/fotos/2025/marzo.jpg", 4096, "binary"),
    ("/fotos/2025/notas.txt", 256, "text"),
    ("/fotos/2025/álbum.bin", 70000, "binary"),
    ("/fotos/2025/verano/playa.jpg", 250000, "binary"),
    ("/fotos/2025/verano/montaña.jpg", 1500000, "binary"),
    ("/fotos/2025/verano/río.jpg", 12345, "binary"),
    ("/fotos/2025/verano/lista.txt", 80, "text"),
    ("/datos/medidas.csv", 40000, "text"),
    ("/datos/config.json", 700, "text"),
    ("/datos/vacío.txt", 0, "text"),
    ("/datos/sensor.bin", 8192, "binary"),
    ("/datos/historico.bin", 400000, "binary"),
    ("/datos/tablas/t1.csv", 2048, "text"),
    ("/datos/tablas/t2.csv", 6000, "text"),
    ("/datos/tablas/t3.bin", 16384, "binary"),
    ("/datos/tablas/t4.bin", 900, "binary"),
]


class Volume:
    """Operaciones sobre el volumen montado que registran la intención en orden."""

    def __init__(self, mount: str, scenario: str) -> None:
        self.mount = mount
        self.entries: dict[str, dict] = {}
        self.order: list[str] = []
        self.ops: list[str] = []
        self.scenario = scenario

    def host(self, path: str) -> str:
        return os.path.join(self.mount, path.lstrip("/"))

    def _register(self, path: str, entry: dict) -> None:
        if path not in self.entries:
            self.order.append(path)
        self.entries[path] = entry

    def _inode(self, path: str) -> int:
        return os.lstat(self.host(path)).st_ino

    def mkdir(self, path: str) -> None:
        os.mkdir(self.host(path))
        self.ops.append(f"mkdir {path}")
        self._register(path, {"path": path, "type": "dir", "expect": "live", "inode": self._inode(path), "ads": []})

    def write(self, path: str, spec: dict, expect: str = "live") -> None:
        data = content(spec)
        with open(self.host(path), "wb", buffering=0) as f:
            pos = 0
            while pos < len(data):
                pos += f.write(data[pos:pos + CHUNK])
        self.ops.append(f"write {path} ({len(data)} B)")
        self._register(path, self._file_entry(path, spec, data, expect))

    def _file_entry(self, path: str, spec: dict, data: bytes, expect: str) -> dict:
        return {
            "path": path, "type": "file", "expect": expect, "inode": self._inode(path),
            "size": len(data), "sha256": sha256(data), "content": spec, "ads": [],
        }

    def write_sparse(self, path: str, spec: dict, punch: list[tuple[int, int]] | None = None) -> None:
        """Escribe solo los segmentos (saltando huecos con seek). Con `punch`, primero escribe el
        fichero entero con un relleno, luego los segmentos y después perfora los tramos indicados."""
        assert spec["kind"] == "segments"
        with open(self.host(path), "wb", buffering=0) as f:
            if punch:
                f.write(binary_bytes(f"{path}#relleno", spec["size"]))
            for seg in spec["segments"]:
                f.seek(seg["offset"])
                f.write(binary_bytes(seg["label"], seg["length"]))
            f.truncate(spec["size"])
        for off, length in punch or []:
            subprocess.run(["fallocate", "--punch-hole", "--keep-size", "--offset", str(off), "--length",
                            str(length), self.host(path)], check=True)
        self.ops.append(f"sparse {path} ({spec['size']} B lógicos)")
        self._register(path, self._file_entry(path, spec, content(spec), "live"))

    def add_ads(self, path: str, name: str, spec: dict) -> None:
        data = content(spec)
        os.setxattr(self.host(path), f"user.{name}", data)
        self.ops.append(f"ads {path}:{name} ({len(data)} B)")
        self.entries[path]["ads"].append({"name": name, "size": len(data), "sha256": sha256(data), "content": spec})

    def delete(self, path: str, expect: str = "deleted") -> None:
        if self.entries[path]["type"] == "dir":
            children = [p for p in self.order if p.startswith(path + "/")]
            for child in sorted(children, key=lambda p: (-p.count("/"), self.order.index(p))):
                self._remove(child, expect)
        self._remove(path, expect)

    def _remove(self, path: str, expect: str) -> None:
        if self.entries[path]["type"] == "dir":
            os.rmdir(self.host(path))
        else:
            os.unlink(self.host(path))
        self.ops.append(f"delete {path}")
        self.entries[path]["expect"] = expect

    def basic_tree(self) -> None:
        for d in BASIC_DIRS:
            self.mkdir(d)
        for path, size, kind in BASIC_FILES:
            self.write(path, {"kind": kind, "label": path, "size": size})

    def dump(self, out: str, extra: dict) -> None:
        doc = {
            "scenario": self.scenario,
            "seed": SEED,
            "entries": [self.entries[p] for p in self.order],
            "operations": self.ops,
            **extra,
        }
        with open(out, "w", encoding="utf-8", newline="\n") as f:
            json.dump(doc, f, ensure_ascii=False, indent=1, sort_keys=True)
            f.write("\n")


# ------------------------------------------------------------------------------- escenarios


def scenario_basic(v: Volume) -> dict:
    v.basic_tree()
    return {}


DELETED_FILES = [
    "/docs/borrador.bin",                     # no residente, 2 clusters
    "/docs/notas.md",                         # residente
    "/docs/proyectos/alfa/registro.bin",      # no residente grande
    "/fotos/2025/verano/río.jpg",             # no residente
    "/datos/tablas/t4.bin",                   # límite residente / no residente
]
DELETED_DIR = "/docs/cartas"                  # 5 ficheros dentro


def scenario_deleted(v: Volume) -> dict:
    v.basic_tree()
    for p in DELETED_FILES:
        v.delete(p)
    v.delete(DELETED_DIR)
    return {}


def scenario_overwritten(v: Volume) -> dict:
    """Tres ficheros borrados cuyos clusters se reutilizan después y dos borrados que se conservan.

    1. Árbol básico y /sobrescritura con 3 objetivos (8, 12 y 16 clusters) y 2 supervivientes.
    2. Se crean vacíos los ficheros nuevos (sus registros MFT existen antes de llenar el volumen).
    3. /relleno.bin ocupa todo el espacio libre (statvfs), así que tras borrar los objetivos sus
       clusters son el único espacio libre.
    4. Se escriben 30 clusters en los ficheros nuevos: caen sobre los 36 de los objetivos.
    5. Al final se borran los supervivientes, cuyos clusters quedan libres e intactos.
    """
    v.basic_tree()
    v.mkdir("/sobrescritura")
    targets = [("/sobrescritura/objetivo_1.bin", 8 * 4096), ("/sobrescritura/objetivo_2.bin", 12 * 4096),
               ("/sobrescritura/objetivo_3.bin", 16 * 4096)]
    survivors = [("/sobrescritura/superviviente_1.txt", 300, "text"),
                 ("/sobrescritura/superviviente_2.bin", 40000, "binary")]
    for p, size in targets:
        v.write(p, {"kind": "binary", "label": p, "size": size})
    for p, size, kind in survivors:
        v.write(p, {"kind": kind, "label": p, "size": size})
    news = [("/sobrescritura/nuevo_1.bin", 16 * 4096), ("/sobrescritura/nuevo_2.bin", 14 * 4096)]
    for p, _ in news:
        v.write(p, {"kind": "binary", "label": p, "size": 0})
    st = os.statvfs(v.mount)
    free = st.f_bavail * st.f_frsize
    filler = "/relleno.bin"
    written = 0
    with open(v.host(filler), "wb", buffering=0) as f:
        data = pattern_bytes(filler, free)
        try:
            while written < len(data):
                written += f.write(data[written:written + CHUNK])
        except OSError:
            pass
    spec = {"kind": "pattern", "label": filler, "size": written}
    v.ops.append(f"fill {filler} ({written} B de {free} B libres)")
    v._register(filler, v._file_entry(filler, spec, content(spec), "live"))
    for p, _ in targets:
        v.delete(p, expect="overwrite_target")
    for p, size in news:
        v.write(p, {"kind": "binary", "label": p, "size": size})
    for p, _, _ in survivors:
        v.delete(p)
    st = os.statvfs(v.mount)
    return {"free_bytes_before_fill": free, "free_bytes_at_end": st.f_bavail * st.f_frsize}


def scenario_fragmented(v: Volume) -> dict:
    """Cuatro ficheros escritos a trozos de 8 KiB intercalados: ntfs-3g asigna cada trozo detrás del
    último ocupado, así que los cuatro quedan fragmentados. Más dos ficheros contiguos de control."""
    v.mkdir("/fragmentos")
    v.write("/fragmentos/contiguo_antes.bin", {"kind": "binary", "label": "/fragmentos/contiguo_antes.bin", "size": 50000})
    files = [("/fragmentos/intercalado_a.bin", 96 * 1024), ("/fragmentos/intercalado_b.bin", 128 * 1024),
             ("/fragmentos/intercalado_c.bin", 64 * 1024 + 1000), ("/fragmentos/intercalado_d.bin", 200 * 1024)]
    piece = 8 * 1024
    datas = {p: binary_bytes(p, size) for p, size in files}
    handles = {p: open(v.host(p), "wb", buffering=0) for p, _ in files}
    try:
        pos = 0
        while any(pos < len(d) for d in datas.values()):
            for p, _ in files:
                if pos < len(datas[p]):
                    handles[p].write(datas[p][pos:pos + piece])
            pos += piece
    finally:
        for h in handles.values():
            h.close()
    for p, size in files:
        spec = {"kind": "binary", "label": p, "size": size}
        v.ops.append(f"interleaved {p} ({size} B en trozos de {piece} B)")
        entry = v._file_entry(p, spec, datas[p], "live")
        entry["expect_fragmented"] = True
        v._register(p, entry)
    v.write("/fragmentos/contiguo_despues.bin", {"kind": "binary", "label": "/fragmentos/contiguo_despues.bin", "size": 70000})
    return {}


def seg(label: str, offset: int, length: int) -> dict:
    return {"label": f"{label}@{offset}", "offset": offset, "length": length}


def scenario_sparse_ads(v: Volume) -> dict:
    v.mkdir("/disperso")
    v.mkdir("/flujos")
    v.mkdir("/flujos/carpeta_con_flujo")
    k = 1024
    p = "/disperso/hueco_inicial.bin"
    v.write_sparse(p, {"kind": "segments", "size": 832 * k, "segments": [seg(p, 768 * k, 64 * k)]})
    p = "/disperso/hueco_medio.bin"
    v.write_sparse(p, {"kind": "segments", "size": 576 * k, "segments": [seg(p, 0, 64 * k), seg(p, 512 * k, 64 * k)]})
    p = "/disperso/hueco_final.bin"
    v.write_sparse(p, {"kind": "segments", "size": 1024 * k, "segments": [seg(p, 0, 64 * k)]})
    p = "/disperso/perforado.bin"
    v.write_sparse(p, {"kind": "segments", "size": 256 * k, "segments": [seg(p, 0, 64 * k), seg(p, 192 * k, 64 * k)]},
                   punch=[(64 * k, 128 * k)])
    p = "/disperso/solo_hueco.bin"
    v.write_sparse(p, {"kind": "segments", "size": 512 * k, "segments": []})
    p = "/flujos/con_flujos.txt"
    v.write(p, {"kind": "text", "label": p, "size": 2048})
    v.add_ads(p, "Zone.Identifier", {"kind": "text", "label": "Zone.Identifier", "size": 26})
    v.add_ads(p, "resumen", {"kind": "binary", "label": f"{p}:resumen", "size": 20000})
    p = "/flujos/solo_flujo.dat"
    v.write(p, {"kind": "binary", "label": p, "size": 0})
    v.add_ads(p, "datos", {"kind": "binary", "label": f"{p}:datos", "size": 5000})
    v.add_ads("/flujos/carpeta_con_flujo", "etiqueta", {"kind": "text", "label": "etiqueta de carpeta", "size": 100})
    p = "/flujos/disperso_con_flujo.bin"
    v.write_sparse(p, {"kind": "segments", "size": 384 * k, "segments": [seg(p, 320 * k, 64 * k)]})
    v.add_ads(p, "miniatura", {"kind": "binary", "label": f"{p}:miniatura", "size": 3000})
    return {}


SCENARIOS = {
    "basic": scenario_basic,
    "deleted": scenario_deleted,
    "overwritten": scenario_overwritten,
    "fragmented": scenario_fragmented,
    "sparse_ads": scenario_sparse_ads,
}


def main() -> int:
    if len(sys.argv) != 4 or sys.argv[1] not in SCENARIOS:
        print(__doc__, file=sys.stderr)
        return 2
    scenario, mount, out = sys.argv[1:]
    os.umask(0o022)
    v = Volume(mount, scenario)
    extra = SCENARIOS[scenario](v)
    os.sync()
    v.dump(out, extra)
    return 0


if __name__ == "__main__":
    sys.exit(main())
