#!/usr/bin/env python3
"""Pruebas negativas del validador estructural del corpus NTFS (no es un validador que siempre aprueba).

Uso:
    python -I -X utf8 tests/generators/test_ntfs_validator.py <dir_imagenes> <dir_manifiestos>

Cada caso altera en memoria una imagen o su manifiesto y exige que validate_ntfs_images.py informe al
menos una discrepancia. El SHA-256 del manifiesto se recalcula sobre la imagen alterada para que la
detección no dependa del hash: tiene que venir de la comprobación estructural. Antes, cada imagen
usada se valida sin alterar y debe pasar. No usa el generador ni The Sleuth Kit.
"""
from __future__ import annotations

import copy
import hashlib
import sys
import tomllib
from pathlib import Path
from typing import Callable

sys.path.insert(0, str(Path(__file__).resolve().parent))
from validate_ntfs_images import validate_bytes  # noqa: E402


def record_offset(m: dict, n: int) -> int:
    v = m["volume"]
    pos = n * v["mft_record_size"]
    for vcn, lcn, count in v["mft_runs"]:
        if vcn * v["cluster_size"] <= pos < (vcn + count) * v["cluster_size"]:
            return v["offset"] + lcn * v["cluster_size"] + pos - vcn * v["cluster_size"]
    raise ValueError(n)


def first(m: dict, pred: Callable[[dict], bool]) -> dict:
    return next(f for f in m["files"] if pred(f))


def flip(img: bytearray, at: int) -> None:
    img[at] ^= 0xFF


def nonres_live(f: dict) -> bool:
    return f["type"] == "file" and f["state"] == "live" and bool(f.get("runs"))


def resident_live(f: dict) -> bool:
    return f["type"] == "file" and f["state"] == "live" and f.get("resident") and f["size"] > 0


def data_byte(m: dict) -> Callable[[bytearray], None]:
    f = first(m, nonres_live)
    v = m["volume"]
    return lambda img: flip(img, v["offset"] + f["runs"][0][1] * v["cluster_size"] + 7)


def record_byte(m: dict, pred: Callable[[dict], bool], rel: int, value: int | None = None) -> Callable[[bytearray], None]:
    f = first(m, pred)
    at = record_offset(m, f["mft_record"]) + rel

    def mut(img: bytearray) -> None:
        if value is None:
            flip(img, at)
        else:
            img[at] = value
    return mut


def set_field(pred: Callable[[dict], bool], key: str, fn: Callable) -> Callable[[dict], None]:
    def mut(m: dict) -> None:
        f = first(m, pred)
        f[key] = fn(f[key])
    return mut


def swap_records(m: dict) -> None:
    a = first(m, nonres_live)
    b = first(m, lambda f: nonres_live(f) and f is not a)
    a["mft_record"], b["mft_record"] = b["mft_record"], a["mft_record"]


def shift_run(m: dict) -> None:
    f = first(m, nonres_live)
    f["runs"] = [[f["runs"][0][0], f["runs"][0][1] + 1, f["runs"][0][2]]] + f["runs"][1:]


def drop_sparse(m: dict) -> None:
    f = first(m, lambda f: bool(f.get("sparse")))
    del f["sparse"]


def change_ads(m: dict) -> None:
    f = first(m, lambda f: bool(f.get("ads")))
    f["ads"][0]["sha256"] = "0" * 64


def undamage(m: dict) -> None:
    f = first(m, lambda f: f.get("mft_damaged"))
    del f["mft_damaged"]


def restore_boot(m: dict) -> Callable[[bytearray], None]:
    v = m["volume"]

    def mut(img: bytearray) -> None:
        b = v["backup_boot_offset"]
        img[v["offset"]:v["offset"] + 512] = img[b:b + 512]
    return mut


def boot_total(m: dict) -> Callable[[bytearray], None]:
    return lambda img: flip(img, m["volume"]["offset"] + 0x28)


