#!/usr/bin/env python3
"""Corrupción determinista de una copia de `ntfs_basic` (escenarios `ntfs_damaged_*`).

Uso:
    python3 -I ntfs_damage.py <tipo> <imagen> <intencion_base.json> <intencion_salida.json>

Tipos:
  boot     sobrescribe el sector de arranque principal (LBA 0 del volumen); la copia del final queda.
  mft      sobrescribe 4 registros MFT de ficheros de usuario (elegidos por ruta; el número de registro
           sale del inodo que dio ntfs-3g y se comprueba la firma FILE y el número antes de dañar).
  mftmirr  sobrescribe los 4 registros de $MFTMirr; $MFT queda intacta.

Los bytes de la corrupción salen de SHAKE-256 con una semilla fija: la misma entrada da la misma
imagen dañada. La salida es la intención de la base con una sección `damage` añadida.
"""
from __future__ import annotations

import hashlib
import json
import struct
import sys

SEED = "memrescue-ntfs-corpus-v1|damage"
MFT_DAMAGED_PATHS = [
    "/datos/tablas/t1.csv",
    "/datos/tablas/t3.bin",
    "/fotos/2025/marzo.jpg",
    "/docs/proyectos/beta/c.bin",
]


def garbage(label: str, size: int) -> bytes:
    return hashlib.shake_256(f"{SEED}|{label}".encode("utf-8")).digest(size)


def boot_fields(sector: bytes) -> dict:
    if sector[3:11] != b"NTFS    " or sector[510:512] != b"\x55\xaa":
        raise SystemExit("el sector de arranque de la base no es NTFS")
    bps = struct.unpack_from("<H", sector, 0x0B)[0]
    spc = sector[0x0D]
    cluster = bps * spc
    raw = struct.unpack_from("<b", sector, 0x40)[0]
    record = raw * cluster if raw > 0 else 1 << -raw
    return {
        "bps": bps,
        "cluster": cluster,
        "record": record,
        "total_sectors": struct.unpack_from("<Q", sector, 0x28)[0],
        "mft_lcn": struct.unpack_from("<Q", sector, 0x30)[0],
        "mftmirr_lcn": struct.unpack_from("<Q", sector, 0x38)[0],
    }


def main() -> int:
    if len(sys.argv) != 5 or sys.argv[1] not in ("boot", "mft", "mftmirr"):
        print(__doc__, file=sys.stderr)
        return 2
    kind, image, base_intent, out = sys.argv[1:]
    with open(base_intent, encoding="utf-8") as f:
        intent = json.load(f)
    with open(image, "r+b") as img:
        b = boot_fields(img.read(512))
        regions = []
        if kind == "boot":
            regions.append({"offset": 0, "length": b["bps"], "what": "sector de arranque principal"})
        elif kind == "mftmirr":
            regions.append({"offset": b["mftmirr_lcn"] * b["cluster"], "length": 4 * b["record"],
                            "what": "registros 0 a 3 de $MFTMirr"})
        else:
            by_path = {e["path"]: e for e in intent["entries"]}
            for p in MFT_DAMAGED_PATHS:
                rec = by_path[p]["inode"]
                off = b["mft_lcn"] * b["cluster"] + rec * b["record"]
                img.seek(off)
                head = img.read(48)
                if head[:4] != b"FILE" or struct.unpack_from("<I", head, 0x2C)[0] != rec:
                    raise SystemExit(f"el registro {rec} ({p}) no está donde se esperaba (¿$MFT fragmentada?)")
                regions.append({"offset": off, "length": b["record"], "what": f"registro MFT {rec} ({p})",
                                "record": rec, "path": p})
        for r in regions:
            img.seek(r["offset"])
            img.write(garbage(f"{kind}|{r['offset']}", r["length"]))
    intent["scenario"] = f"damaged_{kind}"
    intent["damage"] = {"kind": kind, "regions": regions}
    with open(out, "w", encoding="utf-8", newline="\n") as f:
        json.dump(intent, f, ensure_ascii=False, indent=1, sort_keys=True)
        f.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