def mirror_byte(m: dict) -> Callable[[bytearray], None]:
    v = m["volume"]
    return lambda img: flip(img, v["offset"] + v["mftmirr_lcn"] * v["cluster_size"] + 100)


def gpt_name(m: dict) -> Callable[[bytearray], None]:
    return lambda img: flip(img, 2 * 512 + 56)


# (manifiesto, descripción, mutación de imagen (recibe el manifiesto), mutación del manifiesto)
CASES = [
    ("ntfs_basic", "byte de datos de un fichero no residente", data_byte, None),
    ("ntfs_basic", "firma FILE de un registro", lambda m: record_byte(m, nonres_live, 0), None),
    ("ntfs_basic", "secuencia de actualización (fin del primer sector del registro)",
     lambda m: record_byte(m, resident_live, 510), None),
    ("ntfs_basic", "total de sectores del arranque principal", boot_total, None),
    ("ntfs_basic", "byte de $MFTMirr", mirror_byte, None),
    ("ntfs_basic", "SHA-256 de un fichero residente", None, set_field(resident_live, "sha256", lambda _: "0" * 64)),
    ("ntfs_basic", "run desplazado un cluster", None, shift_run),
    ("ntfs_basic", "registros MFT intercambiados", None, swap_records),
    ("ntfs_basic", "estado vivo pasado a borrado", None, set_field(nonres_live, "state", lambda _: "deleted")),
    ("ntfs_basic", "indicador residente invertido", None, set_field(resident_live, "resident", lambda x: not x)),
    ("ntfs_basic", "secuencia del registro", None, set_field(nonres_live, "sequence", lambda x: x + 1)),
    ("ntfs_basic", "número de serie", None, lambda m: m["volume"].update(serial="1234")),
    ("ntfs_deleted", "indicador en uso de un registro borrado",
     lambda m: record_byte(m, lambda f: f["state"] == "deleted" and f["type"] == "file", 0x16, 1), None),
    ("ntfs_sparse_ads", "rango disperso eliminado", None, drop_sparse),
    ("ntfs_sparse_ads", "SHA-256 de un flujo alternativo", None, change_ads),
    ("ntfs_overwritten", "SHA-256 recuperado de un sobrescrito", None,
     set_field(lambda f: f["state"] == "overwritten", "recovered_sha256", lambda _: "0" * 64)),
    ("ntfs_damaged_mft", "registro dañado sin marcar", None, undamage),
    ("ntfs_damaged_boot", "arranque principal restaurado", restore_boot, None),
    ("ntfs_gpt", "nombre de la entrada GPT (CRC-32)", gpt_name, None),
    ("ntfs_mbr", "firma de disco MBR", None, lambda m: m["partition"].update(disk_signature="00000000")),
]


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    images, manifests = Path(sys.argv[1]), Path(sys.argv[2])
    loaded: dict[str, tuple[bytes, dict]] = {}
    failed = 0
    for name in sorted({c[0] for c in CASES}):
        m = tomllib.loads((manifests / f"{name}.manifest.toml").read_text(encoding="utf-8"))
        img = (images / m["file"]).read_bytes()
        loaded[name] = (img, m)
        r = validate_bytes(img, m)
        if r.errors:
            print(f"ERROR {name} sin alterar no pasa: {r.errors[:3]}")
            failed += 1
    for name, desc, img_mut, man_mut in CASES:
        img, m = loaded[name]
        img2, m2 = bytearray(img), copy.deepcopy(m)
        if img_mut:
            img_mut(m)(img2)
        if man_mut:
            man_mut(m2)
        m2["image_sha256"] = hashlib.sha256(img2).hexdigest()
        r = validate_bytes(bytes(img2), m2)
        if r.errors:
            print(f"OK    {name}: {desc} -> {r.errors[0]}")
        else:
            print(f"ERROR {name}: {desc} no se detecta")
            failed += 1
    print(f"{len(CASES)} casos negativos, {failed} fallos")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
